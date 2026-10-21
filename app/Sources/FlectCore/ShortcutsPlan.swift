import Foundation

// Everything the Shortcuts (App Intents) front end decides, kept free of the AppIntents framework so it can be
// checked on its own. An intent is one more entry point like the Dock drop or the Services menu: its files go through
// the same path rules (`EntryRequest` / `LaunchRequest.validate`), its task goes through `Planner.resolve`, and the
// engine runs it in a separate process. Nothing a Shortcut passes in is trusted: files must exist, be plain paths and
// be of a kind the chosen task accepts (`TaskPicker`), and the task must come from the engine's own catalog.

public enum ShortcutsError: Error, Equatable {
    case engineMissing
    case noFiles
    case tooManyFiles
    case badFile(String)
    case unknownTask(String)
    case notRunnable(String)
    case fileNotApplicable(String)
    case invalidOption(String)
    case engineFailed(String?)
    case cancelled
}

extension ShortcutsError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .engineMissing: return "The Flect engine was not found inside the app."
        case .noFiles: return "No files were passed to Flect."
        case .tooManyFiles: return "Too many files for one run."
        case .badFile(let name): return "\"\(name)\" is not a usable file (it must be an existing file with an absolute path)."
        case .unknownTask(let id): return "Flect has no task \"\(id)\"."
        case .notRunnable(let id): return "The task \"\(id)\" cannot run from Shortcuts (it needs a dialog or a choice in the Flect window)."
        case .fileNotApplicable(let name): return "This task does not apply to \"\(name)\"."
        case .invalidOption(let what): return "Invalid option: \(what)."
        case .engineFailed(let summary): return summary ?? "Flect could not finish the task."
        case .cancelled: return "The run was cancelled."
        }
    }
}

/// One task a Shortcut can run: a leaf of the engine's menu that `Planner.resolve` turns into a concrete engine task.
public struct ShortcutTask: Equatable {
    public let id: String          // catalog id, e.g. "image.resize.p1"
    public let engineTask: String  // what the engine will be asked, e.g. "image.resize?scale=50"
    public let label: String
}

/// What the intent wants; resolved against the catalog by `ShortcutsPlan.plan`.
public enum ShortcutsSpec: Equatable {
    case catalogTask(id: String)
    /// Image resize by percent, or by pixels (0 = free) when width or height is given.
    case resize(percent: Int?, width: Int?, height: Int?, keepAspect: Bool)
    case convertImage(format: String)
    case stripMetadata
}

public struct ShortcutsPlanResult: Equatable {
    /// The catalog entry that decides which files the task applies to.
    public let catalogID: String
    public let engineTask: String
}

public enum ShortcutsPlan {
    /// Parts of the menu a Shortcut may run. Folder tasks (which can ask for the Trash), PDF and JAR are left out on
    /// purpose; nothing here deletes or moves originals.
    public static let allowedRoots: Set<String> = ["image", "video", "audio", "text", "archive"]
    /// Image formats the engine converts to by itself (HEIC is an OS feature and needs the app window).
    public static let imageFormats = ["PNG", "JPG", "WEBP", "ICO", "GIF", "SVG"]

    public static let maxPixels = 32_768
    public static let maxPercent = 1_000
    public static let maxFileBytes = 1 << 30

    // MARK: catalog

    /// root > group > (leaf) of a catalog id; a group without children is its own leaf (`image.strip`).
    public static func locate(_ id: String, in catalog: MenuCatalog) -> (root: MenuEntry, group: MenuEntry, leaf: MenuEntry)? {
        for root in catalog.entries {
            for group in root.children {
                if group.id == id { return (root, group, group) }
                if let leaf = group.children.first(where: { $0.id == id }) { return (root, group, leaf) }
            }
        }
        return nil
    }

