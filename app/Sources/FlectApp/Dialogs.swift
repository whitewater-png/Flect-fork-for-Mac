import SwiftUI
import AVKit
import FlectCore

struct DialogHost: View {
    let dialog: Dialog
    let model: AppModel
    var body: some View {
        switch dialog {
        case .pixels(let base): PixelsDialog(base: base, model: model)
        case .targetBytes(let base): TargetDialog(base: base, model: model)
        case .rename: RenameDialog(model: model)
        case .pdfOrder: PdfOrderDialog(model: model)
        case .trim: TrimDialog(model: model, thumbnail: false)
        case .thumbnail: TrimDialog(model: model, thumbnail: true)
        }
    }
}

private struct Buttons: View {
    let model: AppModel
    let actionTitle: String
    let enabled: Bool
    let action: () -> Void
    var body: some View {
        HStack {
            Spacer()
            Button(model.loc.t("common.cancel")) { model.close() }.keyboardShortcut(.cancelAction)
            Button(actionTitle, action: action).keyboardShortcut(.defaultAction).disabled(!enabled)
        }
    }
}

struct PixelsDialog: View {
    let base: String
    let model: AppModel
    @State private var width = ""
    @State private var height = ""
    @State private var keep = true
    private var w: Int { Int(width) ?? 0 }
    private var h: Int { Int(height) ?? 0 }
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(model.loc.t("dialog.resize.hint"))
            HStack {
                Text(model.loc.t("dialog.resize.width")); TextField("", text: $width).frame(width: 90)
                Text(model.loc.t("dialog.resize.height")); TextField("", text: $height).frame(width: 90)
            }
            Toggle(model.loc.t("dialog.resize.keepAspect"), isOn: $keep)
            Buttons(model: model, actionTitle: model.loc.t("dialog.resize.action"), enabled: (w > 0 || h > 0) && w <= 65535 && h <= 65535) {
                model.submit(task: Planner.resizeCustom(base: base, width: w, height: h, keepAspect: keep))
            }
        }.padding(20)
    }
}

enum SizeParser {
    /// `8MB`, `900KB`, `1500000` (binary units like the Windows version).
    static func parse(_ text: String) -> Int64? {
        let t = text.trimmingCharacters(in: .whitespaces).lowercased()
        let units: [(String, Double)] = [("gb", 1 << 30), ("mb", 1 << 20), ("kb", 1 << 10), ("b", 1)]
        for (suffix, mult) in units where t.hasSuffix(suffix) {
            guard let v = Double(t.dropLast(suffix.count).trimmingCharacters(in: .whitespaces)), v > 0, v < 1e6 else { return nil }
            return Int64(v * mult)
        }
        guard let v = Int64(t), v > 0 else { return nil }
        return v
    }
}

struct TargetDialog: View {
    let base: String
    let model: AppModel
    @State private var text = ""
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(model.loc.t("dialog.target.label"))
            TextField("8MB", text: $text)
            Text(SizeParser.parse(text).map { model.loc.f("dialog.target.echo", [ByteCountFormatter.string(fromByteCount: $0, countStyle: .binary)]) } ?? model.loc.t("dialog.target.syntax"))
                .font(.caption).foregroundStyle(.secondary)
            Buttons(model: model, actionTitle: model.loc.t("dialog.target.action"), enabled: SizeParser.parse(text) != nil) {
                if let b = SizeParser.parse(text) { model.submit(task: Planner.compress(base: base, bytes: b)) }
            }
        }.padding(20)
    }
}

struct RenameDialog: View {
    let model: AppModel
    @State private var prefix = ""
    @State private var name = ""
    @State private var suffix = ""
    @State private var numbered = true
    private let bad = CharacterSet(charactersIn: "/:\0")
    private var invalid: Bool { [prefix, name, suffix].contains { $0.rangeOfCharacter(from: bad) != nil } }
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(model.loc.t("dialog.rename.body"))
            TextField(model.loc.t("dialog.rename.prefix"), text: $prefix)
            TextField(model.loc.t("dialog.rename.name"), text: $name)
            TextField(model.loc.t("dialog.rename.suffix"), text: $suffix)
            Toggle(model.loc.t("dialog.rename.numbering"), isOn: $numbered)
            if invalid { Text(model.loc.t("dialog.rename.invalidChars")).foregroundStyle(.red).font(.caption) }
            Buttons(model: model, actionTitle: model.loc.t("dialog.rename.action"), enabled: !invalid && !(prefix + name + suffix).isEmpty) {
                // dry = nil: the engine follows the "preview first" setting and sends rename_preview data.
                model.submitRename(prefix: prefix, name: name, suffix: suffix, numbered: numbered, dry: nil)
            }
        }.padding(20)
    }
}

