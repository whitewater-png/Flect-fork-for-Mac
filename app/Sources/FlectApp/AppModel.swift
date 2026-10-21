import AppKit
import SwiftUI
import FlectCore

enum Screen {
    case idle
    case message(String)
    case running
    case choose(title: String, entries: [MenuEntry])
    case dialog(Dialog)
    case tree(String)
    case analyze(JSONValue)
    case duplicates([[DuplicateFile]])
    case duplicatesPreview(DuplicatePreview)
    case privacyReport([PrivacyReport], errors: [String])
    case emptyFolders(targets: [String], permanent: Bool)
    case renamePreview([String])
    case pdfPreview(URL)
    case settings
    case pick(PickerSelection)
}

struct DuplicateFile: Identifiable {
    let id = UUID()
    let path: String
    let size: Int64
    let modified: String
}

/// The dry run of a duplicate removal: per group the copy that stays and the copies that would go.
struct DuplicatePreview {
    struct Group: Identifiable {
        let id = UUID()
        let keep: DuplicateFile?
        let remove: [DuplicateFile]
    }
    let mode: String            // oldest / newest / all: sent back unchanged to confirm
    let groups: [Group]
    let removeCount: Int
    let reclaimBytes: Int64
    let permanent: Bool
}

/// What one image carries that identifies a place, a device or a person (engine `privacy_report`).
struct PrivacyReport: Identifiable {
    struct Item: Identifiable {
        let id = UUID()
        let kind: String
        let high: Bool
        let detail: String
    }
    let id = UUID()
    let file: String
    let format: String
    let items: [Item]
}

struct LogLine: Identifiable {
    enum Kind { case info, warn, error }
    let id = UUID()
    let kind: Kind
    let text: String
}

@MainActor
final class AppModel: ObservableObject {
    @Published var screen: Screen = .idle
    @Published var status = ""
    @Published var done = 0
    @Published var total = 0
    @Published var filePermille = 0
    @Published var log: [LogLine] = []
    @Published var finished = false
    @Published var finishedText = ""
    @Published var cancelling = false

    var settings = SettingsStore()
    var loc: Loc { Loc.shared }
    var paths: [String] = []
    var engine: EngineClient?
    var catalog: MenuCatalog?
    private var pendingData: [(String, JSONValue)] = []
    private var renameFields: (String, String, String, Bool)?
    // Entry points (Dock drop, Open..., Services), results and undo: see EntryPoints.swift / RunResults.swift
    let tracker = RunTracker()
    let historyStore = RunHistoryStore()
    @Published var history: [RunRecord] = []
    @Published var currentRecordID: UUID?
    @Published var historyFocus: UUID?
    var startedInSettings = false
    var returnScreen: Screen?
    var autoClose: DispatchWorkItem?
    // Presets, favorites and "Run again": see PresetsView.swift
    let presetStore = PresetStore()
    let pickerStore = PickerMemoryStore()
    @Published var presets: [Preset] = []
    @Published var memory = PickerMemory()

    // MARK: start

    func start(arguments: [String]) {
        settings = SettingsStore()
        Loc.shared.use(settings.language)
        engine = EngineClient.locate().map { EngineClient(executable: $0) }
        history = historyStore.load()
        reloadPresets()
        startedInSettings = !arguments.contains("--task")
        guard arguments.contains("--task") else { screen = .settings; return }
        switch LaunchRequest.parse(arguments: arguments) {
        case .failure:
            screen = .message(loc.t("app.noTask"))
        case .success(let req):
            paths = req.paths
            handle(task: req.task)
        }
    }

    func handle(task: String) {
        if task.hasPrefix("preset:") { runPreset(ref: task); return } // `--task preset:<UUID>` from a Quick Action
        switch Planner.resolve(task, settings: settings) {
        case .engine(let t): runEngine(t, paths: paths)
        case .choose(let group): chooseFrom(group)
        case .dialog(let d): screen = .dialog(d)
        case .native(let n): remember(task: task); runNative(n)
        case .unavailable(let key): screen = .message(loc.t(key))
        case .unknown: screen = .message(loc.f("msg.unknownCommand", [task]))
        }
    }

