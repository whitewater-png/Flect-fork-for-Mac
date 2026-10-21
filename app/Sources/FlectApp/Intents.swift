import AppIntents
import Foundation
import FlectCore

// Shortcuts actions (App Intents, macOS 14+). The decisions live in FlectCore/ShortcutsPlan.swift; this file is only
// the adapter: IntentFile in, IntentFile out. Each run starts its own flect-engine process (no shell), checks the
// files like every other entry point, and returns only files the engine reported as created.
//
// NOTE: Shortcuts finds these actions through Metadata.appintents inside the app bundle, which the Xcode build
// toolchain generates. `swift build` plus scripts/build.sh does not produce it; see docs/shortcuts.md.

enum IntentRunner {
    static func execute(files: [IntentFile], spec: ShortcutsSpec) async throws -> [IntentFile] {
        guard let exe = EngineClient.locate() else { throw ShortcutsError.engineMissing }
        let engine = EngineClient(executable: exe)
        let settings = SettingsStore()
        Loc.shared.use(settings.language)
        guard let menu = await engine.fetchMenu(), let catalog = MenuCatalog(menu: menu) else { throw ShortcutsError.engineMissing }

        // Decide the task first: an unknown or non-runnable task never touches the files.
        let plan = try ShortcutsPlan.plan(spec, catalog: catalog, settings: settings).get()

        let staging = FileManager.default.temporaryDirectory.appendingPathComponent("FlectShortcuts-\(UUID().uuidString)", isDirectory: true)
        var urls: [URL] = []
        var scoped: [URL] = []
        defer { for u in scoped { u.stopAccessingSecurityScopedResource() } }
        for (i, file) in files.enumerated() {
            if let url = file.fileURL {
                if url.startAccessingSecurityScopedResource() { scoped.append(url) }
                urls.append(url)
            } else {
                urls.append(try ShortcutsPlan.stage(name: file.filename, data: file.data, index: i, in: staging))
            }
        }
        let items = try ShortcutsPlan.items(from: urls).get()
        if let problem = ShortcutsPlan.check(items: items, catalogID: plan.catalogID, catalog: catalog, isAnimatedGIF: AppModel.isAnimatedGIF) { throw problem }

        let result = await HeadlessRunner.run(engine: engine, task: plan.engineTask, inputs: items.map { $0.path })
        switch result.outcome {
        case .succeeded: break
        case .cancelled: throw ShortcutsError.cancelled
        case .failed: throw ShortcutsError.engineFailed(result.summary)
        }
        return result.outputs.map { IntentFile(fileURL: URL(fileURLWithPath: $0), filename: nil, type: nil) }
    }
}

// MARK: the task list (from the engine's catalog)

struct FlectTaskEntity: AppEntity {
    static let typeDisplayRepresentation = TypeDisplayRepresentation(name: "Flect task")
    static let defaultQuery = FlectTaskQuery()

    var id: String
    var label: String

    var displayRepresentation: DisplayRepresentation { DisplayRepresentation(title: "\(label)") }
}

struct FlectTaskQuery: EntityQuery {
    private func all() async -> [FlectTaskEntity] {
        guard let exe = EngineClient.locate(), let menu = await EngineClient(executable: exe).fetchMenu(), let catalog = MenuCatalog(menu: menu) else { return [] }
        let settings = SettingsStore()
        Loc.shared.use(settings.language)
        return ShortcutsPlan.tasks(in: catalog, settings: settings, loc: Loc.shared).map { FlectTaskEntity(id: $0.id, label: $0.label) }
    }

    func entities(for identifiers: [String]) async throws -> [FlectTaskEntity] {
        let wanted = Set(identifiers)
        return await all().filter { wanted.contains($0.id) }
    }

    func suggestedEntities() async throws -> [FlectTaskEntity] { await all() }
}

// MARK: the actions