    /// The tasks offered in the Shortcuts action's list: only leaves that resolve to a plain engine task without
    /// further input (presets read their value from the settings, conversions carry their target).
    public static func tasks(in catalog: MenuCatalog, settings: SettingsStore, loc: Loc) -> [ShortcutTask] {
        func name(_ e: MenuEntry) -> String { loc.has(e.labelKey) ? loc.t(e.labelKey) : e.label }
        var result: [ShortcutTask] = []
        for root in catalog.entries where allowedRoots.contains(root.id) {
            for group in root.children {
                let leaves = group.children.isEmpty ? [group] : group.children
                for leaf in leaves {
                    guard case .engine(let task) = Planner.resolve(leaf.id, settings: settings), isAllowed(task: task, catalog: catalog) else { continue }
                    let label = leaf.id == group.id
                        ? "\(name(root)): \(name(group))"
                        : "\(name(root)): \(name(group)) - \(Planner.label(for: leaf, settings: settings, loc: loc))"
                    result.append(ShortcutTask(id: leaf.id, engineTask: task, label: label))
                }
            }
        }
        return result
    }

    /// The engine task must belong to an allowed part of the catalog, whatever produced the string.
    static func isAllowed(task: String, catalog: MenuCatalog) -> Bool {
        let base = task.split(separator: "?", maxSplits: 1).first.map(String.init) ?? task
        guard let root = base.split(separator: ".").first, allowedRoots.contains(String(root)) else { return false }
        return catalog.find(base) != nil
    }

    // MARK: planning

    public static func plan(_ spec: ShortcutsSpec, catalog: MenuCatalog, settings: SettingsStore) -> Result<ShortcutsPlanResult, ShortcutsError> {
        let catalogID: String
        let proposed: String?   // a task string built here from validated numbers; nil = take what Planner.resolve says
        switch spec {
        case .catalogTask(let id):
            catalogID = id; proposed = nil
        case .stripMetadata:
            catalogID = "image.strip"; proposed = nil
        case .convertImage(let format):
            guard imageFormats.contains(format) else { return .failure(.invalidOption("format \(format)")) }
            catalogID = "image.convert.\(format)"; proposed = nil
        case .resize(let percent, let width, let height, let keep):
            catalogID = "image.resize.custom"
            if width != nil || height != nil {
                let w = width ?? 0, h = height ?? 0
                guard (0...maxPixels).contains(w), (0...maxPixels).contains(h), w > 0 || h > 0 else { return .failure(.invalidOption("width/height must be 1-\(maxPixels) pixels")) }
                proposed = Planner.resizeCustom(base: "image.resize", width: w, height: h, keepAspect: keep)
            } else {
                let p = percent ?? 50
                guard (1...maxPercent).contains(p) else { return .failure(.invalidOption("percent must be 1-\(maxPercent)")) }
                proposed = "image.resize?scale=\(p)"
            }
        }
        guard let found = locate(catalogID, in: catalog), allowedRoots.contains(found.root.id) else {
            return .failure(allowedRootsContain(catalogID) ? .unknownTask(catalogID) : .notRunnable(catalogID))
        }
        let task: String
        if let proposed = proposed {
            // Planner.resolve passes a parameterised id through unchanged; it still has to name an allowed task.
            guard case .engine(let t) = Planner.resolve(proposed, settings: settings), isAllowed(task: t, catalog: catalog) else { return .failure(.notRunnable(catalogID)) }
            task = t
        } else {
            guard case .engine(let t) = Planner.resolve(catalogID, settings: settings), isAllowed(task: t, catalog: catalog) else { return .failure(.notRunnable(catalogID)) }
            task = t
        }
        return .success(ShortcutsPlanResult(catalogID: catalogID, engineTask: task))
    }

    private static func allowedRootsContain(_ id: String) -> Bool {
        id.split(separator: ".").first.map { allowedRoots.contains(String($0)) } ?? false
    }

    // MARK: files

    /// Same rules as every other entry point, but strict: one unusable file fails the whole run (a Shortcut gets
    /// no dialog to explain a silent skip).
    public static func items(from urls: [URL], fm: FileManager = .default) -> Result<[SelectionItem], ShortcutsError> {
        if urls.isEmpty { return .failure(.noFiles) }
        switch EntryRequest.items(from: urls, fm: fm) {
        case .failure(let f):
            switch f {
            case .tooMany: return .failure(.tooManyFiles)
            case .empty: return .failure(.noFiles)
            case .badPath(let p): return .failure(.badFile(p))
            default: return .failure(.noFiles)
            }
        case .success(let r):
            if r.rejected > 0 { return .failure(.badFile(urls.first { !$0.isFileURL }.map { $0.absoluteString.prefix(80).description } ?? "")) }
            return .success(r.items)
        }
    }

