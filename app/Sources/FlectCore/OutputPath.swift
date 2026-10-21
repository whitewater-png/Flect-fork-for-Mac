import Foundation

/// Output names next to the source (or in the feature's output folder), never overwriting:
/// `name`, `name (2)`, `name (3)` ... (same rules as the engine).
public enum OutputPath {
    /// Where results go for `source` given the "output folder" setting of its feature. Same rules as the
    /// engine (`paths::output_dir`): empty = beside the source; `~` and absolute paths as written;
    /// relative paths start from the source's folder; `.` and `..` are resolved textually.
    public static func directory(for source: URL, folder: String = "", home: URL = FileManager.default.homeDirectoryForCurrentUser) -> URL {
        let beside = source.deletingLastPathComponent()
        let setting = folder.trimmingCharacters(in: .whitespacesAndNewlines)
        if setting.isEmpty { return beside }
        let path: String
        if setting == "~" || setting.hasPrefix("~/") {
            path = home.path + "/" + String(setting.dropFirst())
        } else if setting.hasPrefix("/") {
            path = setting
        } else {
            path = beside.path + "/" + setting
        }
        var parts: [Substring] = []
        for part in path.split(separator: "/", omittingEmptySubsequences: true) {
            if part == "." { continue }
            if part == ".." { _ = parts.popLast(); continue }   // at the root there is nothing to pop
            parts.append(part)
        }
        return URL(fileURLWithPath: "/" + parts.joined(separator: "/"), isDirectory: true)
    }

    /// `directory(for:folder:)`, created when missing (a file in the way is an error). Nothing is created for an unset folder.
    public static func prepareDirectory(for source: URL, folder: String = "", home: URL = FileManager.default.homeDirectoryForCurrentUser, fm: FileManager = .default) throws -> URL {
        let dir = directory(for: source, folder: folder, home: home)
        let setting = folder.trimmingCharacters(in: .whitespacesAndNewlines)
        if !setting.isEmpty {
            // Same rule as the engine (`paths::prepare_output_dir`): a symlink in the way is refused, not followed.
            let anchor: String? = (setting == "~" || setting.hasPrefix("~/")) ? home.path
                : setting.hasPrefix("/") ? nil : source.deletingLastPathComponent().path
            try refuseSymlinks(dir.path, anchor: anchor, fm: fm)
            try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        }
        return dir
    }

    /// Thrown for an output folder that is a symlink or lies behind one (message key `msg.outputFolderLink`).
    public struct FolderIsSymlink: Error, Equatable { public let path: String }

    /// The message key for an error from the writing functions here.
    public static func errorKey(_ error: Error) -> String { error is FolderIsSymlink ? "msg.outputFolderLink" : "msg.ioError" }

    /// Components at or above `anchor` are the caller's own business; for an absolute setting the macOS aliases
    /// directly under `/` (`/tmp`, `/var`, `/etc`) are accepted. Only existing components can be links.
    static func refuseSymlinks(_ dir: String, anchor: String?, fm: FileManager) throws {
        let parts = dir.split(separator: "/", omittingEmptySubsequences: true)
        var current = ""
        for (i, part) in parts.enumerated() {
            current += "/" + part
            if let a = anchor, a == current || a.hasPrefix(current + "/") { continue }
            if anchor == nil && i == 0 { continue }
            guard let attrs = try? fm.attributesOfItem(atPath: current) else { break }   // does not exist yet
            if (attrs[.type] as? FileAttributeType) == .typeSymbolicLink { throw FolderIsSymlink(path: current) }
        }
    }

    public static func derive(source: URL, suffix: String = "", ext: String? = nil, folder: String = "", fm: FileManager = .default) -> URL {
        derive(in: directory(for: source, folder: folder), source: source, suffix: suffix, ext: ext, fm: fm)
    }

    public static func derive(in dir: URL, source: URL, suffix: String = "", ext: String? = nil, fm: FileManager = .default) -> URL {
        let stem = source.deletingPathExtension().lastPathComponent + suffix
        let extension_: String
        if let e = ext { extension_ = e.isEmpty ? "" : (e.hasPrefix(".") ? e : "." + e) }
        else { extension_ = source.pathExtension.isEmpty ? "" : "." + source.pathExtension }
        var candidate = dir.appendingPathComponent(stem + extension_)
        var n = 2
        while exists(candidate, fm) {
            candidate = dir.appendingPathComponent("\(stem) (\(n))\(extension_)")
            n += 1
        }
        return candidate
    }

    /// Dangling symlinks count as taken.
    static func exists(_ url: URL, _ fm: FileManager) -> Bool {
        (try? fm.attributesOfItem(atPath: url.path)) != nil
    }

    /// Create a new file exclusively (fails if the name appeared in the meantime) and write `data`.
    public static func writeNew(_ data: Data, source: URL, suffix: String = "", ext: String? = nil, folder: String = "") throws -> URL {
        let dir = try prepareDirectory(for: source, folder: folder)
        for _ in 0..<16 {
            let target = derive(in: dir, source: source, suffix: suffix, ext: ext)
            let fd = open(target.path, O_WRONLY | O_CREAT | O_EXCL, 0o644)
            if fd < 0 {
                if errno == EEXIST { continue }
                throw CocoaError(.fileWriteUnknown)
            }
            let handle = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
            do {
                try handle.write(contentsOf: data)
                try handle.close()
            } catch {
                try? FileManager.default.removeItem(at: target)
                throw error
            }
            return target
        }
        throw CocoaError(.fileWriteFileExists)
    }
}
