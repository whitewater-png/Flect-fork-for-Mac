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