struct RunFlectTaskIntent: AppIntent {
    static let title: LocalizedStringResource = "Run a Flect Task on Files"
    static let description = IntentDescription("Runs one of Flect's file tasks (resize, convert, compress, remove metadata ...) and returns the files it created. Originals are never overwritten.")

    @Parameter(title: "Task")
    var task: FlectTaskEntity

    @Parameter(title: "Files", supportedTypeIdentifiers: ["public.item"])
    var files: [IntentFile]

    static var parameterSummary: some ParameterSummary { Summary("Run \(\.$task) on \(\.$files)") }

    func perform() async throws -> some IntentResult & ReturnsValue<[IntentFile]> {
        .result(value: try await IntentRunner.execute(files: files, spec: .catalogTask(id: task.id)))
    }
}

struct ResizeImagesIntent: AppIntent {
    static let title: LocalizedStringResource = "Resize Images with Flect"
    static let description = IntentDescription("Resizes images by a percentage, or to a width and/or height in pixels. Returns the new files.")

    @Parameter(title: "Images", supportedTypeIdentifiers: ["public.image"])
    var files: [IntentFile]

    @Parameter(title: "Percent", description: "Used when no width or height is given.", default: 50, inclusiveRange: (1, 1000))
    var percent: Int

    @Parameter(title: "Width (pixels)", inclusiveRange: (1, 32768))
    var width: Int?

    @Parameter(title: "Height (pixels)", inclusiveRange: (1, 32768))
    var height: Int?

    @Parameter(title: "Keep Aspect Ratio", default: true)
    var keepAspect: Bool

    static var parameterSummary: some ParameterSummary {
        Summary("Resize \(\.$files) to \(\.$percent) %") {
            \.$width
            \.$height
            \.$keepAspect
        }
    }

    func perform() async throws -> some IntentResult & ReturnsValue<[IntentFile]> {
        .result(value: try await IntentRunner.execute(files: files, spec: .resize(percent: percent, width: width, height: height, keepAspect: keepAspect)))
    }
}

enum FlectImageFormat: String, AppEnum {
    case png = "PNG", jpg = "JPG", webp = "WEBP", ico = "ICO", gif = "GIF", svg = "SVG"

    static let typeDisplayRepresentation = TypeDisplayRepresentation(name: "Image format")
    static let caseDisplayRepresentations: [FlectImageFormat: DisplayRepresentation] = [
        .png: "PNG", .jpg: "JPEG", .webp: "WebP", .ico: "ICO", .gif: "GIF", .svg: "SVG",
    ]
}

struct ConvertImagesIntent: AppIntent {
    static let title: LocalizedStringResource = "Convert Images with Flect"
    static let description = IntentDescription("Converts images to another format and returns the new files. (HEIC output needs the Flect window and is not offered here.)")

    @Parameter(title: "Images", supportedTypeIdentifiers: ["public.image"])
    var files: [IntentFile]

    @Parameter(title: "Format", default: .png)
    var format: FlectImageFormat

    static var parameterSummary: some ParameterSummary { Summary("Convert \(\.$files) to \(\.$format)") }

    func perform() async throws -> some IntentResult & ReturnsValue<[IntentFile]> {
        .result(value: try await IntentRunner.execute(files: files, spec: .convertImage(format: format.rawValue)))
    }
}

struct MakeImagesSafeToShareIntent: AppIntent {
    static let title: LocalizedStringResource = "Make Images Safe to Share with Flect"
    static let description = IntentDescription("Removes metadata (location, camera details, comments) from images according to Flect's metadata settings and returns the cleaned copies.")

    @Parameter(title: "Images", supportedTypeIdentifiers: ["public.image"])
    var files: [IntentFile]

    static var parameterSummary: some ParameterSummary { Summary("Make \(\.$files) safe to share") }

    func perform() async throws -> some IntentResult & ReturnsValue<[IntentFile]> {
        .result(value: try await IntentRunner.execute(files: files, spec: .stripMetadata))
    }
}
