import SwiftUI
import PDFKit
import FlectCore

@MainActor struct RootView: View {
    @ObservedObject var model: AppModel
    private var loc: Loc { model.loc }

    var body: some View {
        Group {
            switch model.screen {
            case .idle: ProgressView().padding()
            case .message(let text): MessageView(text: text, model: model)
            case .running: RunView(model: model)
            case .choose(let title, let entries): ChooseView(title: title, entries: entries, model: model)
            case .dialog(let d): DialogHost(dialog: d, model: model)
            case .tree(let text): TextResultView(text: text, model: model)
            case .analyze(let data): TextResultView(text: AnalyzeFormatter.text(data, loc: loc), model: model)
            case .duplicates(let groups): DuplicatesView(groups: groups, model: model)
            case .duplicatesPreview(let preview): DuplicatesPreviewView(preview: preview, model: model)
            case .privacyReport(let reports, let errors): PrivacyReportView(reports: reports, errors: errors, model: model)
            case .emptyFolders(let targets, let permanent): EmptyFoldersView(targets: targets, permanent: permanent, model: model)
            case .renamePreview(let lines): RenamePreviewView(lines: lines, model: model)
            case .pdfPreview(let url): PDFPreviewView(url: url)
            case .settings: SettingsView(model: model)
            case .pick(let selection): PickerView(selection: selection, model: model)
            }
        }
        .frame(minWidth: 480, minHeight: 300)
    }
}

@MainActor struct MessageView: View {
    let text: String
    let model: AppModel
    var body: some View {
        VStack(spacing: 16) {
            Text(text).multilineTextAlignment(.center).textSelection(.enabled)
            Button(model.loc.t("common.close")) { model.close() }.keyboardShortcut(.defaultAction)
        }.padding(24)
    }
}

@MainActor struct RunView: View {
    @ObservedObject var model: AppModel
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(model.status).font(.headline).lineLimit(2)
            if model.total > 0 {
                ProgressView(value: Double(model.done), total: Double(model.total))
                Text("\(model.done) / \(model.total)").font(.caption).foregroundStyle(.secondary)
            } else if !model.finished {
                ProgressView().progressViewStyle(.linear)
            }
            if model.filePermille > 0 && !model.finished {
                ProgressView(value: Double(model.filePermille), total: 1000)
            }
            List(model.log) { line in
                Text(line.text)
                    .foregroundStyle(line.kind == .error ? Color.red : line.kind == .warn ? Color.orange : Color.primary)
                    .textSelection(.enabled)
            }
            .frame(minHeight: 100)
            if model.finished, let id = model.currentRecordID { RunResultPanel(model: model, recordID: id) }
            HStack {
                Spacer()
                if model.finished {
                    Button(model.loc.t("common.close")) { model.close() }.keyboardShortcut(.defaultAction)
                } else {
                    Button(model.loc.t("common.cancel")) { model.cancel() }.disabled(model.cancelling).keyboardShortcut(.cancelAction)
                }
            }
        }.padding(16)
    }
}

@MainActor struct ChooseView: View {
    let title: String
    let entries: [MenuEntry]
    let model: AppModel
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.headline)
            List(entries, id: \.id) { entry in
                Button(model.label(for: entry)) { model.handle(task: entry.id) }.buttonStyle(.plain)
            }
            HStack { Spacer(); Button(model.loc.t("common.cancel")) { model.close() }.keyboardShortcut(.cancelAction) }
        }.padding(16)
    }
}

@MainActor struct TextResultView: View {
    let text: String
    let model: AppModel
    var body: some View {
        VStack(spacing: 8) {
            ScrollView([.vertical, .horizontal]) {
                Text(text).font(.system(.body, design: .monospaced)).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading).padding(8)
            }
            HStack {
                Button(model.loc.t("common.copy")) {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(text, forType: .string)
                }
                Spacer()
                Button(model.loc.t("common.close")) { model.close() }.keyboardShortcut(.defaultAction)
            }
        }.padding(16)
    }
}

enum AnalyzeFormatter {
    static func bytes(_ v: Double) -> String { ByteCountFormatter.string(fromByteCount: Int64(v), countStyle: .file) }