struct PdfOrderDialog: View {
    let model: AppModel
    @State private var items: [String] = []
    @State private var selection: String?
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(model.loc.t("dialog.pdfOrder.body"))
            List(items, id: \.self, selection: $selection) { Text(($0 as NSString).lastPathComponent) }
            HStack {
                Button(model.loc.t("dialog.pdfOrder.up")) { move(-1) }.disabled(selection == nil)
                Button(model.loc.t("dialog.pdfOrder.down")) { move(1) }.disabled(selection == nil)
            }
            Buttons(model: model, actionTitle: model.loc.t("common.ok"), enabled: items.count >= 2) { model.submitMerge(ordered: items) }
        }
        .padding(20)
        .onAppear { items = model.paths.sorted { $0.localizedStandardCompare($1) == .orderedAscending } }
    }
    private func move(_ d: Int) {
        guard let s = selection, let i = items.firstIndex(of: s), items.indices.contains(i + d) else { return }
        items.swapAt(i, i + d)
    }
}

/// Video position picker shared by trim (range) and thumbnail (single time).
struct TrimDialog: View {
    let model: AppModel
    let thumbnail: Bool
    @State private var player = AVPlayer()
    @State private var start = ""
    @State private var end = ""
    @State private var time = ""
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(model.loc.t(thumbnail ? "dialog.thumbnail.hint" : "dialog.trim.hint")).font(.caption)
            VideoPlayer(player: player).frame(minHeight: 200)
            if thumbnail {
                HStack {
                    TextField("0:00", text: $time).frame(width: 110)
                    Button(model.loc.t("dialog.thumbnail.set")) { time = format(current()) }
                }
            } else {
                HStack {
                    TextField(model.loc.t("dialog.trim.start"), text: $start).frame(width: 110)
                    Button(model.loc.t("dialog.trim.setStart")) { start = format(current()) }
                    TextField(model.loc.t("dialog.trim.end"), text: $end).frame(width: 110)
                    Button(model.loc.t("dialog.trim.setEnd")) { end = format(current()) }
                }
            }
            Buttons(model: model, actionTitle: model.loc.t(thumbnail ? "dialog.thumbnail.action" : "dialog.trim.action"), enabled: valid) {
                if thumbnail, let t = TimeParser.parse(time) { model.submit(task: Planner.thumbnail(at: t)) }
                else if let s = TimeParser.parse(start), let e = TimeParser.parse(end) { model.submit(task: Planner.trim(start: s, end: e)) }
            }
        }
        .padding(16)
        .onAppear {
            if let p = model.paths.first { player.replaceCurrentItem(with: AVPlayerItem(url: URL(fileURLWithPath: p))) }
        }
        .onDisappear { player.pause() }
    }
    private var valid: Bool {
        if thumbnail { return TimeParser.parse(time) != nil }
        guard let s = TimeParser.parse(start), let e = TimeParser.parse(end) else { return false }
        return e > s
    }
    private func current() -> Double { max(0, player.currentTime().seconds.isFinite ? player.currentTime().seconds : 0) }
    private func format(_ t: Double) -> String { String(format: "%.3f", t) }
}

enum TimeParser {
    /// `12.5`, `1:05.4`, `1:02:03.4` in seconds.
    static func parse(_ text: String) -> Double? {
        let parts = text.trimmingCharacters(in: .whitespaces).split(separator: ":", omittingEmptySubsequences: false)
        guard (1...3).contains(parts.count) else { return nil }
        var total = 0.0
        for p in parts {
            guard let v = Double(p), v >= 0, v.isFinite else { return nil }
            total = total * 60 + v
        }
        return total
    }
}
