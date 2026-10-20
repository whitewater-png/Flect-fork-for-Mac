import Foundation

/// OS features the engine delegates to the app.
public enum NativeTask: Equatable {
    case heicConvert
    case pdfPreview
    case pdfRender(format: String)   // PNG or JPG
    case pdfText
    case pdfMerge
    case ocr
    /// Vision foreground mask; `fallback` = use the engine's colour key when it fails.
    case removeBackground(fallback: Bool)
}

public enum Dialog: Equatable {
    case pixels(base: String)        // "image.resize" / "video.resize"
    case targetBytes(base: String)   // "image.compress" / "video.compress"
    case rename
    case pdfOrder
    case trim
    case thumbnail
}

/// What to do with a task ID received from a Quick Action.
public enum Resolution: Equatable {
    case engine(String)
    /// A group such as `image.resize`: the user picks one of its entries (a preset, a format...).
    case choose(group: String)
    case dialog(Dialog)
    case native(NativeTask)
    case unavailable(key: String)
    case unknown
}

public enum Planner {
    private static func fmt(_ v: Double) -> String {
        v == v.rounded() && abs(v) < 1e15 ? String(Int64(v)) : String(v)
    }

    /// Settings-dependent preset values, with the same defaults as the engine.
    public static func resolve(_ id: String, settings s: SettingsStore) -> Resolution {
        let base = id.split(separator: "?", maxSplits: 1).first.map(String.init) ?? id
        // Already a concrete engine task with parameters (from a re-run or another front end)
        if id.contains("?") { return .engine(id) }
        let parts = base.split(separator: ".").map(String.init)

        switch base {
        case "image.resize", "video.resize", "image.compress", "video.compress", "image.rotate", "video.rotate",
             "image.convert", "video.convert", "video.extractAudio", "audio.convert", "text.convert", "text.encode",
             "text.lineEndings", "pdf.convert":
            return .choose(group: base)
        case "image.resize.custom": return .dialog(.pixels(base: "image.resize"))
        case "video.resize.custom": return .dialog(.pixels(base: "video.resize"))
        case "image.compress.custom": return .dialog(.targetBytes(base: "image.compress"))
        case "video.compress.custom": return .dialog(.targetBytes(base: "video.compress"))
        case "image.resize.p1": return .engine("image.resize?scale=\(fmt(s.double("Image", "Presets", "ResizeA", default: 50)))")
        case "image.resize.p2": return .engine("image.resize?scale=\(fmt(s.double("Image", "Presets", "ResizeB", default: 25)))")
        case "video.resize.p1": return .engine("video.resize?scale=\(fmt(s.double("Video", "Presets", "ResizeA", default: 50)))")
        case "video.resize.p2": return .engine("video.resize?scale=\(fmt(s.double("Video", "Presets", "ResizeB", default: 25)))")
        case "image.compress.p1": return .engine("image.compress?bytes=\(s.int64("Image", "Presets", "CompressA", default: 10 << 20))")
        case "image.compress.p2": return .engine("image.compress?bytes=\(s.int64("Image", "Presets", "CompressB", default: 5 << 20))")
        case "image.compress.p3": return .engine("image.compress?bytes=\(s.int64("Image", "Presets", "CompressC", default: 3 << 20))")
        case "image.compress.p4": return .engine("image.compress?bytes=\(s.int64("Image", "Presets", "CompressD", default: 1 << 20))")
        case "video.compress.discord": return .engine("video.compress?preset=discord")
        case "video.compress.p1": return .engine("video.compress?bytes=\(s.int64("Video", "Presets", "CompressA", default: 50 << 20))")
        case "video.compress.p2": return .engine("video.compress?bytes=\(s.int64("Video", "Presets", "CompressB", default: 25 << 20))")
        case "video.compress.p3": return .engine("video.compress?bytes=\(s.int64("Video", "Presets", "CompressC", default: 5 << 20))")
        case "image.rotate.p1": return .engine("image.rotate?deg=\(fmt(s.double("Image", "Presets", "RotateA", default: 45)))")
        case "image.rotate.p2": return .engine("image.rotate?deg=\(fmt(s.double("Image", "Presets", "RotateB", default: 90)))")
        case "image.rotate.p3": return .engine("image.rotate?deg=\(fmt(s.double("Image", "Presets", "RotateC", default: 180)))")
        case "video.rotate.p1": return .engine("video.rotate?deg=\(fmt(s.double("Video", "Presets", "RotateA", default: 45)))")
        case "video.rotate.p2": return .engine("video.rotate?deg=\(fmt(s.double("Video", "Presets", "RotateB", default: 90)))")
        case "video.rotate.p3": return .engine("video.rotate?deg=\(fmt(s.double("Video", "Presets", "RotateC", default: 180)))")
        case "image.convert.HEIC": return .native(.heicConvert)
        case "pdf.preview": return .native(.pdfPreview)
        case "pdf.convert.PNG": return .native(.pdfRender(format: "PNG"))
        case "pdf.convert.JPG": return .native(.pdfRender(format: "JPG"))
        case "pdf.convert.TXT": return .native(.pdfText)
        case "image.ocr": return .native(.ocr)
        case "pdf.merge": return .dialog(.pdfOrder)
        case "folder.rename": return .dialog(.rename)
        case "video.trim": return .dialog(.trim)
        case "video.thumbnail": return .dialog(.thumbnail)
        case "image.chromakey":
            switch s.string("Image", "BackgroundMode", default: "AiWithFallback") {
            case "ChromaKeyOnly": return .engine(base)
            case "AiOnly": return .native(.removeBackground(fallback: false))
            default: return .native(.removeBackground(fallback: true))
            }
        case "image.strip", "json.pretty", "json.sort", "folder.removeDuplicate", "folder.removeEmpty",
             "folder.tree", "folder.analyze", "folder.compress", "archive.extract", "jar.decompile",
             "video.subtitles", "audio.subtitles":
            return .engine(base)
        default: break
        }
        // <domain>.<action>.<FORMAT> for the format families
        if parts.count == 3, ["image.convert", "video.convert", "video.extractAudio", "audio.convert", "text.convert", "text.encode", "text.lineEndings"].contains(parts[0] + "." + parts[1]) {
            return .engine("\(parts[0]).\(parts[1])?to=\(parts[2])")
        }
        return .unknown
    }

