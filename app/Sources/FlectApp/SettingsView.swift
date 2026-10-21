import SwiftUI
import FlectCore

/// Edits settings.json through the generic tree, so keys this screen does not know survive a save.
@MainActor
final class SettingsEditor: ObservableObject {
    let store: SettingsStore
    @Published var dirty = false
    @Published var message: String?
    @Published var leaves: [MenuEntry] = []
    @Published var deps: [DependencyStatus] = []
    var requiredMissing: Bool { deps.contains { $0.required && !$0.ok } }
    @Published var quickActions = 0
    @Published var installing = false
    @Published var setupMessage: String?

    func refreshQuickActions() { quickActions = QuickActionInstaller.installedCount() }

    /// Generates the Quick Actions with the bundled flect-tools, then restarts Finder so they appear.
    func installQuickActions() async {
        installing = true
        setupMessage = nil
        let loc = Loc.shared
        do {
            let n = try await Task.detached(priority: .userInitiated) { try QuickActionInstaller.install() }.value
            await Task.detached(priority: .userInitiated) { QuickActionInstaller.refreshFinder() }.value
            quickActions = n
            setupMessage = loc.f("setup.step1.done", [String(n)])
        } catch {
            refreshQuickActions()
            setupMessage = loc.t("setup.step1.failed")
        }
        installing = false
    }

    /// Where the fetch scripts are: inside the app bundle, or the repository's scripts folder when run from a build.
    static var scriptsFolder: String {
        if let r = Bundle.main.resourcePath, FileManager.default.fileExists(atPath: r + "/scripts") { return r + "/scripts" }
        return "scripts"
    }

    func refreshDependencies() async {
        let dir = store.directory
        let model = store.string("Video", "Subtitles", "Model", default: "Small")
        let scripts = SettingsEditor.scriptsFolder
        let result = await Task.detached(priority: .utility) {
            DependencyCheck.check(dataDir: dir, modelSetting: model, scripts: scripts)
        }.value
        deps = result
    }

    init(store: SettingsStore) { self.store = store }

    func string(_ f: Field) -> String { (store.get(path: f.path) as? String) ?? (f.def as? String) ?? "" }
    func bool(_ f: Field) -> Bool { (store.get(path: f.path) as? Bool) ?? (f.def as? Bool) ?? false }
    func number(_ f: Field) -> Double { (store.get(path: f.path) as? NSNumber)?.doubleValue ?? (f.def as? NSNumber)?.doubleValue ?? 0 }

    func set(_ f: Field, _ value: Any) { store.set(f.path, value); dirty = true; message = nil; objectWillChange.send() }

    func setNumber(_ f: Field, _ v: Double) {
        guard v.isFinite else { return }
        switch f.kind {
        case .int(let r): set(f, min(max(Int(v.rounded()), r.lowerBound), r.upperBound))
        case .double(let r): set(f, min(max(v, r.lowerBound), r.upperBound))
        case .mib: set(f, Int64(min(max(v, 0.001), 100_000) * 1_048_576))
        default: break
        }
    }

    func mib(_ f: Field) -> Double {
        if let n = store.get(path: f.path) as? NSNumber { return n.doubleValue / 1_048_576 }
        return (f.def as? NSNumber)?.doubleValue ?? 0
    }

    func isHidden(_ id: String) -> Bool { store.hiddenMenuItems.contains(id) }

    func setHidden(_ id: String, _ hidden: Bool) {
        var set = store.hiddenMenuItems
        if hidden { set.insert(id) } else { set.remove(id) }
        store.set(["General", "HiddenMenuItems"], set.sorted())
        dirty = true; objectWillChange.send()
    }

    func save(loc: Loc) {
        do {
            try store.save()
            Loc.shared.use(store.language)
            dirty = false
            message = loc.t("common.ok")
        } catch {
            message = "\(error)"
        }
    }

    func reset() { store.reset(); dirty = true; objectWillChange.send() }
}

@MainActor
struct SettingsView: View {
    let model: AppModel
    @StateObject private var editor: SettingsEditor
    @State private var tab = "general"
    @State private var confirmReset = false

