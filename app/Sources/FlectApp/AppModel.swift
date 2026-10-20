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
    case emptyFolders(targets: [String], permanent: Bool)
    case renamePreview([String])
    case pdfPreview(URL)
    case settings
}

struct DuplicateFile: Identifiable {
    let id = UUID()
    let path: String
    let size: Int64
    let modified: String
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
    private(set) var paths: [String] = []
    var engine: EngineClient?
    private var catalog: MenuCatalog?
    private var pendingData: [(String, JSONValue)] = []
    private var renameFields: (String, String, String, Bool)?

    // MARK: start

    func start(arguments: [String]) {
        settings = SettingsStore()
        Loc.shared.use(settings.language)
        engine = EngineClient.locate().map { EngineClient(executable: $0) }
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
        switch Planner.resolve(task, settings: settings) {
        case .engine(let t): runEngine(t, paths: paths)
        case .choose(let group): chooseFrom(group)
        case .dialog(let d): screen = .dialog(d)
        case .native(let n): runNative(n)
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

    func runEngine(_ task: String, paths runPaths: [String]) {
        guard let engine = engine else { screen = .message(loc.t("app.engineMissing")); return }
        resetProgress()
        screen = .running
        pendingData = []
        Task {
            var outcome = Outcome.failed
            var summary: String?
            var sawProblem = false
            for await event in engine.run(task: task, paths: runPaths) {
                switch event {
                case .status(let k, let a): status = loc.f(k, a)
                case .step(let d, let t): done = d; total = t; filePermille = 0
                case .fileStep(let p): filePermille = p
                case .info(let k, let a): log.append(LogLine(kind: .info, text: loc.f(k, a)))
                case .warn(let k, let a): log.append(LogLine(kind: .warn, text: loc.f(k, a))); sawProblem = true
                case .error(let k, let a): log.append(LogLine(kind: .error, text: loc.f(k, a))); sawProblem = true
                case .needs(let what, let ps): if what == "trash" { trash(ps) }
                case .data(let n, let d): pendingData.append((n, d))
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
        if let (name, data) = pendingData.first, outcome == .succeeded, showResult(name: name, data: data) { return }
        finished = true
        switch outcome {
        case .succeeded: finishedText = summary ?? loc.t("progress.done")
        case .cancelled: finishedText = loc.t("progress.cancelled")
        case .failed: finishedText = summary ?? loc.t("progress.failed")
        }
        let close: Bool
        switch outcome {
        case .succeeded: close = settings.closeOnSuccess && !sawProblem
        case .cancelled: close = settings.closeOnCancel
        case .failed: close = settings.closeOnFailure
        }
        if close {
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) { NSApp.terminate(nil) }
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

    func submitMerge(ordered: [String]) { runEngine("pdf.merge", paths: ordered) }

    func close() { NSApp.terminate(nil) }

    func showInFinder(_ path: String) { NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)]) }

    func setMessage(_ text: String) { screen = .message(text) }
    func setScreen(_ s: Screen) { screen = s }
}
