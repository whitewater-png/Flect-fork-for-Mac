import AppKit
import SwiftUI
import FlectCore

/// Collects what one run reports it created. The paths come from the engine's `data: outputs` events and from the
/// native (PDFKit / ImageIO / Vision) workers; `RunRecorder` then checks every one before it can be undone.
@MainActor
final class RunTracker {
    private(set) var task = ""
    private(set) var inputs: [String] = []
    private(set) var outputs: [String] = []
    private(set) var startedAt = Date()
    /// Set before chaining a second run into the same user-visible run (the colour-key fallback after the AI step).
    var carryOver = false

    func begin(task: String, inputs: [String]) {
        if carryOver { carryOver = false; return }
        self.task = task
        self.inputs = inputs
        outputs = []
        startedAt = Date()
    }

    func add(paths: [String]) { outputs.append(contentsOf: paths) }
}

extension AppModel {
    static func nativeName(_ task: NativeTask) -> String {
        switch task {
        case .heicConvert: return "image.convert.HEIC"
        case .pdfPreview: return "pdf.preview"
        case .pdfRender(let f): return "pdf.convert.\(f)"
        case .pdfText: return "pdf.convert.TXT"
        case .pdfMerge: return "pdf.merge"
        case .ocr: return "image.ocr"
        case .removeBackground: return "image.chromakey"
        }
    }

    var isSettingsScreen: Bool { if case .settings = screen { return true }; return false }

    /// Called once per finished run: remembers what it created, then tells the user (notification).
    func recordFinished(outcome: Outcome, summary: String?, partial: Bool = false, counts: (removed: Int, leftAlone: Int)? = nil) {
        let recorded = RecordedOutcome.string(outcome.rawValue, problems: partial)
        var record = RunRecorder.record(task: tracker.task, outcome: recorded, startedAt: tracker.startedAt, finishedAt: Date(),
                                        inputs: tracker.inputs, reportedOutputs: tracker.outputs, summary: summary)
        record.removed = counts?.removed
        record.leftAlone = counts?.leftAlone
        if record.outputs.isEmpty {
            currentRecordID = nil
            // A deleting run creates no files, but what it removed and left alone is still worth keeping.
            if counts != nil { history = historyStore.upsert(record) }
        } else {
            history = historyStore.upsert(record)
            currentRecordID = record.id
        }
        let text = NotificationText.make(outcome: recorded, summary: summary, outputCount: record.outputs.count, loc: loc)
        Notifier.shared.post(title: text.title, body: text.body, recordID: record.outputs.isEmpty ? nil : record.id)
    }

    func cancelAutoClose() { autoClose?.cancel(); autoClose = nil }

    func record(_ id: UUID?) -> RunRecord? { id.flatMap { id in history.first { $0.id == id } } }

    func updateRecord(_ r: RunRecord) { history = historyStore.upsert(r) }

    /// Moves the run's own outputs to the Trash (see `OutputUndo` for the checks) off the main thread.
    func undo(_ id: UUID) async -> [UndoResult] {
        cancelAutoClose()
        guard let rec = record(id) else { return [] }
        let (updated, results) = await Task.detached(priority: .userInitiated) { OutputUndo.undo(rec) }.value
        updateRecord(updated)
        return results
    }

    func reveal(_ paths: [String]) {
        cancelAutoClose()
        let urls = paths.filter { FileManager.default.fileExists(atPath: $0) }.map { URL(fileURLWithPath: $0) }
        if !urls.isEmpty { NSWorkspace.shared.activateFileViewerSelecting(urls) }
    }

    func taskLabel(_ task: String) -> String {
        if task.hasPrefix("preset:") { return String(task.dropFirst(7)) } // a preset run is shown by its name
        let base = task.split(separator: "?").first.map(String.init) ?? task
        return loc.has("menu.\(base)") ? loc.t("menu.\(base)") : base
    }
}

/// The files a run created, with "Show in Finder" and "Undo". Used under the progress of the run that just ended
/// and in the Recent Runs window.
@MainActor struct RunResultPanel: View {
    @ObservedObject var model: AppModel
    let recordID: UUID
    @State private var message: String?
    @State private var busy = false
    private var loc: Loc { model.loc }

    var body: some View {
        if let rec = model.record(recordID) {
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text(loc.n("ux.results.count", rec.outputs.count)).font(.headline)
                    Spacer()
                    Button(loc.t("ux.results.reveal")) { model.reveal(rec.outputs.filter { $0.status != .trashed }.map { $0.path }) }
                        .disabled(rec.undoableCount == 0)
                    Button(loc.t("ux.results.undo")) { confirmUndo(rec) }
                        .disabled(busy || rec.undoableCount == 0)
                }
                List(rec.outputs, id: \.path) { out in
                    HStack {
                        VStack(alignment: .leading, spacing: 1) {
                            Text((out.path as NSString).lastPathComponent).lineLimit(1).truncationMode(.middle)
                            Text((out.path as NSString).deletingLastPathComponent).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
                        }
                        Spacer()
                        if let s = out.status {
                            Text(loc.t("ux.results.status.\(s.rawValue)")).font(.caption).foregroundStyle(s == .trashed ? Color.secondary : Color.orange)
                        } else {
                            Button(loc.t("ux.results.reveal")) { model.reveal([out.path]) }.buttonStyle(.link).font(.caption)
                        }
                    }
                    .help(out.detail ?? out.path)
                }
                .frame(minHeight: 90)
                if rec.untracked > 0 { Text(loc.n("ux.results.untracked", rec.untracked)).font(.caption).foregroundStyle(.secondary) }
                if let m = message { Text(m).font(.caption) }
            }
        }
    }

    private func confirmUndo(_ rec: RunRecord) {
        model.cancelAutoClose()
        let alert = NSAlert()
        alert.messageText = loc.n("ux.results.confirm", rec.undoableCount)
        alert.informativeText = loc.t("ux.results.confirm.body")
        alert.addButton(withTitle: loc.t("ux.results.undo"))
        alert.addButton(withTitle: loc.t("common.cancel"))
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        busy = true
        Task {
            let results = await model.undo(rec.id)
            let moved = results.filter { $0.status == .trashed }.count
            message = loc.f("ux.results.undone", [String(moved), String(results.count - moved)])
            busy = false
        }
    }
}

/// File > Recent Runs...: the last runs that created files, newest first.
@MainActor struct HistoryView: View {
    @ObservedObject var model: AppModel
    @State private var selection: UUID?
    private var loc: Loc { model.loc }

    var body: some View {
        HStack(spacing: 0) {
            List(model.history, selection: $selection) { rec in
                VStack(alignment: .leading, spacing: 1) {
                    Text(model.taskLabel(rec.task)).lineLimit(1)
                    Text("\(rec.finishedAt.formatted(date: .abbreviated, time: .shortened))  ·  \(loc.n("ux.results.count", rec.outputs.count))")
                        .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
                .tag(rec.id)
            }
            .frame(width: 230)
            Divider()
            Group {
                if let id = selection ?? model.historyFocus ?? model.history.first?.id, model.record(id) != nil {
                    RunResultPanel(model: model, recordID: id).id(id)
                } else {
                    Text(model.history.isEmpty ? loc.t("ux.history.empty") : loc.t("ux.history.pick")).foregroundStyle(.secondary).frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .padding(12)
        }
        .frame(minWidth: 640, minHeight: 320)
        .onChange(of: model.historyFocus) { _, new in if let new { selection = new } }
    }
}