    /// Every file must be of a kind the chosen task is offered for (the same decision as the action picker).
    public static func check(items: [SelectionItem], catalogID: String, catalog: MenuCatalog, isAnimatedGIF: (String) -> Bool = { _ in false }) -> ShortcutsError? {
        guard let found = locate(catalogID, in: catalog) else { return .unknownTask(catalogID) }
        let selection = TaskPicker.selection(for: items, catalog: catalog, isAnimatedGIF: isAnimatedGIF)
        let ok = selection.unsupported == 0
            && selection.sections.count == 1
            && selection.sections[0].category.rawValue == found.root.id
            && selection.sections[0].entries.contains { $0.id == found.group.id }
            && selection.sections[0].paths.count == items.count
        if ok { return nil }
        let covered = Set(selection.sections.filter { $0.category.rawValue == found.root.id && $0.entries.contains { $0.id == found.group.id } }.flatMap { $0.paths })
        let bad = items.first { !covered.contains($0.path) }
        return .fileNotApplicable(((bad?.path ?? items.first?.path ?? "") as NSString).lastPathComponent)
    }

    // MARK: staging files that arrive as data

    /// A Shortcut may hand over file contents without a path. They are written to a fresh private folder under a
    /// harmless name; results then land beside them like for any other file.
    public static func stage(name: String, data: Data, index: Int, in root: URL, fm: FileManager = .default) throws -> URL {
        guard data.count <= maxFileBytes else { throw ShortcutsError.badFile(name) }
        try fm.createDirectory(at: root, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let dir = root.appendingPathComponent(String(index), isDirectory: true)
        try fm.createDirectory(at: dir, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        let url = dir.appendingPathComponent(sanitizedName(name))
        guard fm.createFile(atPath: url.path, contents: data, attributes: [.posixPermissions: 0o600]) else { throw ShortcutsError.badFile(name) }
        return url
    }

    /// A plain file name: no folders, no hidden-file or `..` tricks, bounded length, never empty.
    public static func sanitizedName(_ raw: String) -> String {
        var s = (raw as NSString).lastPathComponent
        s = s.replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "\0", with: "")
        s = s.trimmingCharacters(in: .whitespacesAndNewlines)
        while s.hasPrefix(".") { s.removeFirst() }
        if s.isEmpty { s = "file" }
        if s.utf8.count > 200 {
            let ext = (s as NSString).pathExtension
            let stem = String((s as NSString).deletingPathExtension.prefix(100))
            s = ext.isEmpty || ext.utf8.count > 20 ? stem : stem + "." + ext
        }
        return s
    }
}

// MARK: running

public struct ShortcutRunResult: Equatable {
    public let outcome: Outcome
    /// Only files this run created and the engine reported (`data: outputs`), checked by `RunRecorder`.
    public let outputs: [String]
    public let summary: String?
    public let sawProblem: Bool
}

public enum HeadlessRunner {
    /// Runs one engine task to its end. A `needs: trash` request is ignored: nothing in this path moves or deletes files.
    public static func run(engine: EngineClient, task: String, inputs: [String], loc: Loc = .shared) async -> ShortcutRunResult {
        let startedAt = Date()
        var reported: [String] = []
        var outcome = Outcome.failed
        var summary: String?
        var problem = false
        for await event in engine.run(task: task, paths: inputs) {
            switch event {
            case .data(let name, let d) where name == "outputs":
                reported += (d["paths"]?.array ?? []).compactMap { $0.string }
            case .warn, .error: problem = true
            case .done(let o, let key, let args):
                outcome = o
                summary = key.map { loc.f($0, args) }
            case .crashed:
                problem = true
            default: break
            }
        }
        let record = RunRecorder.record(task: task, outcome: outcome.rawValue, startedAt: startedAt, finishedAt: Date(),
                                        inputs: inputs, reportedOutputs: reported, summary: summary)
        return ShortcutRunResult(outcome: outcome, outputs: record.outputs.map { $0.path }, summary: summary, sawProblem: problem)
    }
}
