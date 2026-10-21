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
///   list under the same settings, see `LitterRules`); every other task: a regular file.
public enum TrashGuard {
    public enum Kind: Equatable {
        /// `folder.removeEmpty`: folders that must still be empty.
        case emptyFolders
        /// Everything else (Remove Duplicate): regular files.
        case files
    }

    public enum Refusal: String, Error, Equatable, CaseIterable {
        case invalidPath, outsideSelection, isSelected, missing, symlink, unexpectedType, notEmpty, unreadable
        /// The request came from a run that is not the current one.
        case staleRun
        /// The localized reason (`strings.ux.*.json`).
        public var key: String { "app.trashWhy.\(rawValue)" }
    }

    /// What one run's Trash requests may touch: that run's selected items and task kind, under its own id. Requests
    /// are checked against the scope of the run whose event stream produced them, and only while that run is the
    /// current one.
    public struct Scope: Equatable {
        public let run: UUID
        public let inputs: [String]
        public let kind: Kind
        public init(run: UUID = UUID(), inputs: [String], kind: Kind) { self.run = run; self.inputs = inputs; self.kind = kind }
    }

    /// `check` for a request of the run `scope` belongs to; `.staleRun` when that run is not `currentRun`.
    public static func check(_ path: String, scope: Scope, currentRun: UUID?, litter: LitterRules = LitterRules()) -> Refusal? {
        guard scope.run == currentRun else { return .staleRun }
        return check(path, inputs: scope.inputs, kind: scope.kind, litter: litter)
    }

    /// What a run's Trash requests came to.
    public struct Tally: Equatable {
        public var requested = 0, moved = 0
        public var notMoved: Int { requested - moved }
        public init(requested: Int = 0, moved: Int = 0) { self.requested = requested; self.moved = moved }
        public mutating func add(_ other: Tally) { requested += other.requested; moved += other.moved }
    }

    /// The run's closing line: the engine's own ("Moved 5 to the Trash...") only when everything it asked for was
    /// moved; otherwise how many of how many were moved and how many were left in place.
    public static func runSummary(_ engine: String?, tally: Tally, loc: Loc) -> String? {
        guard tally.notMoved > 0 else { return engine }
        return loc.f("app.trashPartial", [String(tally.moved), String(tally.requested), String(tally.notMoved)])
    }

    /// Entries looked at inside one folder before giving up (refused as `unreadable`).
    public static let maxEntries = 20_000
    public static let maxDepth = 64

    public static func kind(task: String, steps: [String]?) -> Kind {
        guard steps == nil else { return .files }
        let name = task.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false).first.map(String.init) ?? task
        return name == "folder.removeEmpty" ? .emptyFolders : .files
    }

    /// What counts as OS litter, from the same settings the engine reads (`Folder.Empty`): `IgnoreDesktopIni`
    /// (`desktop.ini` and the macOS litter `.DS_Store`, `.localized`, `Icon\r`, `._*`), `IgnoreThumbsDb`
    /// (`Thumbs.db`) and `IgnoreZeroByteFiles`. Defaults as the engine's.
    public struct LitterRules: Equatable {
        public var zeroByte = false, desktopIni = true, thumbsDb = true
        public init(zeroByte: Bool = false, desktopIni: Bool = true, thumbsDb: Bool = true) {
            self.zeroByte = zeroByte; self.desktopIni = desktopIni; self.thumbsDb = thumbsDb
        }
        public init(settings: SettingsStore) {
            self.init(zeroByte: settings.bool("Folder", "Empty", "IgnoreZeroByteFiles", default: false),
                      desktopIni: settings.bool("Folder", "Empty", "IgnoreDesktopIni", default: true),
                      thumbsDb: settings.bool("Folder", "Empty", "IgnoreThumbsDb", default: true))
        }
    }

    /// OS litter, as the engine's `folder::ignorable`.
    public static func isLitter(name: String, size: Int64, rules: LitterRules) -> Bool {
        if rules.zeroByte && size == 0 { return true }
        let lower = name.lowercased()
        if rules.desktopIni && (["desktop.ini", ".ds_store", ".localized"].contains(lower) || name == "Icon\r" || name.hasPrefix("._")) {
            return true
        }
        return rules.thumbsDb && lower == "thumbs.db"
    }

    /// nil when `path` may be moved to the Trash now, otherwise why not.
    public static func check(_ path: String, inputs: [String], kind: Kind, litter: LitterRules = LitterRules()) -> Refusal? {
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
            return onlyLitter(path, depth: 0, budget: &budget, rules: litter)
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
    static func onlyLitter(_ dir: String, depth: Int, budget: inout Int, rules: LitterRules) -> Refusal? {
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
                if let r = onlyLitter(child, depth: depth + 1, budget: &budget, rules: rules) { return r }
            case S_IFREG:
                if !isLitter(name: name, size: Int64(st.st_size), rules: rules) { return .notEmpty }
            default:
                return .notEmpty // links, sockets, devices...
            }
        }
    }
}
