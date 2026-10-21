import AppKit
import SwiftUI
import FlectCore

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var window: NSWindow?
    private var model: AppModel?
    private var historyWindow: NSWindow?
    private let services = ServicesProvider()
    /// Items that arrived (Dock drop, Services) before the window and model existed.
    private var pendingURLs: [URL] = []

    func applicationWillFinishLaunching(_ notification: Notification) {
        // "Process with Flect..." in the Services menu (NSServices in Info.plist).
        services.handler = { [weak self] urls in self?.present(urls: urls) }
        NSApp.servicesProvider = services
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        let model = AppModel()
        self.model = model
        let host = NSHostingController(rootView: RootView(model: model))
        // Without this the hosting controller resizes the window to the SwiftUI view's minimum (480x328),
        // overriding the sizes set below, so the settings window opened clipped.
        host.sizingOptions = []
        let w = NSWindow(contentViewController: host)
        w.title = "Flect"
        w.styleMask = [.titled, .closable, .miniaturizable, .resizable]
        let settingsMode = !CommandLine.arguments.contains("--task")
        w.setContentSize(settingsMode ? NSSize(width: 760, height: 580) : NSSize(width: 560, height: 420))
        w.minSize = settingsMode ? NSSize(width: 640, height: 480) : NSSize(width: 480, height: 320)
        w.center()
        w.isReleasedWhenClosed = false
        w.makeKeyAndOrderFront(nil)
        window = w
        NSApp.activate(ignoringOtherApps: true)
        model.start(arguments: Array(CommandLine.arguments.dropFirst()))
        installMenu() // after start(): the menu titles follow the language setting
        Notifier.shared.onOpen = { [weak self] id in self?.showHistory(focus: id) }
        Notifier.shared.attach()
        if !pendingURLs.isEmpty { let urls = pendingURLs; pendingURLs = []; present(urls: urls) }
    }

    /// Files / folders dropped on the Dock icon or on Flect.app, or opened with it from Finder.
    func application(_ application: NSApplication, open urls: [URL]) {
        present(urls: urls)
    }

    /// The one funnel for the new entry points: show the action picker (validation happens in `AppModel.offer`).
    private func present(urls: [URL]) {
        guard let model = model else { pendingURLs += urls; return }
        NSApp.activate(ignoringOtherApps: true)
        window?.makeKeyAndOrderFront(nil)
        model.offer(urls: urls)
    }

    @objc func openDocument(_ sender: Any?) {
        guard let model = model else { return }
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = true
        panel.message = model.loc.t("ux.open.prompt")
        panel.prompt = model.loc.t("ux.open.button")
        guard panel.runModal() == .OK else { return }
        present(urls: panel.urls)
    }

    @objc func showRecentRuns(_ sender: Any?) { showHistory(focus: nil) }

    private func showHistory(focus: UUID?) {
        guard let model = model else { return }
        model.cancelAutoClose()
        model.historyFocus = focus
        if historyWindow == nil {
            let host = NSHostingController(rootView: HistoryView(model: model))
            host.sizingOptions = []
            let w = NSWindow(contentViewController: host)
            w.styleMask = [.titled, .closable, .miniaturizable, .resizable]
            w.setContentSize(NSSize(width: 720, height: 400))
            w.center()
            w.isReleasedWhenClosed = false
            historyWindow = w
        }
        historyWindow?.title = model.loc.t("ux.history.title")
        NSApp.activate(ignoringOtherApps: true)
        historyWindow?.makeKeyAndOrderFront(nil)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    func applicationWillTerminate(_ notification: Notification) {
        model?.cancelRunning()
    }

    private func installMenu() {
        let main = NSMenu()
        let appItem = NSMenuItem()
        main.addItem(appItem)
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "Quit Flect", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appItem.submenu = appMenu
        let loc = Loc.shared
        let fileItem = NSMenuItem()
        main.addItem(fileItem)
        let file = NSMenu(title: loc.t("ux.menu.file"))
        let open = NSMenuItem(title: loc.t("ux.menu.open"), action: #selector(openDocument(_:)), keyEquivalent: "o")
        open.target = self
        file.addItem(open)
        let recent = NSMenuItem(title: loc.t("ux.menu.history"), action: #selector(showRecentRuns(_:)), keyEquivalent: "y")
        recent.target = self
        file.addItem(recent)
        fileItem.submenu = file
        let editItem = NSMenuItem()
        main.addItem(editItem)
        let edit = NSMenu(title: "Edit")
        edit.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        edit.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        editItem.submenu = edit
        NSApp.mainMenu = main
    }
}
