import Foundation

// Favorites and "Run again" for the action picker. Pure logic plus a small store (`picker.json`, 0600). Everything
// read back from the file is checked again: a `task:` is only ever a catalog id (run through Planner.resolve like a
// Quick Action), a `step:` must pass Pipeline.parse (the engine's allow-list), a `preset:` must name a saved preset.
// The confirmed second pass of a destructive task (`folder.removeDuplicate?mode=all`, `...?confirm=1`) is neither, so
// it is never remembered and can never be replayed by one click.

/// Something the picker can offer again.
public enum PickerRef: Hashable {
    /// A menu catalog id (`image.strip`, `image.resize.p1`, `pdf.convert.PNG`): resolved by Planner when run.
    case task(String)
    /// A concrete engine step (`image.resize?scale=50`) that passes `Pipeline.parse`.
    case step(String)
    case preset(UUID)

    public var raw: String {
        switch self {
        case .task(let id): return "task:\(id)"
        case .step(let s): return "step:\(s)"
        case .preset(let id): return "preset:\(id.uuidString)"
        }
    }

    /// `[A-Za-z0-9.]`, at most 64 bytes, no parameters.
    public static func isCatalogID(_ s: String) -> Bool {
        !s.isEmpty && s.utf8.count <= 64 && !s.hasPrefix(".") && !s.hasSuffix(".")
            && s.utf8.allSatisfy { ($0 >= 48 && $0 <= 57) || ($0 >= 65 && $0 <= 90) || ($0 >= 97 && $0 <= 122) || $0 == 46 }
    }

    public init?(raw: String) {
        if raw.hasPrefix("task:") {
            let id = String(raw.dropFirst(5))
            guard PickerRef.isCatalogID(id) else { return nil }
            self = .task(id)
        } else if raw.hasPrefix("step:") {
            let s = String(raw.dropFirst(5))
            guard Pipeline.isValidStep(s) else { return nil }
            self = .step(s)
        } else if raw.hasPrefix("preset:") {
            let text = String(raw.dropFirst(7))
            guard PresetFile.isUUID(text), let id = UUID(uuidString: text) else { return nil }
            self = .preset(id)
        } else {
            return nil
        }
    }

    /// The task id whose picker section this runs in (`nil` for a preset: see `PickerShortcuts`).
    var taskName: String? {
        switch self {
        case .task(let id): return id
        case .step(let s): return s.split(separator: "?", maxSplits: 1).first.map(String.init)
        case .preset: return nil
        }
    }
}

public struct PickerMemory: Equatable {
    public static let maxFavorites = 6

    /// Pinned to the top of the picker, in order. Only `.task` and `.preset`.
    public private(set) var favorites: [PickerRef] = []
    /// The last operation started, for "Run again".
    public private(set) var last: PickerRef?

    public init(favorites: [PickerRef] = [], last: PickerRef? = nil) {
        for f in favorites { _ = pin(f) }
        self.last = last
    }

    public static func canPin(_ ref: PickerRef) -> Bool { if case .step = ref { return false }; return true }

    public func isPinned(_ ref: PickerRef) -> Bool { favorites.contains(ref) }

    /// Adds at the end. False when it cannot be pinned, is already pinned, or the list is full.
    @discardableResult
    public mutating func pin(_ ref: PickerRef) -> Bool {
        guard PickerMemory.canPin(ref), !favorites.contains(ref), favorites.count < PickerMemory.maxFavorites else { return false }
        favorites.append(ref)
        return true
    }

    public mutating func unpin(_ ref: PickerRef) { favorites.removeAll { $0 == ref } }

    /// Pin if not pinned, otherwise unpin. Returns whether it is pinned afterwards.
    @discardableResult
    public mutating func toggle(_ ref: PickerRef) -> Bool {
        if isPinned(ref) { unpin(ref); return false }
        return pin(ref)
    }

    /// Moves a favorite up (negative) or down (positive), clamped to the list.
    public mutating func move(_ ref: PickerRef, by offset: Int) {
        guard let i = favorites.firstIndex(of: ref) else { return }
        let j = min(max(i + offset, 0), favorites.count - 1)
        favorites.remove(at: i)
        favorites.insert(ref, at: j)
    }

    public mutating func remember(_ ref: PickerRef) { last = ref }

    /// Drops references to presets that no longer exist.
    public mutating func keepPresets(_ ids: Set<UUID>) {
        favorites.removeAll { if case .preset(let id) = $0 { return !ids.contains(id) }; return false }
        if case .preset(let id)? = last, !ids.contains(id) { last = nil }
    }

    // MARK: file form

