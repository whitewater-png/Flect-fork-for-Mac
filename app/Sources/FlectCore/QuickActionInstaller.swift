import Foundation

/// Installs Flect's Finder Quick Actions by running the bundled `flect-tools` (no shell, argument array),
/// the same thing the README's `flect-tools --out ~/Library/Services` does, so the app can offer a button for it.
public enum QuickActionInstaller {
    public static var servicesFolder: URL {
        FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Services", isDirectory: true)
    }

    /// `Flect Image Resize.workflow` and so on. The old proof-of-concept `Flect PoC.workflow` is not one of ours.
    public static func isFlectWorkflow(_ name: String) -> Bool {
        name.hasPrefix("Flect ") && name.hasSuffix(".workflow") && name != "Flect PoC.workflow"
    }

    public static func installedCount(in folder: URL = servicesFolder, fm: FileManager = .default) -> Int {
        ((try? fm.contentsOfDirectory(atPath: folder.path)) ?? []).filter(isFlectWorkflow).count
    }

    /// `flect-tools` inside the app bundle (Contents/Resources, or next to the executable).
    public static func locateTools(bundle: Bundle = .main) -> URL? {
        var candidates: [URL] = []
        if let r = bundle.resourceURL { candidates.append(r.appendingPathComponent("flect-tools")) }
        if let e = bundle.executableURL { candidates.append(e.deletingLastPathComponent().appendingPathComponent("flect-tools")) }
        return candidates.first { FileManager.default.isExecutableFile(atPath: $0.path) }
    }

    public enum InstallError: Error, Equatable {
        case toolsNotFound
        case failed(Int32)
    }

    /// Generates the Quick Actions into `folder` and returns how many Flect ones are there afterwards.
    /// Blocking: call it off the main thread.
    public static func install(tools: URL? = locateTools(), into folder: URL = servicesFolder) throws -> Int {
        guard let tools else { throw InstallError.toolsNotFound }
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let p = Process()
        p.executableURL = tools
        p.arguments = ["--out", folder.path]
        p.standardInput = FileHandle.nullDevice
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        try p.run()
        p.waitUntilExit()
        guard p.terminationStatus == 0 else { throw InstallError.failed(p.terminationStatus) }
        return installedCount(in: folder)
    }

    /// Asks macOS to re-read the services and restarts Finder so the new items show up (what the README does with `killall Finder`).
    public static func refreshFinder() {
        for (path, args) in [("/System/Library/CoreServices/pbs", ["-update"]), ("/usr/bin/killall", ["Finder"])] {
            guard FileManager.default.isExecutableFile(atPath: path) else { continue }
            let p = Process()
            p.executableURL = URL(fileURLWithPath: path)
            p.arguments = args
            p.standardOutput = FileHandle.nullDevice
            p.standardError = FileHandle.nullDevice
            try? p.run()
            p.waitUntilExit()
        }
    }

    /// System Settings → Login Items & Extensions → Finder extensions, where the Flect items are switched on.
    public static let finderExtensionsSettingsURL = URL(string: "x-apple.systempreferences:com.apple.ExtensionsPreferences?extensionPointIdentifier=com.apple.finder-extension")!
    public static let loginItemsSettingsURL = URL(string: "x-apple.systempreferences:com.apple.LoginItems-Settings.extension")!
}
