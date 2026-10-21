import Foundation

/// `--task <id> --input <file>`: the contract between a Quick Action and the app.
public struct LaunchRequest: Equatable {
    public static let maxPaths = 20_000
    public static let maxPathBytes = 4096
    /// The selection file is read only when it is smaller than this.
    public static let maxInputBytes = 64 << 20
    /// What the generated workflow's `mktemp -t flect-sel` creates: `$TMPDIR/flect-sel.XXXXXXXXXX`.
    public static let inputPrefix = "flect-sel."

    public let task: String
    public let paths: [String]

    public enum Failure: Error, Equatable {
        case missingArguments
        case unreadable
        case empty
        case tooMany
        case badPath(String)
        /// `--input` is not a selection file a Flect Quick Action made (wrong place, name, owner, type or size):
        /// it is neither read nor deleted.
        case untrustedInput
        /// NUL-separated, but with an empty entry that is not the single trailing one (`/a\0\0/b\0`).
        case malformed
        /// One line per path, as Quick Actions installed by Flect 0.2.0 and earlier write it. A file name with a line
        /// break in it would add a path the user never selected, so it is not used (except for one path without any
        /// line break inside): the Quick Actions have to be installed again.
        case outdatedQuickAction
        /// A listed path does not exist (lstat fails with ENOENT or ENOTDIR).
        case missing(String)
        /// macOS does not let Flect look at a listed path (lstat fails with EACCES or EPERM, as privacy protection
        /// does for Desktop, Documents, Downloads, removable volumes... until access is allowed).
        case notPermitted(String)
        /// lstat fails for another reason (I/O error, name too long, ...).
        case unreadablePath(String)
    }

    /// What a failed lstat of a listed path means. `errno` 0 means it succeeded (nil).
    public static func pathFailure(errno code: Int32, path: String) -> Failure? {
        let shown = String(path.prefix(80))
        switch code {
        case 0: return nil
        case ENOENT, ENOTDIR: return .missing(shown)
        case EACCES, EPERM: return .notPermitted(shown)
        default: return .unreadablePath(shown)
        }
    }

    /// lstat(2) of `path`: 0, or the errno it failed with.
    public static func lstatErrno(_ path: String) -> Int32 {
        var st = stat()
        return lstat(path, &st) == 0 ? 0 : errno
    }

    /// Arguments without the program name. The selection file is read and then deleted, but only when it is one a
    /// Quick Action made (`isSelectionFile`); anything else is left alone and the request is refused.
    /// The file holds the selected paths, each followed by a NUL byte (`printf '%s\0'`), exactly as given: nothing is
    /// trimmed and nothing but NUL separates paths (`selectionPaths`).
    /// `statPath` is the lstat used on the listed paths (tests inject errno values through it).
    public static func parse(arguments: [String], deleteInput: Bool = true, tempDirectories: [String]? = nil,
                             statPath: (String) -> Int32 = LaunchRequest.lstatErrno) -> Result<LaunchRequest, Failure> {
        var task: String?, input: String?
        var i = 0
        while i + 1 < arguments.count {
            switch arguments[i] {
            case "--task": task = arguments[i + 1]; i += 2
            case "--input": input = arguments[i + 1]; i += 2
            default: i += 1
            }
        }
        guard let task = task, !task.trimmingCharacters(in: .whitespaces).isEmpty, let input = input else { return .failure(.missingArguments) }
        let data: Data
        switch readSelectionFile(input, tempDirectories: tempDirectories ?? userTempDirectories, delete: deleteInput) {
        case .failure(let f): return .failure(f)
        case .success(let d): data = d
        }
        let paths: [String]
        switch selectionPaths(data) {
        case .failure(let f): return .failure(f)
        case .success(let p): paths = p
        }
        if let failure = validate(paths: paths) { return .failure(failure) }
        for p in paths {
            if let failure = pathFailure(errno: statPath(p), path: p) { return .failure(failure) }
        }
        return .success(LaunchRequest(task: task, paths: paths))
    }

