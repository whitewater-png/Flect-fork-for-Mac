import Foundation

// Steps and chains (presets and pipelines), the Swift twin of core/flect-core/src/pipeline.rs: the same grammar,
// the same allow-list, the same bounds. The engine checks again before running anything; this side exists so the
// app can refuse a bad presets.json up front and only ever save what the engine will accept.
// See docs/mac-port/17-presets-pipeline.ja.md.

/// What a step reads and writes.
public enum StepKind: String, Equatable {
    case image, video, audio, text

    /// The kind of the items in a picker section (`nil`: no chainable task reads them).
    public init?(category: SelectionCategory) {
        switch category {
        case .image: self = .image
        case .video: self = .video
        case .audio: self = .audio
        case .text: self = .text
        default: return nil
        }
    }
}

public struct PipelineStep: Equatable {
    /// The task string as the engine takes it (`image.convert?to=JPG`).
    public let task: String
    /// The task id without parameters.
    public let name: String
    public let params: [String: String]
    public let accepts: [StepKind]
    public let produces: StepKind
}

public enum PipelineError: Error, Equatable {
    case stepCount
    case notAllowed(String)
    case badStep(String)
    case incompatible(String, String)

    /// The engine's message key (`msg.pipeline*`), so both sides show the same text.
    public var key: String {
        switch self {
        case .stepCount: return "msg.pipelineStepCount"
        case .notAllowed: return "msg.pipelineStepNotAllowed"
        case .badStep: return "msg.pipelineBadStep"
        case .incompatible: return "msg.pipelineIncompatible"
        }
    }

    public var args: [String] {
        switch self {
        case .stepCount: return [String(Pipeline.maxSteps)]
        case .notAllowed(let s), .badStep(let s): return [s]
        case .incompatible(let a, let b): return [a, b]
        }
    }
}

public enum Pipeline {
    public static let maxSteps = 8
    public static let maxStepBytes = 1024
    static let maxBytesParam: UInt64 = 1 << 40
    static let maxPixels: UInt64 = 32768

    static let audioTargets = ["MP3", "M4A", "WAV", "AIF", "AIFF", "AAC", "OGG", "WMA"]
    /// `to=` values per converting task, in menu order.
    public static let targets: [String: [String]] = [
        "image.convert": ["PNG", "JPG", "WEBP", "ICO", "GIF", "SVG"],
        "video.convert": ["MP4", "MOV", "MKV", "M4A", "AVI", "WEBM", "FLV", "GIF"],
        "video.extractAudio": audioTargets,
        "audio.convert": audioTargets,
        "text.convert": ["TXT", "JSON", "XML", "CSV", "HTML", "MD"],
        "text.encode": ["UTF8", "UTF8BOM", "UTF16", "SJIS"],
        "text.lineEndings": ["CRLF", "LF"],
    ]

    private typealias Check = ([String: String]) -> StepKind?
    private static let image: [StepKind] = [.image], video: [StepKind] = [.video], text: [StepKind] = [.text]

    /// The chainable tasks, in the order the editor offers them: (id, accepted inputs, parameter check).
    private static let specs: [(String, [StepKind], Check)] = [
        ("image.resize", image, { resize($0, .image) }),
        ("image.compress", image, { p in only(p, ["bytes"]) && (integer(p["bytes"], max: maxBytesParam) ?? 0) > 0 ? .image : nil }),
        ("image.rotate", image, { rotate($0, .image) }),
        ("image.convert", image, { to($0, "image.convert").map { _ in .image } }),
        ("image.strip", image, { $0.isEmpty ? .image : nil }),
        ("image.chromakey", image, { $0.isEmpty ? .image : nil }),
        ("video.resize", video, { resize($0, .video) }),
        ("video.compress", video, { p in
            let ok: Bool
            switch (p["bytes"], p["preset"]) {
            case (let b?, nil): ok = (integer(b, max: maxBytesParam) ?? 0) > 0
            case (nil, let preset?): ok = preset == "discord"
            default: ok = false
            }
            return ok && only(p, ["bytes", "preset"]) ? .video : nil
        }),
        ("video.rotate", video, { rotate($0, .video) }),
        ("video.convert", video, { p in
            switch to(p, "video.convert") {
            case "M4A"?: return .audio
            case "GIF"?: return .image
            case nil: return nil
            default: return .video
            }
        }),
        ("video.extractAudio", video, { to($0, "video.extractAudio").map { _ in .audio } }),
        ("audio.convert", [.audio, .video], { to($0, "audio.convert").map { _ in .audio } }),
        ("text.convert", text, { to($0, "text.convert").map { _ in .text } }),
        ("text.encode", text, { to($0, "text.encode").map { _ in .text } }),
        ("text.lineEndings", text, { to($0, "text.lineEndings").map { _ in .text } }),
        ("json.pretty", text, { $0.isEmpty ? .text : nil }),
        ("json.sort", text, { $0.isEmpty ? .text : nil }),
    ]

