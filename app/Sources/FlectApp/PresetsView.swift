import AppKit
import SwiftUI
import FlectCore

// Presets (named step lists, run as one pipeline), favorites pinned to the top of the action picker, and "Run again".
// The rules live in FlectCore (Pipeline / Presets / PickerMemory); this file is the UI and the glue in AppModel.
// See docs/mac-port/17-presets-pipeline.ja.md.

extension AppModel {
    func reloadPresets() {
        presetStore.reload()
        presets = presetStore.presets
        memory = pickerStore.load()
        // Only a file that read cleanly says which presets exist; a broken or newer one must not prune the pins.
        if presetStore.state == .ok || presetStore.state == .missing { memory.keepPresets(Set(presets.map { $0.id })) }
    }

    /// Saves the whole list. Returns a message when it could not.
    func savePresets(_ list: [Preset]) -> String? {
        do {
            try presetStore.save(list)
        } catch PresetStore.SaveError.readOnly {
            return loc.t("ux.presets.newer")
        } catch {
            return loc.f("ux.presets.saveFailed", ["\(error)"])
        }
        presets = presetStore.presets
        memory.keepPresets(Set(presets.map { $0.id }))
        try? pickerStore.save(memory)
        return nil
    }

    /// Remembers what was just started for "Run again". A catalog id is replayed through Planner (like a Quick Action),
    /// a step only if it passes the engine's allow-list; anything else (a confirmed delete, for one) is not remembered.
    func remember(task: String) {
        let ref: PickerRef?
        if let o = rememberOverride {
            ref = o
            rememberOverride = nil
        } else if PickerRef.isCatalogID(task), Planner.resolve(task, settings: settings) != .unknown {
            ref = .task(task)
        } else if Pipeline.isValidStep(task) {
            ref = .step(task)
        } else {
            ref = nil
        }
        guard let ref = ref, ref != memory.last else { return }
        memory.remember(ref)
        try? pickerStore.save(memory)
    }

    /// Pins or unpins. Says so when the list is full.
    func togglePin(_ ref: PickerRef) {
        if !memory.isPinned(ref) && memory.favorites.count >= PickerMemory.maxFavorites {
            let alert = NSAlert()
            alert.messageText = loc.f("ux.picker.pinFull", [String(PickerMemory.maxFavorites)])
            alert.runModal()
            return
        }
        memory.toggle(ref)
        try? pickerStore.save(memory)
    }

    /// Text for a pinned / remembered item.
    func shortcutLabel(_ s: PickerShortcut) -> String {
        switch s.ref {
        case .preset: return s.preset?.name ?? ""
        case .step(let step): return StepText.describe(step, loc: loc)
        case .task(let id):
            guard let entry = catalog?.find(id) else { return taskLabel(id) }
            let own = label(for: entry)
            // `image.resize.p1` reads "50%" in its chooser; out of context it needs the task name in front.
            let parent = id.split(separator: ".").dropLast().joined(separator: ".")
            return parent.contains(".") && loc.has("menu.\(parent)") ? "\(loc.t("menu.\(parent)")) \(own)" : own
        }
    }

    func runShortcut(_ s: PickerShortcut) {
        paths = s.section.paths
        switch s.ref {
        case .task(let id): handle(task: id)
        case .step(let step): runEngine(step, paths: paths)
        case .preset: if let p = s.preset { runPreset(p) }
        }
    }

    /// Runs a preset on `paths`: one step as an ordinary run, several as one pipeline (the engine checks them again).
    func runPreset(_ p: Preset) {
        guard case .success = Pipeline.plan(p.steps) else { screen = .message(loc.t("ux.presets.missing")); return }
        rememberOverride = .preset(p.id)
        if p.steps.count == 1 {
            runEngine(p.steps[0], paths: paths, label: "preset:\(p.name)")
        } else {
            runEngine("pipeline", paths: paths, steps: p.steps, label: "preset:\(p.name)")
        }
    }