    // MARK: building tasks from dialog answers

    public static func percentEncode(_ s: String) -> String {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        return s.addingPercentEncoding(withAllowedCharacters: allowed) ?? ""
    }

    public static func resizeCustom(base: String, width: Int, height: Int, keepAspect: Bool) -> String {
        "\(base)?scale=custom&w=\(max(0, width))&h=\(max(0, height))&keep=\(keepAspect ? 1 : 0)"
    }

    public static func compress(base: String, bytes: Int64) -> String { "\(base)?bytes=\(max(1, bytes))" }

    public static func rename(prefix: String, name: String, suffix: String, numbered: Bool, dryRun: Bool? = nil) -> String {
        var t = "folder.rename?prefix=\(percentEncode(prefix))&name=\(percentEncode(name))&suffix=\(percentEncode(suffix))&numbered=\(numbered ? 1 : 0)"
        if let d = dryRun { t += "&dry=\(d ? 1 : 0)" }
        return t
    }

    public static func trim(start: Double, end: Double) -> String { "video.trim?start=\(String(format: "%.3f", start))&end=\(String(format: "%.3f", end))" }
    public static func thumbnail(at t: Double) -> String { "video.thumbnail?t=\(String(format: "%.3f", t))" }
    public static func duplicates(mode: String) -> String { "folder.removeDuplicate?mode=\(mode)" }
    public static let confirmEmpty = "folder.removeEmpty?confirm=1"

    /// Human text for an entry of a chooser (presets show their current value).
    public static func label(for entry: MenuEntry, settings s: SettingsStore, loc: Loc) -> String {
        let id = entry.id
        let last = id.split(separator: ".").last.map(String.init) ?? id
        if entry.labelKey == "menu.to" { return loc.f("menu.to", [last]) }
        if id.hasSuffix(".custom") { return loc.t("menu.custom") }
        if id == "video.compress.discord" { return loc.t("menu.video.compress.discord") }
        if case .engine(let task) = resolve(id, settings: s), let value = task.split(separator: "=").last {
            if id.contains(".compress.p"), let bytes = Int64(value) { return ByteCountFormatter.string(fromByteCount: bytes, countStyle: .binary) }
            if id.contains(".resize.p") { return "\(value)%" }
            if id.contains(".rotate.p") { return "\(value)°" }
        }
        return loc.has(entry.labelKey) ? loc.t(entry.labelKey) : entry.label
    }
}
