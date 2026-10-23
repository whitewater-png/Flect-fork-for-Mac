import Foundation

/// What the download dialog offers. `kind` and `maxHeight` are what the engine's `download` request takes
/// (it accepts only these combinations).
public enum DownloadChoice: String, CaseIterable, Identifiable {
    case best, p1080, p720, p480, audio

    public var id: String { rawValue }
    public var kind: String { self == .audio ? "audio" : "video" }
    public var maxHeight: Int? {
        switch self {
        case .p1080: return 1080
        case .p720: return 720
        case .p480: return 480
        case .best, .audio: return nil
        }
    }
    public var labelKey: String { "dialog.download.q.\(rawValue)" }
}

/// The same rules as the engine's `download::validate_url`, so the dialog can tell early. The engine
/// checks again and is the one that decides.
public enum DownloadURL {
    static let maxBytes = 2048
    /// Addresses in one `download` request (the engine's `MAX_DOWNLOAD_URLS`).
    public static let maxPerRequest = 20

    /// The address without surrounding spaces, or nil when it is not a plain http(s) address.
    public static func normalized(_ text: String) -> String? {
        let url = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !url.isEmpty, url.utf8.count <= maxBytes else { return nil }
        guard !url.unicodeScalars.contains(where: { CharacterSet.whitespacesAndNewlines.contains($0) || CharacterSet.controlCharacters.contains($0) }) else { return nil }
        let lower = url.lowercased()
        let rest: Substring
        if lower.hasPrefix("https://") { rest = lower.dropFirst(8) }
        else if lower.hasPrefix("http://") { rest = lower.dropFirst(7) }
        else { return nil }
        let authority = rest.prefix { $0 != "/" && $0 != "?" && $0 != "#" }
        guard !authority.isEmpty, !authority.contains("@") else { return nil }
        return url
    }

    public static func isPlausible(_ text: String) -> Bool { normalized(text) != nil }
}

extension DownloadURL {
    /// The plain http(s) addresses in a piece of text (a selection from a web page, say), in order, without repeats.
    /// Punctuation that ends a sentence is not part of the address; a closing bracket is kept when it matches an
    /// opening one inside it (`.../Foo_(bar)`).
    public static func addresses(inText text: String, limit: Int = maxPerRequest) -> [String] {
        guard let pattern = try? NSRegularExpression(pattern: #"https?://[^\s<>"'`]+"#, options: [.caseInsensitive]) else { return [] }
        let ns = text as NSString
        var found: [String] = [], seen = Set<String>()
        for m in pattern.matches(in: text, range: NSRange(location: 0, length: ns.length)) {
            var candidate = ns.substring(with: m.range)
            while let last = candidate.last {
                if ".,;:!?]}".contains(last) { candidate.removeLast() }
                else if last == ")", candidate.filter({ $0 == ")" }).count > candidate.filter({ $0 == "(" }).count { candidate.removeLast() }
                else { break }
            }
            guard let url = normalized(candidate), seen.insert(url).inserted else { continue }
            found.append(url)
            if found.count >= limit { break }
        }
        return found
    }
}

/// Internet shortcut files: `.webloc` (a property list with a `URL` key, what Safari and Finder make when a link is
/// dragged out) and `.url` (a text file with a `URL=` line).
public enum LinkFile {
    static let maxBytes = 64 * 1024

    /// The address inside, or nil when there is none or it is not a plain http(s) address.
    public static func address(in data: Data, fileExtension ext: String) -> String? {
        guard data.count <= maxBytes else { return nil }
        let raw: String?
        switch ext.lowercased() {
        case "webloc":
            let plist = try? PropertyListSerialization.propertyList(from: data, options: [], format: nil)
            raw = (plist as? [String: Any])?["URL"] as? String
        case "url":
            var text = String(decoding: data, as: UTF8.self)
            if text.hasPrefix("\u{FEFF}") { text.removeFirst() }
            raw = text.components(separatedBy: .newlines).compactMap { line -> String? in
                let t = line.trimmingCharacters(in: .whitespaces)
                return t.lowercased().hasPrefix("url=") ? String(t.dropFirst(4)) : nil
            }.first
        default:
            raw = nil
        }
        return raw.flatMap(DownloadURL.normalized)
    }

    /// Reads a link file. Only a plain file (not a link to one, not a pipe) of at most 64 KiB is read.
    public static func address(atPath path: String) -> String? {
        var st = stat()
        guard lstat(path, &st) == 0, (st.st_mode & S_IFMT) == S_IFREG, st.st_size <= Int64(maxBytes) else { return nil }
        guard let handle = FileHandle(forReadingAtPath: path) else { return nil }
        defer { try? handle.close() }
        guard let data = try? handle.read(upToCount: maxBytes + 1) else { return nil }
        return address(in: data, fileExtension: (path as NSString).pathExtension)
    }
}