    /// The paths in a selection file's bytes.
    /// - Contains a NUL: split on NUL only; one trailing empty entry (the last terminator) is dropped, any other empty
    ///   entry is `.malformed`. Entries are kept byte for byte (no trimming; a line break is part of the name).
    /// - No NUL (written by an outdated Quick Action, one path per line): accepted only when it is a single path with
    ///   no line break inside (an optional final `\n` is the old terminator); otherwise `.outdatedQuickAction`.
    /// - Empty: `.empty`.
    public static func selectionPaths(_ data: Data) -> Result<[String], Failure> {
        if data.isEmpty { return .failure(.empty) }
        if data.contains(0) {
            var parts = data.split(separator: 0, omittingEmptySubsequences: false)
            if parts.last?.isEmpty == true { parts.removeLast() }
            if parts.isEmpty || parts.contains(where: { $0.isEmpty }) { return .failure(.malformed) }
            var out: [String] = []
            out.reserveCapacity(parts.count)
            for p in parts {
                guard let s = String(data: Data(p), encoding: .utf8) else { return .failure(.unreadable) }
                out.append(s)
            }
            return .success(out)
        }
        guard var text = String(data: data, encoding: .utf8) else { return .failure(.unreadable) }
        if text.unicodeScalars.last == "\n" { text.unicodeScalars.removeLast() }
        // Every line separator `components(separatedBy: .newlines)` used to split on: LF, VT, FF, CR, NEL, LS, PS.
        let breaks = CharacterSet.newlines
        guard !text.isEmpty, !text.unicodeScalars.contains(where: { breaks.contains($0) }) else { return .failure(.outdatedQuickAction) }
        return .success([text])
    }

    // MARK: the task

    /// What `--task` may name. Launch arguments come from anyone who can run `open -a Flect-for-Mac --args ...`, so
    /// they never carry engine parameters (`folder.removeDuplicate?mode=all`, `folder.removeEmpty?confirm=1`,
    /// `folder.rename?...&dry=0` would skip the preview and delete or rename at once).
    public enum LaunchTask: Equatable {
        /// An id of the engine's menu below the top level, as the generated Quick Actions pass it (`image.resize`,
        /// `folder.removeDuplicate`, `pdf.convert.PNG`). It goes through `Planner.resolveCatalogID`, which always
        /// starts the deleting and renaming tasks with their preview or dialog.
        case catalog(String)
        /// `preset:<UUID>` (a Quick Action made for a saved preset; its steps are re-checked by `Pipeline.plan`).
        case preset(UUID)
    }

    /// `preset:<UUID>`, or a catalog id that `catalog` contains below its top level. Anything else (parameters,
    /// unknown ids, top-level groups) is nil and must be refused.
    public static func launchTask(_ task: String, catalog: MenuCatalog?) -> LaunchTask? {
        if let id = presetID(task) { return .preset(id) }
        guard PickerRef.isCatalogID(task), task.contains("."), let catalog = catalog, catalog.find(task) != nil else { return nil }
        return .catalog(task)
    }

    /// The UUID of `preset:<UUID>`, nil for anything else.
    public static func presetID(_ task: String) -> UUID? {
        guard task.hasPrefix("preset:") else { return nil }
        let text = String(task.dropFirst(7))
        guard PresetFile.isUUID(text) else { return nil }
        return UUID(uuidString: text)
    }

    // MARK: the selection file

    /// The user's temporary folders, where `mktemp -t` puts the selection file: `NSTemporaryDirectory()`, `$TMPDIR`
    /// and the per-user `DARWIN_USER_TEMP_DIR`. Compared after resolving links (`/var` is `/private/var`).
    public static var userTempDirectories: [String] {
        var dirs = [NSTemporaryDirectory()]
        if let t = ProcessInfo.processInfo.environment["TMPDIR"], !t.isEmpty { dirs.append(t) }
        var buf = [CChar](repeating: 0, count: Int(PATH_MAX) + 1)
        if confstr(_CS_DARWIN_USER_TEMP_DIR, &buf, buf.count) > 0 { dirs.append(String(cString: buf)) }
        return dirs
    }

    static func realPath(_ p: String) -> String? {
        guard let r = realpath(p, nil) else { return nil }
        defer { free(r) }
        return String(cString: r)
    }