    public static var chainableTasks: [String] { specs.map { $0.0 } }

    public static func accepts(_ task: String) -> [StepKind] { specs.first { $0.0 == task }?.1 ?? [] }

    // MARK: values

    /// `-12.5`, `50`: plain decimal only (no exponent, no inf/nan).
    static func number(_ v: String?) -> Double? {
        guard let v = v else { return nil }
        let body = v.hasPrefix("-") ? String(v.dropFirst()) : v
        let parts = body.split(separator: ".", maxSplits: 1, omittingEmptySubsequences: false).map(String.init)
        let digits = { (s: String) in !s.isEmpty && s.utf8.count <= 12 && s.utf8.allSatisfy { $0 >= 48 && $0 <= 57 } }
        guard digits(parts[0]), parts.count == 1 || digits(parts[1]) else { return nil }
        return Double(v)
    }

    static func integer(_ v: String?, max: UInt64) -> UInt64? {
        guard let v = v, !v.isEmpty, v.utf8.count <= 15, v.utf8.allSatisfy({ $0 >= 48 && $0 <= 57 }), let n = UInt64(v), n <= max else { return nil }
        return n
    }

    private static func only(_ p: [String: String], _ allowed: Set<String>) -> Bool { p.keys.allSatisfy { allowed.contains($0) } }

    private static func resize(_ p: [String: String], _ kind: StepKind) -> StepKind? {
        guard only(p, ["scale", "w", "h", "keep"]), let scale = p["scale"] else { return nil }
        if scale == "custom" {
            guard let w = integer(p["w"], max: maxPixels), let h = integer(p["h"], max: maxPixels) else { return nil }
            let keepOK = p["keep"].map { $0 == "0" || $0 == "1" } ?? true
            return keepOK && (w > 0 || h > 0) ? kind : nil
        }
        guard let s = number(scale), p["w"] == nil, p["h"] == nil, p["keep"] == nil else { return nil }
        return s > 0 && s <= 1000 ? kind : nil
    }

    private static func rotate(_ p: [String: String], _ kind: StepKind) -> StepKind? {
        guard only(p, ["deg"]), let d = number(p["deg"]), d >= -360, d <= 360 else { return nil }
        return kind
    }

    private static func to(_ p: [String: String], _ task: String) -> String? {
        guard only(p, ["to"]), let t = p["to"], targets[task]?.contains(t) == true else { return nil }
        return t
    }

    private static func keyOK(_ k: Substring) -> Bool {
        !k.isEmpty && k.utf8.count <= 16 && k.utf8.allSatisfy { ($0 >= 65 && $0 <= 90) || ($0 >= 97 && $0 <= 122) }
    }

    private static func valueOK(_ v: Substring) -> Bool {
        !v.isEmpty && v.utf8.count <= 32 && v.utf8.allSatisfy { ($0 >= 48 && $0 <= 57) || ($0 >= 65 && $0 <= 90) || ($0 >= 97 && $0 <= 122) || $0 == 46 || $0 == 45 }
    }

    /// A step as shown in a message: bounded, without control characters (it may come from an untrusted file).
    static func shown(_ step: String) -> String {
        let scalars = step.unicodeScalars.filter { $0.properties.generalCategory != .control }
        var s = String(String.UnicodeScalarView(scalars.prefix(80)))
        if step.unicodeScalars.count > 80 { s += "…" }
        return s
    }

    // MARK: parsing

    /// One step: `name` or `name?k=v&k=v`, with exactly the parameters its task requires.
    public static func parse(_ step: String) -> Result<PipelineStep, PipelineError> {
        let bad = PipelineError.badStep(shown(step))
        guard !step.isEmpty, step.utf8.count <= maxStepBytes else { return .failure(bad) }
        let parts = step.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false)
        let name = String(parts[0])
        guard let spec = specs.first(where: { $0.0 == name }) else { return .failure(.notAllowed(shown(step))) }
        var params: [String: String] = [:]
        if parts.count == 2 {
            guard !parts[1].isEmpty else { return .failure(bad) }
            for pair in parts[1].split(separator: "&", omittingEmptySubsequences: false) {
                let kv = pair.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
                guard kv.count == 2, keyOK(kv[0]), valueOK(kv[1]), params[String(kv[0])] == nil else { return .failure(bad) }
                params[String(kv[0])] = String(kv[1])
            }
        }
        guard let produces = spec.2(params) else { return .failure(bad) }
        return .success(PipelineStep(task: step, name: name, params: params, accepts: spec.1, produces: produces))
    }

    /// A whole chain: 1...maxSteps valid steps, each output readable by the next step.
    public static func plan(_ steps: [String]) -> Result<[PipelineStep], PipelineError> {
        guard !steps.isEmpty, steps.count <= maxSteps else { return .failure(.stepCount) }
        var parsed: [PipelineStep] = []
        for s in steps {
            switch parse(s) {
            case .success(let p): parsed.append(p)
            case .failure(let e): return .failure(e)
            }
        }
        for (a, b) in zip(parsed, parsed.dropFirst()) where !b.accepts.contains(a.produces) {
            return .failure(.incompatible(shown(a.task), shown(b.task)))
        }
        return .success(parsed)
    }

    public static func isValidStep(_ s: String) -> Bool { if case .success = parse(s) { return true }; return false }
}

