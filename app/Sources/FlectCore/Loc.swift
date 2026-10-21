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
        let b = bundle ?? Bundle.module
        self.english = Loc.load("en", bundle: b)
        self.table = language == .english ? english : Loc.load(language.rawValue, bundle: b)
    }

    public func use(_ language: AppLanguage) {
        self.language = language
        let b = Bundle.module
        table = language == .english ? english : Loc.load(language.rawValue, bundle: b)
    }

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
