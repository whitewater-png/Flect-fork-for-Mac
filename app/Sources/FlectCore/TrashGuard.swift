import Foundation

/// The app's own check of every path the engine asks it to move to the Trash (`needs: trash`). The engine already
/// decides what goes; this makes sure a wrong or stale answer cannot reach anything else:
/// - the path is absolute, without `.` / `..` / empty components, and lies strictly inside one of the run's selected
///   folders (each resolved with realpath; for the path only its folder part is resolved, the last component is not
///   followed, so a link is judged as itself); it is never one of the selected items, by text or by (device, inode),
///   so another spelling of a selected folder (case, normalisation, firmlink) is refused too. (Both deleting tasks take folders: Remove Duplicate lists files found
///   inside the selected folders and ignores selected files, so no trash path is ever a selected item.)
/// - it exists and is not a symbolic link;
/// - Remove Empty: a folder that, checked again right now, holds nothing but folders and OS litter (the engine's
///   list: `.DS_Store`, `._*`, `.localized`, `Icon\r`, `desktop.ini`, `Thumbs.db`, plus 0-byte files when that
///   setting is on); every other task: a regular file.
public enum TrashGuard {
    public enum Kind: Equatable {
        /// `folder.removeEmpty`: folders that must still be empty.
        case emptyFolders
        /// Everything else (Remove Duplicate): regular files.
        case files
    }

    public enum Refusal: String, Error, Equatable, CaseIterable {
        case invalidPath, outsideSelection, isSelected, missing, symlink, unexpectedType, notEmpty, unreadable
        /// The localized reason (`strings.ux.*.json`).
        public var key: String { "app.trashWhy.\(rawValue)" }
    }

    /// Entries looked at inside one folder before giving up (refused as `unreadable`).
    public static let maxEntries = 20_000
    public static let maxDepth = 64

    public static func kind(task: String, steps: [String]?) -> Kind {
        guard steps == nil else { return .files }
        let name = task.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false).first.map(String.init) ?? task
        return name == "folder.removeEmpty" ? .emptyFolders : .files
    }

    /// OS litter, as the engine's `folder::ignorable` with its default toggles.
    public static func isLitter(name: String, size: Int64, zeroByteIsLitter: Bool) -> Bool {
        if zeroByteIsLitter && size == 0 { return true }
        let lower = name.lowercased()
        return ["desktop.ini", ".ds_store", ".localized", "thumbs.db"].contains(lower) || name == "Icon\r" || name.hasPrefix("._")
    }

    /// nil when `path` may be moved to the Trash now, otherwise why not.
    public static func check(_ path: String, inputs: [String], kind: Kind, zeroByteIsLitter: Bool = false) -> Refusal? {
        guard let resolved = resolvedKeepingLast(path) else { return isWellFormed(path) ? .missing : .invalidPath }
        var st = stat()
        let statError: Int32? = lstat(path, &st) == 0 ? nil : errno
        let roots = inputs.compactMap(resolvedInput)
        // One of the selected items itself, in any spelling (case, Unicode normalisation, firmlink): same text, or
        // the same (device, inode) as the item.
        if roots.contains(where: { $0.path == resolved }) { return .isSelected }
        if statError == nil, roots.contains(where: { $0.id == FileID(st) }) { return .isSelected }
        guard roots.contains(where: { resolved.hasPrefix($0.path == "/" ? "/" : $0.path + "/") }) else { return .outsideSelection }
        if let e = statError { return e == ENOENT || e == ENOTDIR ? .missing : .unreadable }
        switch st.st_mode & S_IFMT {
        case S_IFLNK: return .symlink
        case S_IFDIR:
            guard kind == .emptyFolders else { return .unexpectedType }
            var budget = maxEntries
            return onlyLitter(path, depth: 0, budget: &budget, zeroByteIsLitter: zeroByteIsLitter)
        case S_IFREG:
            return kind == .files ? nil : .unexpectedType
        default:
            return .unexpectedType
        }
    }

    struct FileID: Equatable {
        let dev: dev_t, ino: ino_t
        init(_ st: stat) { dev = st.st_dev; ino = st.st_ino }
    }

    /// A selected item as the engine scanned it: a folder (not a link; trailing slashes ignored), resolved fully
    /// with realpath(3) (which also gives the case stored on disk), with its (device, inode). nil for anything else:
    /// the deleting tasks never scan below it.
    static func resolvedInput(_ input: String) -> (path: String, id: FileID)? {
        var p = input
        while p.count > 1, p.hasSuffix("/") { p.removeLast() }
        guard p.hasPrefix("/"), !p.contains("\0") else { return nil }
        var st = stat()
        guard lstat(p, &st) == 0, (st.st_mode & S_IFMT) == S_IFDIR, let r = realpath(p, nil) else { return nil }
        defer { free(r) }
        return (String(cString: r), FileID(st))
    }

    /// Absolute, no NUL, and no empty, `.` or `..` component (a trailing `/` is an empty component).
    static func isWellFormed(_ path: String) -> Bool {
        guard path.hasPrefix("/"), path.count > 1, !path.contains("\0") else { return false }
        return path.dropFirst().split(separator: "/", omittingEmptySubsequences: false).allSatisfy { !$0.isEmpty && $0 != "." && $0 != ".." }
    }

    /// The folder part resolved with realpath(3) (links followed, the case stored on disk), the last component kept as
    /// it is (not followed). nil for a path
    /// that is not well formed or whose folder cannot be resolved. `/` stays `/`.
    static func resolvedKeepingLast(_ path: String) -> String? {
        if path == "/" { return "/" }
        guard isWellFormed(path) else { return nil }
        let ns = path as NSString
        let parent = ns.deletingLastPathComponent, last = ns.lastPathComponent
        guard let p = realpath(parent, nil) else { return nil }
        defer { free(p) }
        let base = String(cString: p)
        return base == "/" ? "/" + last : base + "/" + last
    }

    /// Whether the folder holds only folders and litter, all the way down (links, other files, anything unreadable
    /// and anything beyond the limits make it non-empty).
    static func onlyLitter(_ dir: String, depth: Int, budget: inout Int, zeroByteIsLitter: Bool) -> Refusal? {
        guard depth <= maxDepth, let d = opendir(dir) else { return .unreadable }
        defer { closedir(d) }
        while true {
            errno = 0
            guard let e = readdir(d) else { return errno == 0 ? nil : .unreadable }
            let name = withUnsafePointer(to: e.pointee.d_name) { ptr in
                ptr.withMemoryRebound(to: CChar.self, capacity: Int(e.pointee.d_namlen) + 1) { String(cString: $0) }
            }
            if name == "." || name == ".." { continue }
            budget -= 1
            if budget < 0 { return .unreadable }
            let child = dir + "/" + name
            var st = stat()
            guard lstat(child, &st) == 0 else { return .unreadable }
            switch st.st_mode & S_IFMT {
            case S_IFDIR:
                if let r = onlyLitter(child, depth: depth + 1, budget: &budget, zeroByteIsLitter: zeroByteIsLitter) { return r }
            case S_IFREG:
                if !isLitter(name: name, size: Int64(st.st_size), zeroByteIsLitter: zeroByteIsLitter) { return .notEmpty }
            default:
                return .notEmpty // links, sockets, devices...
            }
        }
    }
}