    init(model: AppModel) {
        self.model = model
        _editor = StateObject(wrappedValue: SettingsEditor(store: model.settings))
    }

    var body: some View {
        let loc = Loc.shared
        VStack(spacing: 0) {
            if editor.quickActions == 0 {
                Text(loc.t("setup.banner")).font(.callout).foregroundStyle(.orange).padding(.horizontal, 12).padding(.top, 8)
            }
            if editor.requiredMissing {
                Text(loc.t("deps.banner")).font(.callout).foregroundStyle(.orange).padding(.horizontal, 12).padding(.top, 8)
            }
            TabView(selection: $tab) {
                SetupView(editor: editor)
                    .tabItem { Text(text(loc, "setup.tab", "Getting started")) }
                    .tag("setup")
                ForEach(SettingsSchema.sections) { section in
                    Form {
                        ForEach(section.fields) { f in FieldRow(field: f, editor: editor) }
                    }
                    .formStyle(.grouped)
                    .tabItem { Text(text(loc, section.key, section.fallback)) }
                    .tag(section.id)
                }
                DependenciesView(editor: editor)
                    .tabItem { Text(text(loc, "deps.tab", "Required tools")) }
                    .tag("deps")
                MenuItemsView(editor: editor, model: model)
                    .tabItem { Text(text(loc, "settings.general.menu", "Menu items")) }
                    .tag("menu")
            }
            Divider()
            HStack {
                Text(loc.t("app.noTask")).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                Spacer()
                if let m = editor.message { Text(m).foregroundStyle(.green) }
                Button(loc.t("settings.reset")) { confirmReset = true }
                Button(loc.t("common.save")) { editor.save(loc: loc) }.keyboardShortcut(.defaultAction).disabled(!editor.dirty)
                Button(loc.t("common.close")) { model.close() }
            }.padding(12)
        }
        .confirmationDialog(loc.t("settings.reset.confirm"), isPresented: $confirmReset) {
            Button(loc.t("settings.reset"), role: .destructive) { editor.reset() }
            Button(loc.t("common.cancel"), role: .cancel) {}
        }
        .frame(minWidth: 620, minHeight: 460)
        .task {
            editor.refreshQuickActions()
            if editor.quickActions == 0 { tab = "setup" }   // first run: start with the setup steps
            await editor.refreshDependencies()
        }
    }
}

private func text(_ loc: Loc, _ key: String, _ fallback: String) -> String { loc.has(key) ? loc.t(key) : fallback }

@MainActor
struct FieldRow: View {
    let field: Field
    @ObservedObject var editor: SettingsEditor

    /// Preset slots share one caption ("Resize"), so the slot letter is appended to tell them apart.
    static func label(for f: Field, loc: Loc) -> String {
        let base = text(loc, f.key, f.fallback)
        guard f.path.contains("Presets"), let last = f.path.last, let letter = last.last, "ABCD".contains(letter) else { return base }
        return "\(base) \(letter)"
    }

    var body: some View {
        let loc = Loc.shared
        let label = FieldRow.label(for: field, loc: loc)
        switch field.kind {
        case .toggle:
            Toggle(label, isOn: Binding(get: { editor.bool(field) }, set: { editor.set(field, $0) }))
        case .text:
            TextField(label, text: Binding(get: { editor.string(field) }, set: { editor.set(field, String($0.prefix(1024))) }))
        case .int, .double:
            TextField(label, value: Binding(get: { editor.number(field) }, set: { editor.setNumber(field, $0) }), format: .number)
        case .mib:
            TextField(label, value: Binding(get: { editor.mib(field) }, set: { editor.setNumber(field, $0) }), format: .number)
        case .choice(let options):
            Picker(label, selection: Binding(get: { editor.string(field) }, set: { editor.set(field, $0) })) {
                ForEach(options, id: \.value) { o in Text(text(loc, o.key, o.fallback)).tag(o.value) }
            }
        }
    }
}

@MainActor
struct DependenciesView: View {
    @ObservedObject var editor: SettingsEditor