// MARK: editing a step

/// The editable values of one step (the preset editor). `build()` always yields a step `Pipeline.parse` accepts
/// when the values are in range; `init?(step:)` reads a saved step back.
public struct StepDraft: Equatable {
    public enum ResizeMode: String { case percent, pixels }

    public var task: String
    public var resizeMode: ResizeMode = .percent
    public var scale: Double = 50
    public var width: Int = 1080
    public var height: Int = 1080
    public var keepAspect = true
    public var degrees: Double = 90
    /// Compress target in MiB.
    public var megabytes: Double = 2
    /// `video.compress?preset=discord`.
    public var discord = false
    /// `to=` for the converting tasks.
    public var target: String

    public init(task: String) {
        self.task = task
        target = Pipeline.targets[task]?.first ?? ""
    }

    public init?(step: String) {
        guard case .success(let s) = Pipeline.parse(step) else { return nil }
        self.init(task: s.name)
        let p = s.params
        if p["scale"] == "custom" {
            resizeMode = .pixels
            width = Int(p["w"] ?? "") ?? 0
            height = Int(p["h"] ?? "") ?? 0
            keepAspect = p["keep"] != "0"
        } else if let v = Pipeline.number(p["scale"]) {
            scale = v
        }
        if let d = Pipeline.number(p["deg"]) { degrees = d }
        if let b = Pipeline.integer(p["bytes"], max: Pipeline.maxBytesParam) { megabytes = Double(b) / 1_048_576 }
        discord = p["preset"] == "discord"
        if let t = p["to"] { target = t }
    }

    /// `12.5`, `50`: at most two decimals, never an exponent.
    static func decimal(_ v: Double) -> String {
        let r = (v * 100).rounded() / 100
        if r == r.rounded() { return String(Int64(r)) }
        var s = String(format: "%.2f", r)
        while s.hasSuffix("0") { s.removeLast() }
        return s
    }

    public var hasParameters: Bool { !["image.strip", "image.chromakey", "json.pretty", "json.sort"].contains(task) }

    public func build() -> String {
        switch task {
        case "image.resize", "video.resize":
            if resizeMode == .pixels {
                let w = min(max(width, 0), Int(Pipeline.maxPixels)), h = min(max(height, 0), Int(Pipeline.maxPixels))
                return "\(task)?scale=custom&w=\(w)&h=\(h)&keep=\(keepAspect ? 1 : 0)"
            }
            return "\(task)?scale=\(StepDraft.decimal(min(max(scale, 0.01), 1000)))"
        case "image.rotate", "video.rotate":
            return "\(task)?deg=\(StepDraft.decimal(min(max(degrees, -360), 360)))"
        case "image.compress", "video.compress":
            if task == "video.compress" && discord { return "video.compress?preset=discord" }
            let bytes = Int64((min(max(megabytes, 0.001), 1_000_000) * 1_048_576).rounded())
            return "\(task)?bytes=\(max(1, bytes))"
        default:
            return Pipeline.targets[task] != nil ? "\(task)?to=\(target)" : task
        }
    }
}

// MARK: describing a step

public enum StepText {
    /// "Resize 50%", "Compress ≤ 2 MB", "Convert to JPG": the menu label plus the parameters that matter.
    public static func describe(_ step: String, loc: Loc) -> String {
        guard case .success(let s) = Pipeline.parse(step) else { return Pipeline.shown(step) }
        let base = loc.has("menu.\(s.name)") ? loc.t("menu.\(s.name)") : s.name
        let p = s.params
        let detail: String?
        if p["scale"] == "custom" { detail = "\(p["w"] ?? "0")×\(p["h"] ?? "0")" }
        else if let v = p["scale"] { detail = "\(v)%" }
        else if let d = p["deg"] { detail = "\(d)°" }
        else if let b = Pipeline.integer(p["bytes"], max: Pipeline.maxBytesParam) { detail = "≤ " + ByteCountFormatter.string(fromByteCount: Int64(b), countStyle: .binary) }
        else if p["preset"] == "discord" { detail = loc.t("menu.video.compress.discord") }
        else if let t = p["to"] { detail = loc.f("menu.to", [t]) }
        else { detail = nil }
        return detail.map { "\(base) \($0)" } ?? base
    }

    public static func describe(steps: [String], loc: Loc) -> String { steps.map { describe($0, loc: loc) }.joined(separator: " → ") }
}
