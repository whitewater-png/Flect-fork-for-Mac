import Foundation

/// The app's own check of every path the engine asks it to move to the Trash (`needs: trash`). The engine already
/// decides what goes; this makes sure a wrong or stale answer cannot reach anything else:
/// - the path is absolute, without `.` / `..` / empty components, and lies strictly inside one of the run's selected
///   folders (each resolved with realpath; for the path only its folder part is resolved, the last component is not
///   followed, so a link is judged as itself);
/// - it is never one of the selected items, files and folders alike, by text or by (device, inode), so another
///   spelling of a selected item (case, normalisation, firmlink) is refused too, and never a folder that holds a
///   selected item. (The engine plans neither either; this is the app's own check.)
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
        case invalidPath, outsideSelection, isSelected, holdsSelected, changed, keptChanged, engineOutdated, cancelled, missing, symlink, unexpectedType, notEmpty, unreadable
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
    /// `fingerprint`: what the engine verified for this path (`needs` item). The item is moved only while `lstat`
    /// still gives exactly that (`fingerprint(of:)`); a request without one comes from an engine older than this app
    /// and is refused (`.engineOutdated`). `kept`: for a duplicate, the copy that stays, which must also still have
    /// exactly the fingerprint the engine checked (`.keptChanged`).
    public static func check(_ path: String, scope: Scope, currentRun: UUID?, litter: LitterRules = LitterRules(),
                             fingerprint: String?, kept: TrashKept? = nil) -> Refusal? {
        guard scope.run == currentRun else { return .staleRun }
        if let r = check(path, inputs: scope.inputs, kind: scope.kind, litter: litter) { return r }
        guard let want = fingerprint else { return .engineOutdated }
        guard Self.fingerprint(of: path) == want else { return .changed }
        if let k = kept {
            guard k.path != path, Self.fingerprint(of: k.path) == k.fingerprint else { return .keptChanged }
        }
        return nil
    }

    /// The engine's fingerprint of `path` as `lstat` sees it now: `f:dev:ino:size:mtime_s.mtime_ns:ctime_s.ctime_ns`
    /// for a regular file, `d:dev:ino` for a folder, nil for anything else (`docs/mac-port/08-engine-protocol.ja.md`).
    public static func fingerprint(of path: String) -> String? {
        var st = stat()
        guard lstat(path, &st) == 0 else { return nil }
        let dev = UInt64(bitPattern: Int64(st.st_dev)), ino = UInt64(st.st_ino)
        switch st.st_mode & S_IFMT {
        case S_IFDIR: return "d:\(dev):\(ino)"
        case S_IFREG:
            let m = st.st_mtimespec, c = st.st_ctimespec
            return "f:\(dev):\(ino):\(UInt64(st.st_size)):\(m.tv_sec).\(m.tv_nsec):\(c.tv_sec).\(c.tv_nsec)"
        default: return nil
        }
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
        // Singular forms (English only, like every `.one` key): `.one` when the only item was not moved,
        // `.left.one` when one of several was not.
        let suffix = tally.requested == 1 ? ".one" : tally.notMoved == 1 ? ".left.one" : ""
        let template = suffix.isEmpty ? loc.t("app.trashPartial") : loc.t("app.trashPartial", variant: suffix)
        return Loc.format(template, [String(tally.moved), String(tally.requested), String(tally.notMoved)])
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
        let selected = inputs.compactMap(resolvedInput)
        let roots = selected.filter(\.isFolder)
        // One of the selected items itself, in any spelling (case, Unicode normalisation, firmlink): same text, or
        // the same (device, inode) as the item.
        if selected.contains(where: { $0.path == resolved }) { return .isSelected }
        if statError == nil, selected.contains(where: { $0.ids.contains(FileID(st)) }) { return .isSelected }
        // A folder that holds a selected item (or a selected link's target), by text or by (device, inode) of the
        // folders above the item, so case and normalisation variants are caught too.
        if selected.contains(where: { $0.path.hasPrefix(resolved + "/") }) { return .holdsSelected }
        if statError == nil, selected.contains(where: { $0.holders.contains(FileID(st)) }) { return .holdsSelected }
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

    struct FileID: Hashable {
        let dev: dev_t, ino: ino_t
        init(_ st: stat) { dev = st.st_dev; ino = st.st_ino }
    }

    struct Selected {
        /// Resolved as described at `resolvedInput`.
        let path: String
        /// The item's own (device, inode), plus its target's for a link.
        let ids: Set<FileID>
        /// The folders above the item (and above a link's target), resolved, by (device, inode).
        let holders: Set<FileID>
        let isFolder: Bool
    }

    /// A selected item (trailing slashes ignored, links not followed for `isFolder`). A folder is resolved fully with
    /// realpath(3), which also gives the case stored on disk; only folders have anything inside them that the engine
    /// scanned. Anything else (a file, a link) keeps its last component as it is. A link also stands for its target.
    /// nil when it cannot be looked at.
    static func resolvedInput(_ input: String) -> Selected? {
        var p = input
        while p.count > 1, p.hasSuffix("/") { p.removeLast() }
        guard p.hasPrefix("/"), !p.contains("\0") else { return nil }
        var st = stat()
        guard lstat(p, &st) == 0 else { return nil }
        var ids: Set<FileID> = [FileID(st)]
        var target = stat()
        if stat(p, &target) == 0 { ids.insert(FileID(target)) }
        var holders = Set<FileID>()
        // Folders above the item along its resolved path (realpath follows a link to its target).
        for start in [realPath(p), resolvedKeepingLast(p)].compactMap({ $0 }) {
            var dir = (start as NSString).deletingLastPathComponent
            while true {
                var a = stat()
                if lstat(dir, &a) == 0 { holders.insert(FileID(a)) }
                if dir == "/" || dir.isEmpty { break }
                dir = (dir as NSString).deletingLastPathComponent
            }
        }
        if (st.st_mode & S_IFMT) == S_IFDIR, let r = realPath(p) {
            return Selected(path: r, ids: ids, holders: holders, isFolder: true)
        }
        return Selected(path: resolvedKeepingLast(p) ?? p, ids: ids, holders: holders, isFolder: false)
    }

    static func realPath(_ p: String) -> String? {
        guard let r = realpath(p, nil) else { return nil }
        defer { free(r) }
        return String(cString: r)
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