    var body: some View {
        let loc = Loc.shared
        VStack(alignment: .leading, spacing: 8) {
            Text(loc.t("deps.hint")).font(.caption).foregroundStyle(.secondary).padding(.horizontal)
            List(editor.deps) { d in
                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Image(systemName: d.ok ? "checkmark.circle.fill" : "xmark.circle.fill").foregroundStyle(d.ok ? Color.green : (d.required ? Color.red : Color.orange))
                        Text(d.name).font(.headline)
                        Text("· " + loc.t(d.neededForKey) + " · " + loc.t(d.required ? "deps.required" : "deps.optional")).font(.caption).foregroundStyle(.secondary)
                        Spacer()
                        Text(loc.t(d.ok ? "deps.ok" : "deps.missing")).font(.caption)
                    }
                    if !d.ok {
                        HStack {
                            Text(d.command).font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                            Spacer()
                            Button(loc.t("deps.copy")) {
                                NSPasteboard.general.clearContents()
                                NSPasteboard.general.setString(d.command, forType: .string)
                            }
                        }
                    }
                }.padding(.vertical, 2)
            }
            HStack { Spacer(); Button(loc.t("deps.recheck")) { Task { await editor.refreshDependencies() } } }.padding(.horizontal)
        }
        .padding(.vertical, 8)
        .task { await editor.refreshDependencies() }
    }
}

@MainActor
struct MenuItemsView: View {
    @ObservedObject var editor: SettingsEditor
    let model: AppModel

    var body: some View {
        let loc = Loc.shared
        VStack(alignment: .leading) {
            Text(loc.t("settings.general.menu.hint")).font(.caption).foregroundStyle(.secondary).padding(.horizontal)
            List(editor.leaves, id: \.id) { entry in
                Toggle(isOn: Binding(get: { !editor.isHidden(entry.id) }, set: { editor.setHidden(entry.id, !$0) })) {
                    Text(entry.id).font(.system(.body, design: .monospaced))
                }
            }
        }
        .task { editor.leaves = await model.menuLeaves() }
    }
}

/// First-run steps: install the Quick Actions, switch them on in System Settings, then use them.
@MainActor
struct SetupView: View {
    @ObservedObject var editor: SettingsEditor

    var body: some View {
        let loc = Loc.shared
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                Text(loc.t("setup.intro")).font(.callout).foregroundStyle(.secondary)

                VStack(alignment: .leading, spacing: 6) {
                    Text(loc.t("setup.step1.title")).font(.headline)
                    Text(loc.t("setup.step1.body")).font(.callout)
                    HStack(spacing: 10) {
                        Button(loc.t(editor.quickActions == 0 ? "setup.step1.button" : "setup.step1.reinstall")) {
                            Task { await editor.installQuickActions() }
                        }
                        .disabled(editor.installing)
                        if editor.installing { ProgressView().controlSize(.small) }
                        Text(editor.quickActions == 0 ? loc.t("setup.step1.none") : loc.f("setup.step1.ok", [String(editor.quickActions)]))
                            .font(.caption).foregroundStyle(editor.quickActions == 0 ? Color.orange : Color.green)
                    }
                    if let m = editor.setupMessage { Text(m).font(.caption).foregroundStyle(.secondary) }
                }

                VStack(alignment: .leading, spacing: 6) {
                    Text(loc.t("setup.step2.title")).font(.headline)
                    Text(loc.t("setup.step2.body")).font(.callout)
                    Button(loc.t("setup.step2.button")) {
                        if !NSWorkspace.shared.open(QuickActionInstaller.finderExtensionsSettingsURL) {
                            NSWorkspace.shared.open(QuickActionInstaller.loginItemsSettingsURL)
                        }
                    }
                }

                VStack(alignment: .leading, spacing: 6) {
                    Text(loc.t("setup.step3.title")).font(.headline)
                    Text(loc.t("setup.step3.body")).font(.callout)
                }
            }
            .padding(20)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task { editor.refreshQuickActions() }
    }
}
