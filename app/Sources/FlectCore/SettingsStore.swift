import Foundation

/// `settings.json`, edited as a generic JSON tree so keys this app does not know about survive a save
/// (the file is shared with the engine and may come from the Windows version).
public final class SettingsStore {
    public private(set) var root: [String: Any]
    public let directory: URL

    public static var defaultDirectory: URL {
        if let home = ProcessInfo.processInfo.environment["FLECT_HOME"], !home.isEmpty { return URL(fileURLWithPath: home) }
        return FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support/Flect-for-Mac")
    }

    public init(directory: URL = SettingsStore.defaultDirectory) {
        self.directory = directory
        let url = directory.appendingPathComponent("settings.json")
        if let data = try? Data(contentsOf: url), data.count < 4 << 20 {
            let trimmed = data.starts(with: [0xEF, 0xBB, 0xBF]) ? data.dropFirst(3) : data[...]
            root = (try? JSONSerialization.jsonObject(with: Data(trimmed)) as? [String: Any]) ?? [:]
        } else {
            root = [:]
        }
    }

    // MARK: access by path, e.g. get("Image", "Presets", "ResizeA")

    public func get(_ path: String...) -> Any? { get(path: path) }

    public func get(path: [String]) -> Any? {
        var cur: Any? = root
        for key in path {
            guard let d = cur as? [String: Any] else { return nil }
            cur = d[key]
        }
        return cur is NSNull ? nil : cur
    }

    public func string(_ path: String..., default def: String = "") -> String { (get(path: path) as? String) ?? def }
    public func bool(_ path: String..., default def: Bool) -> Bool { (get(path: path) as? Bool) ?? def }
    public func double(_ path: String..., default def: Double) -> Double { (get(path: path) as? NSNumber)?.doubleValue ?? def }
    public func int(_ path: String..., default def: Int) -> Int { (get(path: path) as? NSNumber)?.intValue ?? def }
    public func int64(_ path: String..., default def: Int64) -> Int64 { (get(path: path) as? NSNumber)?.int64Value ?? def }
    public func strings(_ path: String...) -> [String] { (get(path: path) as? [String]) ?? [] }

    public func set(_ path: [String], _ value: Any) {
        guard let first = path.first else { return }
        if path.count == 1 { root[first] = value; return }
        root[first] = SettingsStore.setting(root[first] as? [String: Any] ?? [:], Array(path.dropFirst()), value)
    }

    private static func setting(_ dict: [String: Any], _ path: [String], _ value: Any) -> [String: Any] {
        var d = dict
        guard let first = path.first else { return d }
        if path.count == 1 { d[first] = value; return d }
        d[first] = setting(d[first] as? [String: Any] ?? [:], Array(path.dropFirst()), value)
        return d
    }

    /// Atomic write with owner-only permissions (the file can name folders and tools).
    public func save() throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        root["Version"] = 1
        let data = try JSONSerialization.data(withJSONObject: root, options: [.prettyPrinted, .sortedKeys])
        let tmp = directory.appendingPathComponent(".settings.\(UUID().uuidString).tmp")
        guard FileManager.default.createFile(atPath: tmp.path, contents: data, attributes: [.posixPermissions: 0o600]) else { throw CocoaError(.fileWriteUnknown) }
        _ = try FileManager.default.replaceItemAt(directory.appendingPathComponent("settings.json"), withItemAt: tmp)
    }

    // MARK: values the app needs (defaults match the engine / the Windows version)

    public var language: AppLanguage { AppLanguage.from(setting: string("General", "Language", default: "System")) }
    public var deletePermanently: Bool { string("Folder", "DeleteMethod", default: "RecycleBin") == "Permanent" }
    public var hiddenMenuItems: Set<String> { Set(strings("General", "HiddenMenuItems")) }
    public var closeOnSuccess: Bool { bool("General", "CloseOnSuccess", default: true) }
    public var closeOnFailure: Bool { bool("General", "CloseOnFailure", default: false) }
    public var closeOnCancel: Bool { bool("General", "CloseOnCancel", default: true) }
}
