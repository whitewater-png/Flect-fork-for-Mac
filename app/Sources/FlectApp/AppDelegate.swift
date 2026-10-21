import AppKit
import SwiftUI
import FlectCore

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var window: NSWindow?
    private var model: AppModel?

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
        installMenu()
        model.start(arguments: Array(CommandLine.arguments.dropFirst()))
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
        let editItem = NSMenuItem()
        main.addItem(editItem)
        let edit = NSMenu(title: "Edit")
        edit.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        edit.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        editItem.submenu = edit
        NSApp.mainMenu = main
    }
}
