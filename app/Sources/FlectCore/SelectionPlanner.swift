import Foundation

/// What kind of item a selected path is (ported from `classify` in core/flect-core/src/menu.rs, which
/// the Finder Quick Actions follow through UTIs). The same decision is needed for files that arrive by
/// Dock drop, the Open panel or Services, where there is no Quick Action to pick the task.
public enum SelectionCategory: String, CaseIterable {
    case folder, archive, pdf, image, video, audio, text, jar
}

public struct SelectionItem: Equatable {
    public let path: String
    public let isDirectory: Bool
    public init(path: String, isDirectory: Bool) { self.path = path; self.isDirectory = isDirectory }
}

public struct Classification: Equatable {
    public let category: SelectionCategory
    public let format: String?
}

public enum SelectionClassifier {
    private static let image: [String: String] = [
        "png": "PNG", "jpg": "JPG", "jpeg": "JPG", "webp": "WEBP", "heic": "HEIC", "heif": "HEIC",
        "ico": "ICO", "gif": "GIF", "svg": "SVG",
    ]
    private static let video: [String: String] = [
        "mp4": "MP4", "mov": "MOV", "mkv": "MKV", "m4a": "M4A", "avi": "AVI", "webm": "WEBM", "flv": "FLV",
    ]
    private static let audio: [String: String] = [
        "mp3": "MP3", "wav": "WAV", "aif": "AIF", "aiff": "AIFF", "aac": "AAC", "ogg": "OGG", "wma": "WMA",
    ]
    private static let textFormats: [String: String] = [
        "txt": "TXT", "text": "TXT", "json": "JSON", "csv": "CSV", "xml": "XML", "ini": "INI", "cfg": "CFG",
        "conf": "CFG", "css": "CSS", "html": "HTML", "htm": "HTML", "md": "MD", "markdown": "MD",
    ]
    private static let textExtensions: Set<String> = [
        "txt", "text", "md", "markdown", "log", "csv", "tsv", "ini", "cfg", "conf", "json", "xml", "yaml", "yml",
        "toml", "html", "htm", "css", "js", "mjs", "cjs", "ts", "tsx", "jsx", "c", "h", "cpp", "hpp", "cc", "hh",
        "cs", "java", "kt", "py", "rb", "go", "rs", "php", "pl", "lua", "sh", "bash", "bat", "cmd", "ps1", "psm1",
        "sql", "srt", "vtt", "asc", "tex", "rst", "env", "props", "properties", "gradle", "cmake", "m", "mm",
        "swift", "vb", "r", "jl", "zsh",
    ]

    /// The extension the way Rust's `Path::extension` sees it: none for `.json` (a dotfile) or `name.`.
    static func fileExtension(of name: String) -> String? {
        guard let dot = name.lastIndex(of: "."), dot != name.startIndex else { return nil }
        let ext = name[name.index(after: dot)...]
        return ext.isEmpty ? nil : String(ext).lowercased()
    }

    public static func classify(path: String, isDirectory: Bool) -> Classification? {
        func c(_ cat: SelectionCategory, _ f: String?) -> Classification { Classification(category: cat, format: f) }
        if isDirectory { return c(.folder, nil) }
        let name = (path as NSString).lastPathComponent.lowercased()
        if name.utf8.count > 7 && name.hasSuffix(".tar.gz") { return c(.archive, "TAR.GZ") }
        guard let ext = fileExtension(of: name) else { return nil }
        switch ext {
        case "pdf": return c(.pdf, "PDF")
        case "jar": return c(.jar, "JAR")
        case "zip": return c(.archive, "ZIP")
        case "7z": return c(.archive, "7Z")
        case "tar": return c(.archive, "TAR")
        case "rar": return c(.archive, "RAR")
        default: break
        }
        if let f = image[ext] { return c(.image, f) }
        if let f = video[ext] { return c(.video, f) }
        if let f = audio[ext] { return c(.audio, f) }
        if textExtensions.contains(ext) { return c(.text, textFormats[ext] ?? "TEXT") }
        return nil
    }

    /// Text conversion targets per source format (`text_targets` in menu.rs).
    public static func textTargets(from format: String) -> [String] {
        var t: [String] = []
        if format != "TXT" { t.append("TXT") }
        switch format {
        case "CSV": t += ["JSON", "XML", "HTML", "MD"]
        case "JSON": t += ["CSV", "XML"]
        case "XML": t.append("JSON")
        case "INI", "CFG": t += ["JSON", "XML"]
        default: break
        }
        return t
    }
}

