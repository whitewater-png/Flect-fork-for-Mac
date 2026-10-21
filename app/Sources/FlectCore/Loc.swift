import Foundation

public enum AppLanguage: String {
    case english = "en", japanese = "ja", chinese = "zh"

    /// `Language` value of settings.json: System, English, Japanese, ChineseSimplified.
    public static func from(setting: String?, preferred: [String] = Locale.preferredLanguages) -> AppLanguage {
        switch setting {
        case "English": return .english
        case "Japanese": return .japanese
        case "ChineseSimplified": return .chinese
        default:
            let first = preferred.first ?? "en"
            if first.hasPrefix("ja") { return .japanese }
            if first.hasPrefix("zh") { return .chinese }
            return .english
        }
    }
}

/// Message lookup. Tables are JSON files shipped as resources (migrated from the Windows version).
public final class Loc {
    public static let shared = Loc(language: .english)

    public private(set) var language: AppLanguage
    private var table: [String: String]
    private var english: [String: String]

    public init(language: AppLanguage, bundle: Bundle? = nil) {
        self.language = language
        let b = bundle ?? Loc.resourceBundle
        self.english = Loc.load("en", bundle: b)
        self.table = language == .english ? english : Loc.load(language.rawValue, bundle: b)
    }

    public func use(_ language: AppLanguage) {
        self.language = language
        let b = Loc.resourceBundle
        table = language == .english ? english : Loc.load(language.rawValue, bundle: b)
    }

    /// The SwiftPM resource bundle. In a packaged app it sits in Contents/Resources, where the generated
    /// `Bundle.module` accessor of older toolchains does not look: it checks the .app root and the absolute
    /// build path, and traps when neither exists (a zip built on CI and unpacked elsewhere crashed at launch).
    /// `Bundle.module` is therefore touched only as the fallback for `swift run` and tests.
    static let resourceBundle: Bundle = {
        let name = "FlectForMac_FlectCore.bundle"
        let roots = [Bundle.main.resourceURL, Bundle.main.bundleURL, Bundle.main.executableURL?.deletingLastPathComponent()]
        for root in roots.compactMap({ $0 }) {
            if let b = Bundle(url: root.appendingPathComponent(name)) { return b }
        }
        return Bundle.module
    }()

    private static func load(_ code: String, bundle: Bundle) -> [String: String] {
        guard let url = bundle.url(forResource: "strings.\(code)", withExtension: "json", subdirectory: "Resources") ?? bundle.url(forResource: "strings.\(code)", withExtension: "json"),
              let data = try? Data(contentsOf: url),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: String] else { return [:] }
        return obj
    }

    public func has(_ key: String) -> Bool { table[key] != nil || english[key] != nil }

    public func t(_ key: String) -> String { table[key] ?? english[key] ?? key }

    /// `{0}`, `{1}` placeholders.
    public func f(_ key: String, _ args: [String]) -> String { Loc.format(t(key), args) }

    /// Count with singular form (`key.one`).
    public func n(_ key: String, _ count: Int) -> String {
        let template = (count == 1 ? (table[key + ".one"] ?? english[key + ".one"]) : nil) ?? t(key)
        return Loc.format(template, [String(count)])
    }

    public static func format(_ template: String, _ args: [String]) -> String {
        var out = template
        for (i, a) in args.enumerated() { out = out.replacingOccurrences(of: "{\(i)}", with: a) }
        return out
    }
}