    private func chooseFrom(_ group: String) {
        guard let engine = engine else { screen = .message(loc.t("app.engineMissing")); return }
        Task {
            if catalog == nil, let m = await engine.fetchMenu() { catalog = MenuCatalog(menu: m) }
            guard let entry = catalog?.find(group) else { screen = .message(loc.f("msg.unknownCommand", [group])); return }
            let hidden = settings.hiddenMenuItems
            screen = .choose(title: loc.has(entry.labelKey) ? loc.t(entry.labelKey) : entry.label, entries: entry.children.filter { !hidden.contains($0.id) })
        }
    }

    /// Every menu entry that does something (for the "which items to show" list in settings).
    func menuLeaves() async -> [MenuEntry] {
        guard let engine = engine else { return [] }
        if catalog == nil, let m = await engine.fetchMenu() { catalog = MenuCatalog(menu: m) }
        func walk(_ es: [MenuEntry]) -> [MenuEntry] { es.flatMap { $0.children.isEmpty ? [$0] : walk($0.children) } }
        return walk(catalog?.entries ?? [])
    }

    func label(for entry: MenuEntry) -> String { Planner.label(for: entry, settings: settings, loc: loc) }

    // MARK: engine runs

    /// `steps`: run them as one pipeline instead of `task`. `label`: what the run history shows (a preset's name).
    /// `remembering`: what "Run again" should repeat, when it is not `task` itself (a preset).
    func runEngine(_ task: String, paths runPaths: [String], steps: [String]? = nil, label: String? = nil, remembering: PickerRef? = nil) {
        guard let engine = engine else { screen = .message(loc.t("app.engineMissing")); return }
        resetProgress()
        screen = .running
        pendingData = []
        tracker.begin(task: label ?? task, inputs: runPaths)
        remember(task: task, as: remembering)
        let events = steps.map { engine.pipeline(steps: $0, paths: runPaths) } ?? engine.run(task: task, paths: runPaths)
        Task {
            var outcome = Outcome.failed
            var summary: String?
            var sawProblem = false
            for await event in events {
                switch event {
                case .status(let k, let a): status = loc.f(k, a)
                case .step(let d, let t): done = d; total = t; filePermille = 0
                case .fileStep(let p): filePermille = p
                case .info(let k, let a): log.append(LogLine(kind: .info, text: loc.f(k, a)))
                case .warn(let k, let a): log.append(LogLine(kind: .warn, text: loc.f(k, a))); sawProblem = true
                case .error(let k, let a): log.append(LogLine(kind: .error, text: loc.f(k, a))); sawProblem = true
                case .needs(let what, let ps): if what == "trash" { trash(ps) }
                case .data(let n, let d):
                    if n == "outputs" { tracker.add(paths: (d["paths"]?.array ?? []).compactMap { $0.string }) } else { pendingData.append((n, d)) }
                case .menu: break
                case .done(let o, let key, let args):
                    outcome = o
                    summary = key.map { loc.f($0, args) }
                case .crashed:
                    log.append(LogLine(kind: .error, text: loc.t("app.engineCrashed")))
                    sawProblem = true
                }
            }
            finishRun(outcome: outcome, summary: summary, sawProblem: sawProblem)
        }
    }

    func finishRun(outcome: Outcome, summary: String?, sawProblem: Bool) {
        cancelling = false
        recordFinished(outcome: outcome, summary: summary)
        // A privacy check with one unreadable file ends as failed, but the other reports are still worth showing.
        if let (name, data) = pendingData.first, outcome == .succeeded || name == "privacy_report", showResult(name: name, data: data) { return }
        finished = true
        switch outcome {
        case .succeeded: finishedText = summary ?? loc.t("progress.done")
        case .cancelled: finishedText = loc.t("progress.cancelled")
        case .failed: finishedText = summary ?? loc.t("progress.failed")
        }
        status = finishedText // the headline must not keep saying "working" after the run ended
        let close: Bool
        switch outcome {
        case .succeeded: close = settings.closeOnSuccess && !sawProblem
        case .cancelled: close = settings.closeOnCancel
        case .failed: close = settings.closeOnFailure
        }
        if close {
            // Leave time to use "Undo" when the run created files; any use of the results window cancels the close.
            let work = DispatchWorkItem { [weak self] in self?.close() }
            autoClose = work
            DispatchQueue.main.asyncAfter(deadline: .now() + (currentRecordID == nil ? 1.0 : 8.0), execute: work)
        }
    }

