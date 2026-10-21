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
    /// Installed Flect Quick Actions made by an older version (they must be installed again).
    @Published var outdatedQuickActions = 0
    @Published var updateChecking = false
    @Published var updateResult: UpdateResult?
    @Published var autoCheckUpdates = UserDefaults.standard.bool(forKey: UpdateChecker.autoCheckKey)

    static var appVersion: String { (Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String) ?? "0.0.0-dev" }

    func setAutoCheck(_ on: Bool) {
        autoCheckUpdates = on
        UserDefaults.standard.set(on, forKey: UpdateChecker.autoCheckKey)
    }

    /// `automatic` runs stay silent when nothing is new or the network is unavailable.
    func checkForUpdates(automatic: Bool = false) async {
        updateChecking = true
        if !automatic { updateResult = nil }
        let result = await UpdateChecker.check(current: SettingsEditor.appVersion)
        UserDefaults.standard.set(Date(), forKey: UpdateChecker.lastCheckKey)
        switch result {
        case .available(let i): UserDefaults.standard.set(i.tag, forKey: UpdateChecker.knownLatestKey)
        case .upToDate: UserDefaults.standard.removeObject(forKey: UpdateChecker.knownLatestKey)
        case .failed: break
        }
        if !(automatic && result == .failed) { updateResult = result }
        updateChecking = false
    }

    /// A newer release found by an earlier check, still newer than the running version.
    func loadKnownUpdate() {
        guard updateResult == nil, let tag = UserDefaults.standard.string(forKey: UpdateChecker.knownLatestKey),
              let info = UpdateChecker.releaseInfo(forKnownTag: tag) else { return }
        if case .available = UpdateChecker.decide(current: SettingsEditor.appVersion, latest: info) { updateResult = .available(info) }
        else { UserDefaults.standard.removeObject(forKey: UpdateChecker.knownLatestKey) }
    }

    var availableUpdate: ReleaseInfo? { if case .available(let i)? = updateResult { return i } else { return nil } }
    @Published var installing = false
    @Published var setupMessage: String?

    func refreshQuickActions() {
        quickActions = QuickActionInstaller.installedCount()
        outdatedQuickActions = QuickActionInstaller.outdatedCount()
    }

    /// Generates the Quick Actions with the bundled flect-tools, then restarts Finder so they appear.
    func installQuickActions() async {
        installing = true
        setupMessage = nil
        let loc = Loc.shared
        do {
            let n = try await Task.detached(priority: .userInitiated) { try QuickActionInstaller.install() }.value
            await Task.detached(priority: .userInitiated) { QuickActionInstaller.refreshFinder() }.value
            quickActions = n
            outdatedQuickActions = QuickActionInstaller.outdatedCount()
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
            if let u = editor.availableUpdate {
                Text(Loc.shared.f("update.banner", [u.version.description, SettingsEditor.appVersion])).font(.callout).foregroundStyle(.blue).padding(.horizontal, 12).padding(.top, 8)
            }
            if editor.quickActions == 0 {
                Text(loc.t("setup.banner")).font(.callout).foregroundStyle(.orange).padding(.horizontal, 12).padding(.top, 8)
            } else if editor.outdatedQuickActions > 0 {
                Text(loc.t("ux.setup.outdatedBanner")).font(.callout).foregroundStyle(.orange).padding(.horizontal, 12).padding(.top, 8)
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
                        if section.id == "output" {
                            Text(text(loc, "settings.output.hint", "Leave a field empty to save beside the original.")).font(.caption).foregroundStyle(.secondary)
                        }
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
                PresetsView(model: model)
                    .tabItem { Text(text(loc, "ux.presets.tab", "Presets")) }
                    .tag("presets")
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
            editor.loadKnownUpdate()
            if editor.quickActions == 0 || editor.outdatedQuickActions > 0 { tab = "setup" }   // first run, or Quick Actions to reinstall: start with the setup steps
            await editor.refreshDependencies()
            let last = UserDefaults.standard.object(forKey: UpdateChecker.lastCheckKey) as? Date
            if UpdateChecker.shouldAutoCheck(enabled: editor.autoCheckUpdates, last: last) { await editor.checkForUpdates(automatic: true) }
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

    /// An absolute folder picked in the standard panel (a typed relative path stays possible by hand).
    static func chooseFolder() -> String? {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false; panel.canChooseDirectories = true; panel.canCreateDirectories = true; panel.allowsMultipleSelection = false
        return panel.runModal() == .OK ? panel.url?.path : nil
    }

    var body: some View {
        let loc = Loc.shared
        let label = FieldRow.label(for: field, loc: loc)
        switch field.kind {
        case .toggle:
            Toggle(label, isOn: Binding(get: { editor.bool(field) }, set: { editor.set(field, $0) }))
        case .text:
            TextField(label, text: Binding(get: { editor.string(field) }, set: { editor.set(field, String($0.prefix(1024))) }))
        case .folder:
            HStack {
                TextField(label, text: Binding(get: { editor.string(field) }, set: { editor.set(field, String($0.prefix(1024))) }), prompt: Text("./Folder"))
                Button(loc.t("common.browse")) { if let path = FieldRow.chooseFolder() { editor.set(field, path) } }
            }
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
                    if editor.outdatedQuickActions > 0 {
                        Text(loc.n("ux.setup.outdated", editor.outdatedQuickActions)).font(.caption).foregroundStyle(Color.orange)
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

                Divider()
                UpdateSection(editor: editor)
            }
            .padding(20)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task { editor.refreshQuickActions() }
    }
}

/// "Is there a newer version?" Manual by default; the check at launch is opt-in. Nothing is installed automatically.
@MainActor
struct UpdateSection: View {
    @ObservedObject var editor: SettingsEditor

    var body: some View {
        let loc = Loc.shared
        VStack(alignment: .leading, spacing: 6) {
            Text(loc.t("update.title")).font(.headline)
            Text(loc.f("update.current", [SettingsEditor.appVersion])).font(.callout)
            HStack(spacing: 10) {
                Button(loc.t("update.button")) { Task { await editor.checkForUpdates() } }.disabled(editor.updateChecking)
                if editor.updateChecking { ProgressView().controlSize(.small); Text(loc.t("update.checking")).font(.caption).foregroundStyle(.secondary) }
                switch editor.updateResult {
                case .upToDate?: Text(loc.t("update.upToDate")).font(.caption).foregroundStyle(.green)
                case .failed?: Text(loc.t("update.failed")).font(.caption).foregroundStyle(.orange)
                case .available(let i)?:
                    Text(loc.f("update.available", [i.version.description])).font(.caption).foregroundStyle(.blue)
                    Button(loc.t("update.open")) { NSWorkspace.shared.open(i.url) }
                case nil: EmptyView()
                }
            }
            Toggle(loc.t("update.auto"), isOn: Binding(get: { editor.autoCheckUpdates }, set: { editor.setAutoCheck($0) }))
            Text(loc.t("update.privacy")).font(.caption).foregroundStyle(.secondary)
        }
    }
}