    public static func decode(_ data: Data) -> PickerMemory {
        guard data.count <= 64 * 1024, let root = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              (root["version"] as? NSNumber)?.intValue == 1 else { return PickerMemory() }
        let favorites = ((root["favorites"] as? [Any]) ?? []).compactMap { ($0 as? String).flatMap(PickerRef.init(raw:)) }
        return PickerMemory(favorites: favorites, last: (root["last"] as? String).flatMap(PickerRef.init(raw:)))
    }

    public func encode() throws -> Data {
        var root: [String: Any] = ["version": 1, "favorites": favorites.map { $0.raw }]
        if let l = last { root["last"] = l.raw }
        return try JSONSerialization.data(withJSONObject: root, options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes])
    }
}

/// `picker.json` next to settings.json. A missing or damaged file reads as empty (it only holds conveniences).
public final class PickerMemoryStore {
    public let url: URL
    public static var defaultURL: URL { SettingsStore.defaultDirectory.appendingPathComponent("picker.json") }

    public init(url: URL = PickerMemoryStore.defaultURL) { self.url = url }

    public func load() -> PickerMemory {
        guard let attrs = try? FileManager.default.attributesOfItem(atPath: url.path), attrs[.type] as? FileAttributeType == .typeRegular,
              let data = try? Data(contentsOf: url) else { return PickerMemory() }
        return PickerMemory.decode(data)
    }

    public func save(_ memory: PickerMemory) throws {
        let fm = FileManager.default
        try fm.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let tmp = url.deletingLastPathComponent().appendingPathComponent(".picker.\(UUID().uuidString).tmp")
        guard fm.createFile(atPath: tmp.path, contents: try memory.encode(), attributes: [.posixPermissions: 0o600]) else { throw CocoaError(.fileWriteUnknown) }
        guard rename(tmp.path, url.path) == 0 else { try? fm.removeItem(at: tmp); throw CocoaError(.fileWriteUnknown) }
    }
}

// MARK: what to show for a selection

/// One row the picker shows above the normal list.
public struct PickerShortcut: Equatable {
    public let ref: PickerRef
    /// Where it runs: its paths are the ones passed to the task.
    public let section: TaskSection
    public let preset: Preset?
}

public enum PickerShortcuts {
    /// The category of a task id (`json.*` is text).
    static func category(of task: String) -> SelectionCategory? {
        let group = task.split(separator: ".").first.map(String.init) ?? task
        return group == "json" ? .text : SelectionCategory(rawValue: group)
    }

    /// The section of `selection` where `task` (a catalog id or a step's task id) is on offer: same category, and the
    /// section lists it or its group. This reuses the picker's rules (single file, all JSON, not an animated GIF...).
    public static func section(for task: String, in selection: PickerSelection) -> TaskSection? {
        guard let cat = category(of: task) else { return nil }
        return selection.sections.first { s in
            s.category == cat && s.entries.contains { $0.id == task || task.hasPrefix($0.id + ".") }
        }
    }

    /// Where a preset can run: its first step must read the section's kind and be on offer there.
    public static func section(for preset: Preset, in selection: PickerSelection) -> TaskSection? {
        guard case .success(let steps) = Pipeline.plan(preset.steps), let first = steps.first,
              let s = section(for: first.name, in: selection), let kind = StepKind(category: s.category), first.accepts.contains(kind) else { return nil }
        return s
    }

    public static func shortcut(_ ref: PickerRef, presets: [Preset], selection: PickerSelection) -> PickerShortcut? {
        switch ref {
        case .preset(let id):
            guard let p = presets.first(where: { $0.id == id }), let s = section(for: p, in: selection) else { return nil }
            return PickerShortcut(ref: ref, section: s, preset: p)
        case .task, .step:
            guard let name = ref.taskName, let s = section(for: name, in: selection) else { return nil }
            return PickerShortcut(ref: ref, section: s, preset: nil)
        }
    }

    /// Pinned items that apply to this selection, in pinned order.
    public static func favorites(_ memory: PickerMemory, presets: [Preset], selection: PickerSelection) -> [PickerShortcut] {
        memory.favorites.compactMap { shortcut($0, presets: presets, selection: selection) }
    }

    /// "Run again": the last operation, when it applies to this selection.
    public static func runAgain(_ memory: PickerMemory, presets: [Preset], selection: PickerSelection) -> PickerShortcut? {
        memory.last.flatMap { shortcut($0, presets: presets, selection: selection) }
    }

    /// Saved presets that can run on this selection, in saved order.
    public static func presets(_ presets: [Preset], selection: PickerSelection) -> [PickerShortcut] {
        presets.compactMap { p in section(for: p, in: selection).map { PickerShortcut(ref: .preset(p.id), section: $0, preset: p) } }
    }
}