    /// `flect-sel.` followed by mktemp's random letters and digits, nothing else.
    public static func isSelectionFileName(_ name: String) -> Bool {
        guard name.hasPrefix(inputPrefix) else { return false }
        let rest = name.utf8.dropFirst(inputPrefix.utf8.count)
        return (1...32).contains(rest.count) && rest.allSatisfy { ($0 >= 48 && $0 <= 57) || ($0 >= 65 && $0 <= 90) || ($0 >= 97 && $0 <= 122) }
    }

    /// Where the file is and what it is called: an absolute path whose name is `isSelectionFileName` and whose folder,
    /// with links resolved, is one of `tempDirectories` (also resolved). Not a check of the file itself (`stat` does that).
    public static func isInTempDirectory(_ path: String, tempDirectories: [String]) -> Bool {
        guard path.hasPrefix("/"), !path.contains("\0") else { return false }
        let ns = path as NSString
        guard isSelectionFileName(ns.lastPathComponent), let parent = realPath(ns.deletingLastPathComponent) else { return false }
        let allowed = Set(tempDirectories.compactMap { realPath($0) })
        return allowed.contains(parent)
    }

    /// The file itself: a regular file (not a link, folder, FIFO...), owned by this user, one link, under the size limit.
    static func isTrustedStat(_ st: stat) -> Bool {
        (st.st_mode & S_IFMT) == S_IFREG && st.st_uid == getuid() && st.st_nlink == 1 && st.st_size >= 0 && st.st_size < off_t(maxInputBytes)
    }

    /// Whether `path` is a selection file a Quick Action made: right folder and name (`isInTempDirectory`), and `lstat`
    /// says it is a plain file of ours (`isTrustedStat`).
    public static func isSelectionFile(_ path: String, tempDirectories: [String] = userTempDirectories) -> Bool {
        guard isInTempDirectory(path, tempDirectories: tempDirectories) else { return false }
        var st = stat()
        return lstat(path, &st) == 0 && isTrustedStat(st)
    }

    /// Reads the selection file and, when `delete`, removes it with unlink(2), only if it passes `isSelectionFile`
    /// both before and after opening it (O_NOFOLLOW, same device and inode as the name). Never removes anything else,
    /// never follows a link, never removes a folder.
    static func readSelectionFile(_ path: String, tempDirectories: [String], delete: Bool) -> Result<Data, Failure> {
        guard isInTempDirectory(path, tempDirectories: tempDirectories) else { return .failure(.untrustedInput) }
        var before = stat()
        guard lstat(path, &before) == 0 else { return .failure(.unreadable) }
        guard isTrustedStat(before) else { return .failure(.untrustedInput) }
        let fd = open(path, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)
        guard fd >= 0 else { return .failure(.unreadable) }
        defer { close(fd) }
        var opened = stat()
        guard fstat(fd, &opened) == 0, isTrustedStat(opened), opened.st_dev == before.st_dev, opened.st_ino == before.st_ino else {
            return .failure(.untrustedInput)
        }
        var data = Data()
        var chunk = [UInt8](repeating: 0, count: 64 << 10)
        while true {
            let n = read(fd, &chunk, chunk.count)
            if n < 0 { return .failure(.unreadable) }
            if n == 0 { break }
            data.append(contentsOf: chunk[0..<n])
            if data.count >= maxInputBytes { return .failure(.unreadable) }
        }
        if delete {
            // Unlink the name only while it still is the file that was read.
            var now = stat()
            if lstat(path, &now) == 0, isTrustedStat(now), now.st_dev == opened.st_dev, now.st_ino == opened.st_ino {
                unlink(path)
            }
        }
        return .success(data)
    }

    // MARK: paths

    /// One path as the engine accepts it: absolute, bounded, no NUL.
    public static func isValidPath(_ p: String) -> Bool {
        p.hasPrefix("/") && p.utf8.count <= maxPathBytes && !p.contains("\0")
    }

    /// The rules every selection must pass before it reaches the planner, whichever entry point it came through
    /// (Quick Action file, Dock drop, Open panel, Services).
    public static func validate(paths: [String]) -> Failure? {
        if paths.isEmpty { return .empty }
        if paths.count > maxPaths { return .tooMany }
        for l in paths where !isValidPath(l) { return .badPath(String(l.prefix(80))) }
        return nil
    }
}
