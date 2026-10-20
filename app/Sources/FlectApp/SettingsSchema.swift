import Foundation

/// Declarative description of the settings screen. Keys and values match settings.json (PascalCase, enum names as strings).
enum FieldKind {
    case toggle
    case text
    case int(ClosedRange<Int>)
    case double(ClosedRange<Double>)
    case choice([(value: String, key: String, fallback: String)])
    /// Stored as bytes, edited in MiB.
    case mib
}

struct Field: Identifiable {
    let id: String
    let path: [String]
    let key: String
    let fallback: String
    let kind: FieldKind
    let def: Any

    init(_ path: [String], _ key: String, _ fallback: String, _ kind: FieldKind, _ def: Any) {
        self.id = path.joined(separator: ".")
        self.path = path; self.key = key; self.fallback = fallback; self.kind = kind; self.def = def
    }
}

struct SettingsSection: Identifiable {
    let id: String
    let key: String
    let fallback: String
    let fields: [Field]
}

enum SettingsSchema {
    private static func choice(_ base: String, _ items: [(String, String)]) -> FieldKind {
        .choice(items.map { (value: $0.0, key: "\(base).\($0.1)", fallback: $0.0) })
    }

    static let sections: [SettingsSection] = [
        SettingsSection(id: "general", key: "settings.nav.general", fallback: "General", fields: [
            Field(["General", "Language"], "settings.general.language", "Language", choice("settings.general.language", [("System", "system"), ("English", "en"), ("Japanese", "ja"), ("ChineseSimplified", "zh")]), "System"),
            Field(["General", "CloseOnSuccess"], "settings.general.dialog.success", "Close when finished", .toggle, true),
            Field(["General", "CloseOnFailure"], "settings.general.dialog.failure", "Close on failure", .toggle, false),
            Field(["General", "CloseOnCancel"], "settings.general.dialog.cancel", "Close on cancel", .toggle, true),
        ]),
        SettingsSection(id: "folder", key: "settings.nav.folder", fallback: "Folder", fields: [
            Field(["Folder", "DeleteMethod"], "settings.folder.delete", "Delete method", choice("settings.folder.delete", [("RecycleBin", "recycle"), ("Permanent", "permanent")]), "RecycleBin"),
            Field(["Folder", "Duplicates", "Strictness"], "settings.folder.dup.strictness", "Duplicate check", choice("settings.folder.dup", [("SizeOnly", "sizeOnly"), ("HeadHash", "headHash"), ("FullContent", "fullContent")]), "FullContent"),
            Field(["Folder", "Duplicates", "MatchName"], "settings.folder.dup.matchName", "Names must match", .toggle, false),
            Field(["Folder", "Duplicates", "MatchTimestamp"], "settings.folder.dup.matchTime", "Timestamps must match", .toggle, false),
            Field(["Folder", "Duplicates", "IgnoreEmptyFiles"], "settings.folder.dup.ignoreEmpty", "Ignore empty files", .toggle, true),
            Field(["Folder", "Duplicates", "Keep"], "settings.folder.dup.keep", "Which file to keep", choice("settings.folder.dup.keep", [("Ask", "ask"), ("Oldest", "oldest"), ("Newest", "newest")]), "Ask"),
            Field(["Folder", "Rename", "UseNumbering"], "settings.folder.rename.numbering", "Number the files", .toggle, true),
            Field(["Folder", "Rename", "DigitCount"], "settings.folder.rename.digits", "Digits (0 = automatic)", .int(0...12), 0),
            Field(["Folder", "Rename", "StartNumber"], "settings.folder.rename.start", "Start number", .int(0...1_000_000), 1),
            Field(["Folder", "Rename", "DryRun"], "settings.folder.rename.dryRun", "Preview before renaming", .toggle, false),
            Field(["Folder", "Empty", "IgnoreZeroByteFiles"], "settings.folder.empty.zero", "Treat 0-byte files as empty", .toggle, false),
            Field(["Folder", "Tree", "FoldersOnly"], "settings.folder.tree.foldersOnly", "Tree: folders only", .toggle, false),
            Field(["Folder", "Analyze", "SaveFormat"], "settings.folder.analyze.format", "Report format", .choice(["Csv", "Json", "Html", "Txt"].map { (value: $0, key: "", fallback: $0) }), "Csv"),
            Field(["Folder", "Archive", "CompressFormat"], "settings.folder.compress.format", "Archive format", choice("settings.folder.compress.format", [("Zip", "zip"), ("SevenZip", "sevenzip"), ("Tar", "tar"), ("TarGz", "targz")]), "Zip"),
            Field(["Folder", "ScanExclusions", "Folders"], "settings.exclusions.folders", "Skip folders (names, separated by ;)", .text, ""),
            Field(["Folder", "ScanExclusions", "Files"], "settings.exclusions.files", "Skip files", .text, ""),
            Field(["Folder", "ScanExclusions", "Extensions"], "settings.exclusions.extensions", "Skip extensions", .text, ""),
        ]),
        SettingsSection(id: "image", key: "settings.nav.image", fallback: "Image", fields: [
            Field(["Image", "Presets", "ResizeA"], "settings.presets.resize", "Resize preset A (%)", .double(1...1000), 50.0),
            Field(["Image", "Presets", "ResizeB"], "settings.presets.resize", "Resize preset B (%)", .double(1...1000), 25.0),
            Field(["Image", "Presets", "CompressA"], "settings.presets.compress", "Compress preset A (MiB)", .mib, 10.0),
            Field(["Image", "Presets", "CompressB"], "settings.presets.compress", "Compress preset B (MiB)", .mib, 5.0),
            Field(["Image", "Presets", "CompressC"], "settings.presets.compress", "Compress preset C (MiB)", .mib, 3.0),
            Field(["Image", "Presets", "CompressD"], "settings.presets.compress", "Compress preset D (MiB)", .mib, 1.0),
            Field(["Image", "Presets", "RotateA"], "settings.presets.rotate", "Rotate preset A (deg)", .double(-360...360), 45.0),
            Field(["Image", "Presets", "RotateB"], "settings.presets.rotate", "Rotate preset B (deg)", .double(-360...360), 90.0),
            Field(["Image", "Presets", "RotateC"], "settings.presets.rotate", "Rotate preset C (deg)", .double(-360...360), 180.0),
            Field(["Image", "AllowWebpFallback"], "settings.image.webpFallback", "Allow WebP when a target size cannot be met", .toggle, true),
            Field(["Image", "WebpQuality"], "settings.image.webpQuality", "WebP quality", .int(1...100), 80),
            Field(["Image", "BackgroundMode"], "settings.image.background.mode", "Background removal method", .choice([("AiWithFallback", "settings.image.background.mode.aiFallback"), ("AiOnly", "settings.image.background.mode.aiOnly"), ("ChromaKeyOnly", "settings.image.background.mode.chromaOnly")].map { (value: $0.0, key: $0.1, fallback: $0.0) }), "AiWithFallback"),
            Field(["Image", "BackgroundKeyColor"], "settings.image.background.color", "Background key color (#RRGGBB)", .text, "#00FF00"),
            Field(["Image", "BackgroundTolerance"], "settings.image.background.tolerance", "Background tolerance", .double(0...100), 25.0),
            Field(["Image", "Metadata", "RemoveExif"], "settings.image.metadata.exif", "Remove EXIF", .toggle, true),
            Field(["Image", "Metadata", "RemoveGps"], "settings.image.metadata.gps", "Remove GPS", .toggle, true),
            Field(["Image", "Metadata", "RemoveComments"], "settings.image.metadata.comments", "Remove comments", .toggle, true),
            Field(["Image", "Metadata", "RemoveColorProfile"], "settings.image.metadata.profile", "Remove color profile", .toggle, false),
            Field(["Image", "Metadata", "ApplyOrientation"], "settings.image.metadata.orientation", "Apply orientation", .toggle, true),
        ]),
        SettingsSection(id: "video", key: "settings.nav.video", fallback: "Video", fields: [
            Field(["Video", "Codec"], "settings.video.codec", "Codec", choice("settings.video.codec", [("H264", "h264"), ("H265", "h265"), ("Vp9", "vp9"), ("Av1", "av1")]), "H264"),
            Field(["Video", "Container"], "settings.video.container", "Container", choice("settings.video.container", [("Auto", "auto"), ("Mp4", "mp4"), ("Mkv", "mkv"), ("WebM", "webm")]), "Auto"),
            Field(["Video", "Hardware"], "settings.video.hardware", "Hardware encoder", .choice([(value: "None", key: "settings.video.hardware.none", fallback: "None"), (value: "VideoToolbox", key: "", fallback: "VideoToolbox")]), "None"),
            Field(["Video", "Priority"], "settings.video.priority", "Priority", choice("settings.video.priority", [("Speed", "speed"), ("Balanced", "balanced"), ("Quality", "quality")]), "Balanced"),
            Field(["Video", "Presets", "ResizeA"], "settings.presets.resize", "Resize preset A (%)", .double(1...1000), 50.0),
            Field(["Video", "Presets", "ResizeB"], "settings.presets.resize", "Resize preset B (%)", .double(1...1000), 25.0),
            Field(["Video", "Presets", "CompressA"], "settings.presets.compress", "Compress preset A (MiB)", .mib, 50.0),
            Field(["Video", "Presets", "CompressB"], "settings.presets.compress", "Compress preset B (MiB)", .mib, 25.0),
            Field(["Video", "Presets", "CompressC"], "settings.presets.compress", "Compress preset C (MiB)", .mib, 5.0),
            Field(["Video", "Presets", "RotateA"], "settings.presets.rotate", "Rotate preset A (deg)", .double(-360...360), 45.0),
            Field(["Video", "Presets", "RotateB"], "settings.presets.rotate", "Rotate preset B (deg)", .double(-360...360), 90.0),
            Field(["Video", "Presets", "RotateC"], "settings.presets.rotate", "Rotate preset C (deg)", .double(-360...360), 180.0),
            Field(["Video", "Trim", "Accuracy"], "settings.video.trim.accuracy", "Trim accuracy", choice("settings.video.trim", [("Keyframe", "keyframe"), ("Exact", "exact")]), "Keyframe"),
            Field(["Video", "Thumbnail", "Format"], "settings.video.thumbnail.format", "Thumbnail format", choice("settings.video.thumbnail.format", [("Png", "png"), ("Jpg", "jpg")]), "Png"),
            Field(["Video", "Thumbnail", "Quality"], "settings.video.thumbnail.quality", "Thumbnail quality", .int(1...100), 90),
            Field(["Video", "GifFps"], "settings.video.gif.fps", "GIF frames per second", .int(1...60), 15),
            Field(["Video", "GifMaxWidth"], "settings.video.gif.width", "GIF max width", .int(16...4096), 640),
            Field(["Video", "Subtitles", "Model"], "settings.video.subtitles.model", "Subtitle model", .choice([("Tiny", "settings.video.subtitles.model.tiny"), ("Base", "settings.video.subtitles.model.base"), ("Small", "settings.video.subtitles.model.small"), ("Medium", "settings.video.subtitles.model.medium"), ("LargeV3", "settings.video.subtitles.model.large"), ("Turbo", "settings.video.subtitles.model.turbo")].map { (value: $0.0, key: $0.1, fallback: $0.0) }), "Small"),
            Field(["Video", "Subtitles", "Language"], "settings.video.subtitles.language", "Subtitle language (ja, en... empty = automatic)", .text, ""),
            Field(["Video", "Subtitles", "Device"], "settings.video.subtitles.device", "Subtitle device", choice("settings.video.subtitles.device", [("Auto", "auto"), ("Cpu", "cpu")]), "Auto"),
            Field(["Video", "Subtitles", "Output"], "settings.video.subtitles.output", "Subtitle format", choice("settings.video.subtitles.output", [("Srt", "srt"), ("Vtt", "vtt"), ("Txt", "txt")]), "Srt"),
            Field(["Video", "RotateFillColor"], "settings.video.rotate.fill", "Rotate fill color (#RRGGBB)", .text, "#000000"),
        ]),
        SettingsSection(id: "text", key: "settings.nav.text", fallback: "Text", fields: [
            Field(["Text", "SjisSubstitute"], "settings.text.sjis", "Shift_JIS substitute character", .text, "?"),
            Field(["Text", "JsonSort", "Comparison"], "settings.text.json.comparison", "JSON key order", choice("settings.text.json.comparison", [("Ordinal", "ordinal"), ("IgnoreCase", "ignoreCase"), ("Natural", "natural")]), "Ordinal"),
            Field(["Text", "JsonSort", "Descending"], "settings.text.json.descending", "Descending", .toggle, false),
            Field(["Text", "JsonSort", "Recursive"], "settings.text.json.recursive", "Sort nested objects", .toggle, true),
            Field(["Text", "JsonSort", "SortPrimitiveArrays"], "settings.text.json.arrays", "Sort arrays of plain values", .toggle, false),
            Field(["Text", "JsonSort", "PinnedKeys"], "settings.text.json.pinned", "Pinned keys (comma separated)", .text, ""),
        ]),
        SettingsSection(id: "ocr", key: "settings.nav.ocr", fallback: "OCR", fields: [
            Field(["Ocr", "Output"], "settings.ocr.output", "Output", choice("settings.ocr.output", [("TextFile", "file"), ("Clipboard", "clipboard"), ("ShowOnly", "show")]), "TextFile"),
            Field(["Ocr", "Language"], "settings.ocr.language", "Languages (e.g. ja-JP, en-US)", .text, ""),
        ]),
    ]
}
