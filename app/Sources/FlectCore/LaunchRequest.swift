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
    }

    /// Arguments without the program name. The selection file is read and then deleted, but only when it is one a
    /// Quick Action made (`isSelectionFile`); anything else is left alone and the request is refused.
    public static func parse(arguments: [String], deleteInput: Bool = true, tempDirectories: [String]? = nil) -> Result<LaunchRequest, Failure> {
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
        guard var text = String(data: data, encoding: .utf8) else { return .failure(.unreadable) }
        if text.hasPrefix("\u{FEFF}") { text.removeFirst() }
        let lines = text.components(separatedBy: .newlines).map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        if let failure = validate(paths: lines) { return .failure(failure) }
        return .success(LaunchRequest(task: task, paths: lines))
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