    /// `--task preset:<UUID>` (a Quick Action made for a preset).
    func runPreset(ref: String) {
        guard case .preset(let id)? = PickerRef(raw: ref), let p = presets.first(where: { $0.id == id }) else {
            screen = .message(loc.t("ux.presets.missing"))
            return
        }
        runPreset(p)
    }
}

// MARK: picker

/// "Run again", the pinned items and the presets that apply to the selection, above the normal task list.
@MainActor struct PickerShortcutRows: View {
    let selection: PickerSelection
    @ObservedObject var model: AppModel
    private var loc: Loc { model.loc }

    var body: some View {
        let again = PickerShortcuts.runAgain(model.memory, presets: model.presets, selection: selection)
        let pinned = PickerShortcuts.favorites(model.memory, presets: model.presets, selection: selection)
        let presets = PickerShortcuts.presets(model.presets, selection: selection)
        if let a = again {
            Section {
                Button { model.runShortcut(a) } label: { Label(loc.f("ux.picker.runAgain", [model.shortcutLabel(a)]), systemImage: "arrow.clockwise") }
                    .buttonStyle(.plain)
            }
        }
        if !pinned.isEmpty {
            Section(loc.t("ux.picker.pinned")) {
                ForEach(pinned, id: \.ref) { s in
                    Button { model.runShortcut(s) } label: { Label(model.shortcutLabel(s), systemImage: "pin.fill") }
                        .buttonStyle(.plain)
                        .contextMenu { PinMenu(ref: s.ref, model: model) }
                }
            }
        }
        if !presets.isEmpty {
            Section(loc.t("ux.picker.presets")) {
                ForEach(presets, id: \.ref) { s in
                    Button { model.runShortcut(s) } label: {
                        VStack(alignment: .leading, spacing: 1) {
                            Text(s.preset?.name ?? "")
                            Text(StepText.describe(steps: s.preset?.steps ?? [], loc: loc)).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                        }
                    }
                    .buttonStyle(.plain)
                    .contextMenu { PinMenu(ref: s.ref, model: model) }
                }
            }
        }
    }
}

@MainActor struct PinMenu: View {
    let ref: PickerRef
    @ObservedObject var model: AppModel

    var body: some View {
        Button(model.loc.t(model.memory.isPinned(ref) ? "ux.picker.unpin" : "ux.picker.pin")) { model.togglePin(ref) }
    }
}

// MARK: settings tab

@MainActor struct PresetsView: View {
    @ObservedObject var model: AppModel
    @State private var editing: Preset?
    @State private var deleting: Preset?
    @State private var error: String?
    private var loc: Loc { model.loc }
    private var readOnly: Bool { model.presetStore.isReadOnly }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(loc.t("ux.presets.hint")).font(.caption).foregroundStyle(.secondary)
            switch model.presetStore.state {
            case .broken(let why): Text(loc.f("ux.presets.broken", [why])).font(.callout).foregroundStyle(.orange)
            case .newer: Text(loc.t("ux.presets.newer")).font(.callout).foregroundStyle(.orange)
            case .ok, .missing: EmptyView()
            }
            if model.presets.isEmpty {
                Text(loc.t("ux.presets.empty")).foregroundStyle(.secondary).frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List(model.presets) { p in
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(p.name).font(.headline)
                            Text(StepText.describe(steps: p.steps, loc: loc)).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                        }
                        Spacer()
                        Button { model.togglePin(.preset(p.id)) } label: { Image(systemName: model.memory.isPinned(.preset(p.id)) ? "pin.fill" : "pin") }
                            .buttonStyle(.borderless).help(loc.t("ux.presets.pin"))
                        Button(loc.t("ux.presets.edit")) { editing = p }.disabled(readOnly)
                        Button(loc.t("ux.presets.delete")) { deleting = p }.disabled(readOnly)
                    }
                    .help(loc.f("ux.presets.cli", [p.name]))
                }
            }
            HStack {
                if let e = error { Text(e).font(.caption).foregroundStyle(.red) }
                Spacer()
                Button(loc.t("ux.presets.new")) { editing = Preset(name: "", steps: [StepDraft(task: "image.resize").build()]) }
                    .disabled(readOnly || model.presets.count >= PresetFile.maxPresets)
            }
        }
        .padding(12)
        .sheet(item: $editing) { p in PresetEditor(model: model, original: p) }
        .confirmationDialog(loc.f("ux.presets.deleteConfirm", [deleting?.name ?? ""]),
                            isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } })) {
            Button(loc.t("ux.presets.delete"), role: .destructive) {
                if let d = deleting { error = model.savePresets(model.presets.filter { $0.id != d.id }) }
                deleting = nil
            }
            Button(loc.t("common.cancel"), role: .cancel) { deleting = nil }
        } message: {
            Text(loc.t("ux.presets.deleteBody"))
        }
        .task { model.reloadPresets() }
    }
}

