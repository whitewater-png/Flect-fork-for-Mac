import AppKit
import ImageIO
import SwiftUI
import FlectCore

// Ways to start Flect besides a Finder Quick Action: dropping items on the Dock icon / Flect.app, File > Open... (⌘O)
// and the Services menu ("Process with Flect..."). All of them end in `AppModel.offer(urls:)`, which applies the same
// path rules as a Quick Action's selection file (LaunchRequest.validate), shows only tasks from the engine's catalog,
// and runs the chosen one through Planner.resolve exactly like a Quick Action does.

extension AppModel {
    /// Shows the action picker for these items. Returns false when it refused because a task is running.
    @discardableResult
    func offer(urls: [URL]) -> Bool {
        if case .running = screen, !finished {
            let alert = NSAlert()
            alert.messageText = loc.t("ux.picker.busy")
            alert.runModal()
            return false
        }
        switch EntryRequest.items(from: urls) {
        case .failure(let failure):
            screen = .message(loc.t(failure == .tooMany ? "ux.picker.tooMany" : "ux.picker.none"))
        case .success(let request):
            guard engine != nil else { screen = .message(loc.t("app.engineMissing")); return true }
            if case .settings = screen { returnScreen = .settings }
            screen = .idle
            Task {
                if catalog == nil, let m = await engine?.fetchMenu() { catalog = MenuCatalog(menu: m) }
                guard let catalog = catalog else { screen = .message(loc.t("app.engineMissing")); return }
                var selection = TaskPicker.selection(for: request.items, catalog: catalog, isAnimatedGIF: Self.isAnimatedGIF)
                selection.rejected = request.rejected
                screen = .pick(selection)
            }
        }
        return true
    }

    /// An animated GIF is not offered "Convert" (as in the Quick Actions).
    nonisolated static func isAnimatedGIF(_ path: String) -> Bool {
        guard let src = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil) else { return false }
        return CGImageSourceGetCount(src) > 1
    }

    /// The user picked a task for one group of the selection. `handle` is the same planner path a Quick Action uses.
    func runPicked(_ section: TaskSection, _ entry: MenuEntry) {
        paths = section.paths
        handle(task: entry.id)
    }

    func cancelPicker() {
        if let r = returnScreen { returnScreen = nil; screen = r } else { close() }
    }
}

/// NSServices target: "Process with Flect...".
final class ServicesProvider: NSObject {
    var handler: (([URL]) -> Void)?

    /// NSMessage `processWithFlect`.
    @objc func processWithFlect(_ pboard: NSPasteboard, userData: String, error: AutoreleasingUnsafeMutablePointer<NSString>) {
        let urls = (pboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL]) ?? []
        guard !urls.isEmpty else { error.pointee = "No files were passed to Flect." as NSString; return }
        DispatchQueue.main.async { self.handler?(urls) }
    }
}

@MainActor struct PickerView: View {
    let selection: PickerSelection
    let model: AppModel
    private var loc: Loc { model.loc }

    private var subject: String {
        selection.total == 1 ? selection.firstName : loc.f("ux.picker.many", [String(selection.total)])
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(loc.f("ux.picker.title", [subject])).font(.headline).lineLimit(2)
            if selection.sections.isEmpty {
                Text(loc.t("ux.picker.none")).foregroundStyle(.secondary).frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List {
                    ForEach(selection.sections) { section in
                        Section(selection.sections.count > 1 || section.paths.count > 1
                                ? loc.f("ux.picker.section", [loc.t("menu.\(section.category.rawValue)"), String(section.paths.count)])
                                : loc.t("menu.\(section.category.rawValue)")) {
                            ForEach(section.entries, id: \.id) { entry in
                                Button(model.label(for: entry)) { model.runPicked(section, entry) }.buttonStyle(.plain)
                            }
                        }
                    }
                }
            }
            if selection.unsupported > 0 { Text(loc.n("ux.picker.unsupported", selection.unsupported)).font(.caption).foregroundStyle(.secondary) }
            if selection.rejected > 0 { Text(loc.n("ux.picker.rejected", selection.rejected)).font(.caption).foregroundStyle(.secondary) }
            HStack { Spacer(); Button(loc.t("common.cancel")) { model.cancelPicker() }.keyboardShortcut(.cancelAction) }
        }.padding(16)
    }
}
