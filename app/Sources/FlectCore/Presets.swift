import Foundation

// User presets: named step lists ("SNS 1080px / 2MB" = resize + compress), stored in `presets.json` next to
// settings.json. The file is read as untrusted input with the same rules as core/flect-core/src/presets.rs
// (both are tested against core/flect-core/tests/fixtures/presets/). One bad entry makes the whole file invalid:
// reading the good part and saving it would silently drop the rest. An invalid file is never overwritten in place.

public struct Preset: Equatable, Identifiable {
    public var id: UUID
    public var name: String
    /// 1...Pipeline.maxSteps steps that pass `Pipeline.plan`.
    public var steps: [String]

    public init(id: UUID = UUID(), name: String, steps: [String]) {
        self.id = id; self.name = name; self.steps = steps
    }
}

public enum PresetFile {
    public static let schemaVersion = 1
    public static let maxBytes = 256 * 1024
    public static let maxPresets = 100
    public static let maxNameLength = 64

    public enum Failure: Error, Equatable {
        case tooLarge
        /// Written by a newer version of Flect: left alone, and the app will not save over it.
        case newer(Int)
        case invalid(String)
    }

    /// Canonical 8-4-4-4-12 hex UUID (either case).
    public static func isUUID(_ s: String) -> Bool {
        let b = Array(s.utf8)
        guard b.count == 36 else { return false }
        for (i, c) in b.enumerated() {
            if [8, 13, 18, 23].contains(i) { if c != 45 { return false } }
            else if !((c >= 48 && c <= 57) || (c >= 65 && c <= 70) || (c >= 97 && c <= 102)) { return false }
        }
        return true
    }

    /// No padding, bounded, and nothing invisible or direction-changing that could make two names look alike.
    public static func isValidName(_ name: String) -> Bool {
        let scalars = name.unicodeScalars
        guard (1...maxNameLength).contains(scalars.count), name.trimmingCharacters(in: .whitespacesAndNewlines) == name else { return false }
        return !scalars.contains { s in
            s.properties.generalCategory == .control
                || (0x200B...0x200F).contains(s.value) || (0x202A...0x202E).contains(s.value)
                || (0x2060...0x2069).contains(s.value) || s.value == 0xFEFF
        }
    }

    /// Integer JSON numbers only (`1`, not `1.0` or `true`).
    private static func integer(_ v: Any?) -> Int? {
        guard let n = v as? NSNumber, CFGetTypeID(n) != CFBooleanGetTypeID(), !CFNumberIsFloatType(n as CFNumber) else { return nil }
        return n.intValue
    }

    /// The rules for a list about to be saved (also applied to every loaded file).
    public static func problem(in presets: [Preset]) -> String? {
        if presets.count > maxPresets { return "too many presets" }
        var ids = Set<UUID>(), names = Set<String>()
        for (i, p) in presets.enumerated() {
            if !ids.insert(p.id).inserted { return "preset \(i + 1): duplicate id" }
            if !isValidName(p.name) || !names.insert(p.name.lowercased()).inserted { return "preset \(i + 1): bad or duplicate name" }
            if case .failure(let e) = Pipeline.plan(p.steps) { return "preset \(i + 1): \(e.key) \(e.args)" }
        }
        return nil
    }

    public static func decode(_ data: Data) -> Result<[Preset], Failure> {
        guard data.count <= maxBytes else { return .failure(.tooLarge) }
        // UTF-8 without a BOM, as the engine reads it (JSONSerialization would also take UTF-16/32).
        guard !data.starts(with: [0xEF, 0xBB, 0xBF]), String(data: data, encoding: .utf8) != nil else { return .failure(.invalid("not UTF-8")) }
        guard let root = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { return .failure(.invalid("not a JSON object")) }
        // The version decides how to read the rest, so it is looked at first.
        guard let version = integer(root["version"]), version >= 0 else { return .failure(.invalid("missing or bad version")) }
        if version > schemaVersion { return .failure(.newer(version)) }
        guard version == schemaVersion else { return .failure(.invalid("unsupported version")) }
        guard Set(root.keys) == ["version", "presets"], let list = root["presets"] as? [Any] else { return .failure(.invalid("unexpected fields")) }
        guard list.count <= maxPresets else { return .failure(.invalid("too many presets")) }
        var out: [Preset] = []
        for (i, item) in list.enumerated() {
            guard let d = item as? [String: Any], Set(d.keys) == ["id", "name", "steps"],
                  let idText = d["id"] as? String, isUUID(idText), let id = UUID(uuidString: idText),
                  let name = d["name"] as? String, let steps = d["steps"] as? [String] else {
                return .failure(.invalid("preset \(i + 1): bad fields"))
            }
            out.append(Preset(id: id, name: name, steps: steps))
        }
        if let p = problem(in: out) { return .failure(.invalid(p)) }
        return .success(out)
    }