/// Create or edit one preset: a name and 1...8 steps, checked live with the engine's rules.
@MainActor struct PresetEditor: View {
    struct Row: Identifiable { let id = UUID(); var draft: StepDraft }

    let model: AppModel
    let original: Preset
    @State private var name: String
    @State private var rows: [Row]
    @State private var error: String?
    @Environment(\.dismiss) private var dismiss
    private var loc: Loc { model.loc }

    init(model: AppModel, original: Preset) {
        self.model = model
        self.original = original
        _name = State(initialValue: original.name)
        let drafts = original.steps.compactMap(StepDraft.init(step:))
        _rows = State(initialValue: (drafts.isEmpty ? [StepDraft(task: "image.resize")] : drafts).map { Row(draft: $0) })
    }

    private var isNew: Bool { !model.presets.contains { $0.id == original.id } }
    private var steps: [String] { rows.map { $0.draft.build() } }
    private var planError: PipelineError? { if case .failure(let e) = Pipeline.plan(steps) { return e }; return nil }
    private var nameOK: Bool {
        PresetFile.isValidName(name) && !model.presets.contains { $0.id != original.id && $0.name.lowercased() == name.lowercased() }
    }

    /// The first task that can read what the last step makes.
    private var nextTask: String {
        guard let last = rows.last, case .success(let s) = Pipeline.parse(last.draft.build()) else { return "image.resize" }
        return Pipeline.chainableTasks.first { Pipeline.accepts($0).contains(s.produces) } ?? "image.resize"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(loc.t(isNew ? "ux.presets.newTitle" : "ux.presets.editTitle")).font(.headline)
            TextField(loc.t("ux.presets.name"), text: Binding(get: { name }, set: { name = String($0.prefix(PresetFile.maxNameLength * 2)) }))
            if !name.isEmpty && !nameOK { Text(loc.t("ux.presets.badName")).font(.caption).foregroundStyle(.red) }
            Text(loc.t("ux.presets.steps")).font(.subheadline)
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(Array(rows.enumerated()), id: \.element.id) { i, row in
                        StepEditorRow(draft: binding(for: row.id), number: i + 1, loc: loc,
                                      canMoveUp: i > 0, canMoveDown: i + 1 < rows.count, canRemove: rows.count > 1,
                                      move: { offset in move(row.id, by: offset) }, remove: { rows.removeAll { $0.id == row.id } })
                        Divider()
                    }
                    Button(loc.t("ux.presets.addStep")) { rows.append(Row(draft: StepDraft(task: nextTask))) }
                        .disabled(rows.count >= Pipeline.maxSteps)
                }
                .padding(.vertical, 4)
            }
            if let e = planError { Text(loc.f(e.key, e.args)).font(.caption).foregroundStyle(.red) }
            if let e = error { Text(e).font(.caption).foregroundStyle(.red) }
            HStack {
                Spacer()
                Button(loc.t("common.cancel")) { dismiss() }.keyboardShortcut(.cancelAction)
                Button(loc.t("ux.presets.save")) { save() }.keyboardShortcut(.defaultAction).disabled(!nameOK || planError != nil)
            }
        }
        .padding(16)
        .frame(minWidth: 520, minHeight: 420)
    }

    private func binding(for id: UUID) -> Binding<StepDraft> {
        Binding(get: { rows.first { $0.id == id }?.draft ?? StepDraft(task: "image.strip") },
                set: { new in if let i = rows.firstIndex(where: { $0.id == id }) { rows[i].draft = new } })
    }

    private func move(_ id: UUID, by offset: Int) {
        guard let i = rows.firstIndex(where: { $0.id == id }) else { return }
        let j = min(max(i + offset, 0), rows.count - 1)
        let row = rows.remove(at: i)
        rows.insert(row, at: j)
    }

    private func save() {
        let updated = Preset(id: original.id, name: name, steps: steps)
        var list = model.presets
        if let i = list.firstIndex(where: { $0.id == original.id }) { list[i] = updated } else { list.append(updated) }
        if let e = model.savePresets(list) { error = e } else { dismiss() }
    }
}