    private func showResult(name: String, data: JSONValue) -> Bool {
        switch name {
        case "tree": screen = .tree(data["text"]?.string ?? "")
        case "analyze": screen = .analyze(data)
        case "duplicates":
            let groups = (data["groups"]?.array ?? []).map { g in
                (g.array ?? []).map { DuplicateFile(path: $0["path"]?.string ?? "", size: Int64($0["size"]?.double ?? 0), modified: $0["modified"]?.string ?? "") }
            }
            screen = .duplicates(groups)
        case "duplicates_preview":
            func file(_ v: JSONValue?) -> DuplicateFile? {
                guard let v = v, let path = v["path"]?.string else { return nil }
                return DuplicateFile(path: path, size: Int64(v["size"]?.double ?? 0), modified: "")
            }
            let groups = (data["groups"]?.array ?? []).map { g in
                DuplicatePreview.Group(keep: file(g["keep"]), remove: (g["remove"]?.array ?? []).compactMap { file($0) })
            }
            guard let mode = data["mode"]?.string, ["oldest", "newest", "all"].contains(mode) else { return false }
            screen = .duplicatesPreview(DuplicatePreview(mode: mode, groups: groups, removeCount: data["removeCount"]?.int ?? 0,
                                                         reclaimBytes: Int64(data["reclaimBytes"]?.double ?? 0), permanent: data["permanent"]?.bool ?? false))
        case "privacy_report":
            let reports = pendingData.filter { $0.0 == "privacy_report" }.map { entry -> PrivacyReport in
                let d = entry.1
                let items = (d["items"]?.array ?? []).map { PrivacyReport.Item(kind: $0["kind"]?.string ?? "hidden", high: $0["level"]?.string == "high", detail: $0["detail"]?.string ?? "") }
                return PrivacyReport(file: d["file"]?.string ?? "", format: d["format"]?.string ?? "", items: items)
            }
            screen = .privacyReport(reports, errors: log.filter { $0.kind == .error }.map { $0.text })
        case "empty_folders":
            screen = .emptyFolders(targets: (data["targets"]?.array ?? []).compactMap { $0.string }, permanent: data["permanent"]?.bool ?? false)
        case "rename_preview": screen = .renamePreview((data["lines"]?.array ?? []).compactMap { $0.string })
        default: return false
        }
        return true
    }

    func resetProgress() {
        status = loc.t("app.running"); done = 0; total = 0; filePermille = 0
        log = []; finished = false; finishedText = ""; cancelling = false
    }

    func cancelRunning() { engine?.cancel() }

    func cancel() {
        cancelling = true
        status = loc.t("app.cancelling")
        engine?.cancel()
    }

    // MARK: trash (the engine never deletes on its own in Recycle mode)

    private func trash(_ ps: [String]) {
        for p in ps {
            guard p.hasPrefix("/") else { continue }
            do { try FileManager.default.trashItem(at: URL(fileURLWithPath: p), resultingItemURL: nil) }
            catch { log.append(LogLine(kind: .error, text: loc.f("app.trashFailed", [(p as NSString).lastPathComponent]))) }
        }
    }

    // MARK: answers from dialogs and results

    func submit(task: String) { runEngine(task, paths: paths) }

    func submitRename(prefix: String, name: String, suffix: String, numbered: Bool, dry: Bool?) {
        renameFields = (prefix, name, suffix, numbered)
        runEngine(Planner.rename(prefix: prefix, name: name, suffix: suffix, numbered: numbered, dryRun: dry), paths: paths)
    }

    func applyRename() {
        guard let f = renameFields else { return }
        runEngine(Planner.rename(prefix: f.0, name: f.1, suffix: f.2, numbered: f.3, dryRun: false), paths: paths)
    }

    /// Merging is PDFKit work (the engine has no PDF code), in the order chosen in the dialog.
    func submitMerge(ordered: [String]) {
        paths = ordered
        runNative(.pdfMerge)
    }

    func close() {
        // An instance opened as the settings window goes back to it after a run started from Open.../a drop.
        if startedInSettings, !isSettingsScreen { screen = .settings; return }
        NSApp.terminate(nil)
    }

    func showInFinder(_ path: String) { NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)]) }

    func setMessage(_ text: String) { screen = .message(text) }
    func setScreen(_ s: Screen) { screen = s }
}