    static func text(_ d: JSONValue, loc: Loc) -> String {
        var out: [String] = []
        out.append("\(loc.t("dialog.analyze.total")): \(bytes(d["totalBytes"]?.double ?? 0))")
        out.append("\(loc.t("dialog.analyze.files")): \(d["files"]?.int ?? 0)")
        out.append("\(loc.t("dialog.analyze.folders")): \(d["folders"]?.int ?? 0)")
        if let t = d["scanSeconds"]?.double { out.append("\(loc.t("dialog.analyze.scanTime")): \(String(format: "%.1f", t)) s") }
        if let dup = d["duplicates"], (dup["groups"]?.int ?? 0) > 0 {
            out.append("")
            out.append("\(loc.t("dialog.analyze.duplicates")): \(dup["groups"]?.int ?? 0) / \(dup["files"]?.int ?? 0) / \(bytes(dup["reclaimableBytes"]?.double ?? 0))")
        }
        if let largest = d["largest"]?.array, !largest.isEmpty {
            out.append("")
            out.append(loc.t("dialog.analyze.largest"))
            for e in largest { out.append("  \(bytes(e["bytes"]?.double ?? 0))  \(e["path"]?.string ?? "")") }
        }
        if let types = d["byType"]?.array, !types.isEmpty {
            out.append("")
            out.append(loc.t("dialog.analyze.types"))
            for e in types { out.append("  \(e["extension"]?.string ?? "?")  \(e["files"]?.int ?? 0)  \(bytes(e["bytes"]?.double ?? 0))") }
        }
        if let empty = d["emptyFolders"]?.array, !empty.isEmpty {
            out.append("")
            out.append(loc.t("dialog.analyze.empty"))
            for e in empty { out.append("  \(e.string ?? "")") }
        }
        return out.joined(separator: "\n")
    }
}

@MainActor struct DuplicatesView: View {
    let groups: [[DuplicateFile]]
    let model: AppModel
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(model.loc.t("app.keepWhich")).font(.headline)
            List {
                ForEach(Array(groups.enumerated()), id: \.offset) { _, g in
                    Section(ByteCountFormatter.string(fromByteCount: g.first?.size ?? 0, countStyle: .file)) {
                        ForEach(g) { f in Text(f.path).font(.caption).textSelection(.enabled) }
                    }
                }
            }
            HStack {
                // Each choice first shows what would be removed (dry run); the Trash is only asked for after confirming there.
                Button(model.loc.t("app.keepOldest")) { model.submit(task: Planner.duplicates(mode: "oldest", dryRun: true)) }
                Button(model.loc.t("app.keepNewest")) { model.submit(task: Planner.duplicates(mode: "newest", dryRun: true)) }
                Button(model.loc.t("app.deleteAll")) { model.submit(task: Planner.duplicates(mode: "all", dryRun: true)) }
                Spacer()
                Button(model.loc.t("common.cancel")) { model.close() }.keyboardShortcut(.cancelAction)
            }
        }.padding(16)
    }
}

/// "Show what will happen, then confirm" for duplicate removal. Nothing has been touched when this appears.
@MainActor struct DuplicatesPreviewView: View {
    let preview: DuplicatePreview
    let model: AppModel
    var body: some View {
        let loc = model.loc
        let size = ByteCountFormatter.string(fromByteCount: preview.reclaimBytes, countStyle: .file)
        VStack(alignment: .leading, spacing: 8) {
            Text(loc.f(preview.permanent ? "share.dup.titlePermanent" : "share.dup.titleTrash", [String(preview.removeCount), size])).font(.headline)
            Text(loc.t("share.dup.note")).font(.caption).foregroundStyle(.secondary)
            if preview.permanent { Text(loc.t("settings.folder.delete.warning")).foregroundStyle(.red) }
            List {
                ForEach(preview.groups) { g in
                    Section(ByteCountFormatter.string(fromByteCount: g.keep?.size ?? g.remove.first?.size ?? 0, countStyle: .file)) {
                        if let k = g.keep {
                            Label { Text(k.path).font(.caption).textSelection(.enabled) } icon: { Image(systemName: "checkmark.circle").foregroundStyle(.green) }
                                .help(loc.t("share.dup.keep"))
                        } else {
                            Text(loc.t("share.dup.keepNone")).font(.caption).foregroundStyle(.secondary)
                        }
                        ForEach(g.remove) { f in
                            Label { Text(f.path).font(.caption).textSelection(.enabled) } icon: { Image(systemName: "trash").foregroundStyle(.red) }
                                .help(loc.t("share.dup.remove"))
                        }
                    }
                }
            }
            HStack {
                Spacer()
                // Cancel is the default button: pressing Return must never delete anything.
                Button(loc.t("common.cancel")) { model.close() }.keyboardShortcut(.defaultAction)
                Button(loc.t(preview.permanent ? "share.dup.confirmPermanent" : "share.dup.confirmTrash")) {
                    model.submit(task: Planner.duplicates(mode: preview.mode))
                }
            }
        }.padding(16)
    }
}