@MainActor struct StepEditorRow: View {
    @Binding var draft: StepDraft
    let number: Int
    let loc: Loc
    let canMoveUp: Bool
    let canMoveDown: Bool
    let canRemove: Bool
    let move: (Int) -> Void
    let remove: () -> Void

    /// "Image › Resize": the same task name exists for images and videos.
    private func title(_ task: String) -> String {
        let group = task.hasPrefix("json.") ? "text" : String(task.split(separator: ".").first ?? "")
        return "\(loc.t("menu.\(group)")) › \(loc.t("menu.\(task)"))"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("\(number).").monospacedDigit()
                Picker("", selection: Binding(get: { draft.task }, set: { draft = StepDraft(task: $0) })) {
                    ForEach(Pipeline.chainableTasks, id: \.self) { Text(title($0)).tag($0) }
                }
                .labelsHidden()
                Button { move(-1) } label: { Image(systemName: "arrow.up") }.disabled(!canMoveUp).help(loc.t("ux.presets.moveUp"))
                Button { move(1) } label: { Image(systemName: "arrow.down") }.disabled(!canMoveDown).help(loc.t("ux.presets.moveDown"))
                Button { remove() } label: { Image(systemName: "minus.circle") }.disabled(!canRemove).help(loc.t("ux.presets.removeStep"))
            }
            fields.padding(.leading, 24)
        }
    }

    @ViewBuilder private var fields: some View {
        switch draft.task {
        case "image.resize", "video.resize":
            Picker("", selection: $draft.resizeMode) {
                Text(loc.t("ux.presets.percent")).tag(StepDraft.ResizeMode.percent)
                Text(loc.t("ux.presets.pixels")).tag(StepDraft.ResizeMode.pixels)
            }
            .pickerStyle(.segmented).labelsHidden().frame(maxWidth: 260)
            if draft.resizeMode == .percent {
                TextField(loc.t("ux.presets.scale"), value: $draft.scale, format: .number)
            } else {
                HStack {
                    TextField(loc.t("ux.presets.width"), value: $draft.width, format: .number)
                    TextField(loc.t("ux.presets.height"), value: $draft.height, format: .number)
                }
                Toggle(loc.t("ux.presets.keep"), isOn: $draft.keepAspect)
            }
        case "image.rotate", "video.rotate":
            TextField(loc.t("ux.presets.degrees"), value: $draft.degrees, format: .number)
        case "image.compress":
            TextField(loc.t("ux.presets.megabytes"), value: $draft.megabytes, format: .number)
        case "video.compress":
            Toggle(loc.t("ux.presets.discord"), isOn: $draft.discord)
            if !draft.discord { TextField(loc.t("ux.presets.megabytes"), value: $draft.megabytes, format: .number) }
        default:
            if let targets = Pipeline.targets[draft.task] {
                Picker(loc.t("ux.presets.target"), selection: $draft.target) {
                    ForEach(targets, id: \.self) { Text($0).tag($0) }
                }
                .frame(maxWidth: 260)
            }
        }
    }
}