/// The tasks that apply to one kind of item within a selection.
public struct TaskSection: Equatable, Identifiable {
    public let category: SelectionCategory
    public let paths: [String]
    public let entries: [MenuEntry]
    public var id: String { category.rawValue }
}

/// What the action picker shows for a set of dropped / opened items.
public struct PickerSelection: Equatable {
    public let sections: [TaskSection]
    /// Items no Flect task handles (unknown types).
    public let unsupported: Int
    public let total: Int
    public let firstName: String
    /// Dropped things that were not usable paths (set by the app from `EntryRequest`).
    public var rejected = 0
}

/// Chooses the tasks for a selection from the engine's menu catalog, with the same rules as the Quick Actions
/// (`rules` in menu.json). It only ever offers IDs that exist in the catalog; running one still goes through
/// `Planner.resolve`, so nothing arriving by a new entry point reaches the engine unvalidated.
public enum TaskPicker {
    public static func selection(for items: [SelectionItem], catalog: MenuCatalog, isAnimatedGIF: (String) -> Bool = { _ in false }) -> PickerSelection {
        var byCategory: [SelectionCategory: [(path: String, format: String?)]] = [:]
        var unsupported = 0
        for item in items {
            if let c = SelectionClassifier.classify(path: item.path, isDirectory: item.isDirectory) {
                byCategory[c.category, default: []].append((item.path, c.format))
            } else {
                unsupported += 1
            }
        }
        var sections: [TaskSection] = []
        for category in SelectionCategory.allCases {
            guard let members = byCategory[category], let group = catalog.find(category.rawValue) else { continue }
            let entries = group.children.filter { applies($0.id, to: members, isAnimatedGIF: isAnimatedGIF) }
            if !entries.isEmpty { sections.append(TaskSection(category: category, paths: members.map { $0.path }, entries: entries)) }
        }
        let first = ((items.first?.path ?? "") as NSString).lastPathComponent
        return PickerSelection(sections: sections, unsupported: unsupported, total: items.count, firstName: first)
    }

    /// `rules` of menu.json: single / multiple / not-animated-gif / all-json / single-format.
    static func applies(_ id: String, to members: [(path: String, format: String?)], isAnimatedGIF: (String) -> Bool) -> Bool {
        switch id {
        case "pdf.preview", "video.trim", "video.thumbnail":
            return members.count == 1
        case "pdf.merge":
            return members.count >= 2
        case "image.convert":
            return !members.contains { $0.format == "GIF" && isAnimatedGIF($0.path) }
        case "json.pretty", "json.sort":
            return members.allSatisfy { $0.format == "JSON" }
        case "text.convert":
            guard let f = members.first?.format, members.allSatisfy({ $0.format == f }) else { return false }
            return !SelectionClassifier.textTargets(from: f).isEmpty
        default:
            return true
        }
    }
}

/// Turns the URLs of a drop / Open panel / Services call into items for the picker, applying the same
/// path rules as a Quick Action's selection file.
public enum EntryRequest {
    public struct Result: Equatable {
        public let items: [SelectionItem]
        /// Dropped things that were not usable paths (not file URLs, missing, too long, ...).
        public let rejected: Int
    }

    public static func items(from urls: [URL], fm: FileManager = .default) -> Swift.Result<Result, LaunchRequest.Failure> {
        var seen = Set<String>()
        var items: [SelectionItem] = []
        var rejected = 0
        for url in urls {
            guard url.isFileURL else { rejected += 1; continue }
            let path = url.standardizedFileURL.path
            guard LaunchRequest.isValidPath(path) else { rejected += 1; continue }
            if !seen.insert(path).inserted { continue }
            // lstat semantics: a symlink is never treated as the folder it points to (the engine does not follow them either).
            guard let type = (try? fm.attributesOfItem(atPath: path))?[.type] as? FileAttributeType else { rejected += 1; continue }
            items.append(SelectionItem(path: path, isDirectory: type == .typeDirectory))
        }
        if let failure = LaunchRequest.validate(paths: items.map { $0.path }) { return .failure(failure) }
        return .success(Result(items: items, rejected: rejected))
    }
}