/// Result of the read-only privacy check.
@MainActor struct PrivacyReportView: View {
    let reports: [PrivacyReport]
    let errors: [String]
    let model: AppModel
    var body: some View {
        let loc = model.loc
        VStack(alignment: .leading, spacing: 8) {
            Text(loc.t("share.privacy.title")).font(.headline)
            Text(loc.t("share.privacy.readOnly")).font(.caption).foregroundStyle(.secondary)
            List {
                ForEach(reports) { r in
                    Section("\(r.file)  (\(r.format))") {
                        if r.items.isEmpty {
                            Label(loc.t("share.privacy.clean"), systemImage: "checkmark.shield").foregroundStyle(.green)
                        } else {
                            ForEach(r.items) { item in
                                HStack(alignment: .firstTextBaseline) {
                                    Image(systemName: item.high ? "exclamationmark.triangle.fill" : "exclamationmark.circle")
                                        .foregroundStyle(item.high ? Color.red : Color.orange)
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(loc.has("share.kind.\(item.kind)") ? loc.t("share.kind.\(item.kind)") : item.kind)
                                        if !item.detail.isEmpty { Text(item.detail).font(.caption).foregroundStyle(.secondary).textSelection(.enabled) }
                                    }
                                }
                            }
                        }
                    }
                }
                ForEach(errors, id: \.self) { Text($0).foregroundStyle(.red).font(.caption) }
            }
            HStack {
                Spacer()
                if reports.contains(where: { !$0.items.isEmpty }) {
                    Button(loc.t("share.privacy.makeSafe")) { model.submit(task: Planner.makeSafe(maxEdge: 0)) }
                }
                Button(loc.t("common.close")) { model.close() }.keyboardShortcut(.defaultAction)
            }
        }.padding(16)
    }
}

@MainActor struct EmptyFoldersView: View {
    let targets: [String]
    let permanent: Bool
    let model: AppModel
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(model.loc.f("app.confirmEmpty", [String(targets.count)])).font(.headline)
            if permanent { Text(model.loc.t("settings.folder.delete.warning")).foregroundStyle(.red) }
            List(targets, id: \.self) { Text($0).font(.caption).textSelection(.enabled) }
            HStack {
                Spacer()
                Button(model.loc.t("common.cancel")) { model.close() }.keyboardShortcut(.cancelAction)
                Button(model.loc.t("common.remove")) { model.submit(task: Planner.confirmEmpty) }.keyboardShortcut(.defaultAction)
            }
        }.padding(16)
    }
}

@MainActor struct RenamePreviewView: View {
    let lines: [String]
    let model: AppModel
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(model.loc.t("dialog.rename.preview")).font(.headline)
            List(lines, id: \.self) { Text($0).font(.system(.caption, design: .monospaced)) }
            HStack {
                Spacer()
                Button(model.loc.t("common.cancel")) { model.close() }.keyboardShortcut(.cancelAction)
                Button(model.loc.t("dialog.rename.action")) { model.applyRename() }.keyboardShortcut(.defaultAction)
            }
        }.padding(16)
    }
}

struct PDFPreviewView: NSViewRepresentable {
    let url: URL
    func makeNSView(context: Context) -> PDFView {
        let v = PDFView()
        v.autoScales = true
        v.document = PDFDocument(url: url)
        return v
    }
    func updateNSView(_ nsView: PDFView, context: Context) {}
}
