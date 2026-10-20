import Foundation

/// Output names next to the source, never overwriting: `name`, `name (2)`, `name (3)` ... (same rules as the engine).
public enum OutputPath {
    public static func derive(source: URL, suffix: String = "", ext: String? = nil, fm: FileManager = .default) -> URL {
        let dir = source.deletingLastPathComponent()
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
    public static func writeNew(_ data: Data, source: URL, suffix: String = "", ext: String? = nil) throws -> URL {
        for _ in 0..<16 {
            let target = derive(source: source, suffix: suffix, ext: ext)
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