    public static func encode(_ presets: [Preset]) throws -> Data {
        let list: [[String: Any]] = presets.map { ["id": $0.id.uuidString, "name": $0.name, "steps": $0.steps] }
        return try JSONSerialization.data(withJSONObject: ["version": schemaVersion, "presets": list], options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes])
    }
}

/// `presets.json` in the data folder (FLECT_HOME aware). The app is its only writer; the CLI only reads it.
public final class PresetStore {
    public enum State: Equatable {
        case ok
        case missing
        /// Unreadable or invalid: shown as empty; kept as `presets.json.broken` on the next save, never overwritten.
        case broken(String)
        /// From a newer version: read-only.
        case newer(Int)
    }

    public enum SaveError: Error, Equatable {
        case readOnly
        case invalid(String)
    }

    public static let fileName = "presets.json"
    public let directory: URL
    public var url: URL { directory.appendingPathComponent(PresetStore.fileName) }
    public private(set) var presets: [Preset] = []
    public private(set) var state: State = .missing

    public var isReadOnly: Bool { if case .newer = state { return true }; return false }

    public init(directory: URL = SettingsStore.defaultDirectory) {
        self.directory = directory
        reload()
    }

    public func preset(_ id: UUID) -> Preset? { presets.first { $0.id == id } }

    public func reload() {
        presets = []
        let fm = FileManager.default
        // Checked before opening: a FIFO planted under the name would block the read.
        let resolved = (url.path as NSString).resolvingSymlinksInPath
        guard let attrs = try? fm.attributesOfItem(atPath: resolved) else {
            state = fm.fileExists(atPath: url.path) || (try? fm.destinationOfSymbolicLink(atPath: url.path)) != nil ? .broken("unreadable") : .missing
            return
        }
        guard attrs[.type] as? FileAttributeType == .typeRegular else { state = .broken("not a regular file"); return }
        guard ((attrs[.size] as? NSNumber)?.intValue ?? 0) <= PresetFile.maxBytes else { state = .broken("too large"); return }
        guard let handle = FileHandle(forReadingAtPath: resolved) else { state = .broken("unreadable"); return }
        defer { try? handle.close() }
        let data = (try? handle.read(upToCount: PresetFile.maxBytes + 1)) ?? Data()
        switch PresetFile.decode(data) {
        case .success(let p): presets = p; state = .ok
        case .failure(.newer(let v)): state = .newer(v)
        case .failure(.tooLarge): state = .broken("too large")
        case .failure(.invalid(let why)): state = .broken(why)
        }
    }

    /// Writes the whole list atomically with owner-only permissions. A broken file is first renamed aside (never
    /// deleted); a newer one is never touched.
    public func save(_ list: [Preset], now: Date = Date()) throws {
        if isReadOnly { throw SaveError.readOnly }
        if let p = PresetFile.problem(in: list) { throw SaveError.invalid(p) }
        let fm = FileManager.default
        try fm.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        if case .broken = state, (try? fm.attributesOfItem(atPath: url.path)) != nil {
            try fm.moveItem(at: url, to: PresetStore.asideURL(for: url, now: now, fm: fm))
        }
        let data = try PresetFile.encode(list)
        let tmp = directory.appendingPathComponent(".presets.\(UUID().uuidString).tmp")
        guard fm.createFile(atPath: tmp.path, contents: data, attributes: [.posixPermissions: 0o600]) else { throw CocoaError(.fileWriteUnknown) }
        // rename(2): atomic, and replaces a symlink at the name rather than writing through it.
        guard rename(tmp.path, url.path) == 0 else {
            try? fm.removeItem(at: tmp)
            throw CocoaError(.fileWriteUnknown)
        }
        presets = list
        state = .ok
    }

    /// `presets.json.broken`, or `presets.json.broken-<date>` (then `-<date>-<n>`) when that is taken.
    static func asideURL(for url: URL, now: Date, fm: FileManager) -> URL {
        let first = URL(fileURLWithPath: url.path + ".broken")
        if (try? fm.attributesOfItem(atPath: first.path)) == nil && (try? fm.destinationOfSymbolicLink(atPath: first.path)) == nil { return first }
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyyMMdd-HHmmss"
        let stamp = url.path + ".broken-" + f.string(from: now)
        var candidate = URL(fileURLWithPath: stamp)
        var n = 2
        while (try? fm.attributesOfItem(atPath: candidate.path)) != nil {
            candidate = URL(fileURLWithPath: "\(stamp)-\(n)")
            n += 1
        }
        return candidate
    }
}
