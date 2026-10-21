import XCTest
@testable import FlectCore

/// Presets, pipelines and the picker's favorites / "Run again" (docs/mac-port/17-presets-pipeline.ja.md).
final class PresetPipelineTests: XCTestCase {
    func tmp() -> URL {
        let d = FileManager.default.temporaryDirectory.appendingPathComponent("flect-presets-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    /// core/flect-core/tests/fixtures/presets: the same files the Rust parser is tested with.
    var fixtures: URL {
        URL(fileURLWithPath: #filePath).deletingLastPathComponent().appendingPathComponent("../../../core/flect-core/tests/fixtures/presets").standardizedFileURL
    }

    func catalog() throws -> MenuCatalog {
        func leaf(_ id: String) -> [String: Any] { ["id": id] }
        func group(_ id: String, _ kids: [[String: Any]]) -> [String: Any] { ["id": id, "children": kids] }
        let entries: [[String: Any]] = [
            group("folder", [leaf("folder.removeDuplicate"), leaf("folder.tree")]),
            group("image", [group("image.resize", [leaf("image.resize.p1")]), group("image.convert", [leaf("image.convert.PNG")]), leaf("image.strip"), leaf("image.ocr")]),
            group("video", [leaf("video.trim"), group("video.compress", [leaf("video.compress.p1")]), group("video.convert", [leaf("video.convert.M4A")])]),
            group("audio", [group("audio.convert", [leaf("audio.convert.MP3")])]),
            group("text", [group("text.encode", [leaf("text.encode.UTF8")]), leaf("json.pretty"), leaf("json.sort")]),
        ]
        return try XCTUnwrap(MenuCatalog(menu: JSONValue(any: ["entries": entries])))
    }

    func selection(_ names: [String]) throws -> PickerSelection {
        TaskPicker.selection(for: names.map { SelectionItem(path: "/Users/a/\($0)", isDirectory: false) }, catalog: try catalog())
    }

    func ok(_ s: String) -> Bool { Pipeline.isValidStep(s) }

    // MARK: steps and chains (same cases as pipeline.rs)

    func testStepGrammarMatchesTheEngine() {
        for s in ["image.resize?scale=50", "image.resize?scale=12.5", "image.resize?scale=custom&w=1080&h=1080&keep=1",
                  "image.resize?scale=custom&w=0&h=600", "image.rotate?deg=-90", "image.compress?bytes=2097152",
                  "image.convert?to=JPG", "image.strip", "video.compress?preset=discord", "video.compress?bytes=1048576",
                  "video.convert?to=M4A", "audio.convert?to=MP3", "text.encode?to=SJIS", "json.sort"] {
            XCTAssertTrue(ok(s), s)
        }
        for s in ["", "image.resize", "image.resize?", "image.resize?scale=0", "image.resize?scale=1e3", "image.resize?scale=nan",
                  "image.resize?scale=50&w=10", "image.resize?scale=custom&w=0&h=0", "image.resize?scale=custom&w=40000&h=1",
                  "image.resize?scale=custom&w=10&h=10&keep=2", "image.resize?scale=50&scale=60", "image.resize?scale=50&x=1",
                  "image.resize?scale=%35%30", "image.rotate?deg=720", "image.compress?bytes=0", "image.compress?bytes=-5",
                  "image.convert?to=jpg", "image.convert?to=HEIC", "image.strip?x=1", "video.compress?bytes=1&preset=discord",
                  "video.compress?preset=other", "text.lineEndings?to=CR", "json.pretty?"] {
            guard case .failure(.badStep) = Pipeline.parse(s) else { XCTFail("accepted \(s)"); continue }
        }
        for s in ["folder.removeDuplicate?mode=all", "archive.extract", "image.ocr", "video.trim?start=0&end=1", "image.resize.p1", "../x"] {
            guard case .failure(.notAllowed) = Pipeline.parse(s) else { XCTFail("allowed \(s)"); continue }
        }
        XCTAssertFalse(ok(String(repeating: "a", count: Pipeline.maxStepBytes + 1)))
    }

    func testChainsAreBoundedAndKindsMustConnect() {
        func planOK(_ s: [String]) -> Bool { if case .success = Pipeline.plan(s) { return true }; return false }
        XCTAssertTrue(planOK(["image.resize?scale=50", "image.convert?to=JPG", "image.strip"]))
        XCTAssertTrue(planOK(["video.convert?to=M4A", "audio.convert?to=MP3"]))
        XCTAssertTrue(planOK(["video.convert?to=GIF", "image.resize?scale=50"]))
        XCTAssertTrue(planOK(Array(repeating: "image.strip", count: Pipeline.maxSteps)))
        XCTAssertEqual(Pipeline.plan([]).failureValue, .stepCount)
        XCTAssertEqual(Pipeline.plan(Array(repeating: "image.strip", count: Pipeline.maxSteps + 1)).failureValue, .stepCount)
        XCTAssertEqual(Pipeline.plan(["image.strip", "video.resize?scale=50"]).failureValue, .incompatible("image.strip", "video.resize?scale=50"))
        XCTAssertEqual(Pipeline.plan(["text.encode?to=UTF8", "image.strip"]).failureValue?.key, "msg.pipelineIncompatible")
    }

    func testEveryChainableTaskCanBeEditedAndRoundTrips() {
        for task in Pipeline.chainableTasks {
            let built = StepDraft(task: task).build()
            XCTAssertTrue(ok(built), "default \(task) -> \(built)")
            XCTAssertEqual(StepDraft(step: built)?.build(), built, task)
        }
        for s in ["image.resize?scale=custom&w=1080&h=0&keep=0", "image.resize?scale=12.5", "video.compress?preset=discord",
                  "image.compress?bytes=2097152", "video.rotate?deg=-45", "text.lineEndings?to=LF"] {
            XCTAssertEqual(StepDraft(step: s)?.build(), s)
        }
        var d = StepDraft(task: "image.resize")
        d.scale = 0.0001
        XCTAssertTrue(ok(d.build()), "tiny values are clamped, never written as 1e-04")
        d.resizeMode = .pixels; d.width = 99999; d.height = 10
        XCTAssertTrue(ok(d.build()))
        XCTAssertNil(StepDraft(step: "folder.tree"))
    }

    func testStepsAreDescribedForHumans() {
        let loc = Loc(language: .english)
        XCTAssertTrue(StepText.describe("image.resize?scale=50", loc: loc).hasSuffix("50%"))
        XCTAssertTrue(StepText.describe("image.resize?scale=custom&w=1080&h=1080&keep=1", loc: loc).hasSuffix("1080×1080"))
        XCTAssertTrue(StepText.describe("image.convert?to=JPG", loc: loc).contains("JPG"))
        XCTAssertEqual(StepText.describe(steps: ["image.strip", "json.sort"], loc: loc).components(separatedBy: " → ").count, 2)
    }

    // MARK: presets.json

    func testSharedFixturesAgreeWithTheEngine() throws {
        let files = try FileManager.default.contentsOfDirectory(atPath: fixtures.path).sorted()
        XCTAssertTrue(files.contains { $0.hasPrefix("invalid-") } && files.contains { $0.hasPrefix("valid-") })
        for f in files {
            let data = try Data(contentsOf: fixtures.appendingPathComponent(f))
            switch (f.split(separator: "-").first, PresetFile.decode(data)) {
            case ("valid"?, .success): break
            case ("invalid"?, .failure(.invalid)): break
            case ("newer"?, .failure(.newer)): break
            case (_, let r): XCTFail("\(f): \(r)")
            }
        }
        let basic = try PresetFile.decode(Data(contentsOf: fixtures.appendingPathComponent("valid-basic.json"))).get()
        XCTAssertEqual(basic.map { $0.name }, ["SNS 1080px / 2MB", "写真 → JPG（情報削除）"])
        XCTAssertEqual(try PresetFile.decode(PresetFile.encode(basic)).get(), basic, "what the app writes, it (and the engine) reads back")
    }

    func testSavesPrivatelyAndSetsABrokenFileAsideInsteadOfOverwritingIt() throws {
        let dir = tmp()
        let file = dir.appendingPathComponent("presets.json")
        try Data("{ not json".utf8).write(to: file)
        let store = PresetStore(directory: dir)
        guard case .broken = store.state else { return XCTFail("\(store.state)") }
        XCTAssertEqual(store.presets, [])
        XCTAssertEqual(try String(contentsOf: file, encoding: .utf8), "{ not json", "loading changes nothing")
        let p = Preset(name: "Web", steps: ["image.resize?scale=50", "image.strip"])
        try store.save([p])
        XCTAssertEqual(try String(contentsOf: dir.appendingPathComponent("presets.json.broken"), encoding: .utf8), "{ not json")
        let mode = try FileManager.default.attributesOfItem(atPath: file.path)[.posixPermissions] as? NSNumber
        XCTAssertEqual(mode?.intValue, 0o600)
        let again = PresetStore(directory: dir)
        XCTAssertEqual(again.state, .ok)
        XCTAssertEqual(again.presets, [p])
        // a second broken file does not replace the first one set aside
        try Data("[]".utf8).write(to: file)
        again.reload()
        try again.save([p], now: Date(timeIntervalSince1970: 0))
        let names = try FileManager.default.contentsOfDirectory(atPath: dir.path).sorted()
        XCTAssertEqual(names.filter { $0.hasPrefix("presets.json.broken") }.count, 2, "\(names)")
        XCTAssertFalse(names.contains { $0.hasSuffix(".tmp") })
    }

    func testANewerFileIsReadOnly() throws {
        let dir = tmp()
        let file = dir.appendingPathComponent("presets.json")
        let body = #"{"version":2,"presets":[],"folders":[]}"#
        try Data(body.utf8).write(to: file)
        let store = PresetStore(directory: dir)
        XCTAssertEqual(store.state, .newer(2))
        XCTAssertTrue(store.isReadOnly)
        XCTAssertThrowsError(try store.save([Preset(name: "x", steps: ["image.strip"])]))
        XCTAssertEqual(try String(contentsOf: file, encoding: .utf8), body)
    }

    func testInvalidListsAreNotSaved() throws {
        let dir = tmp()
        let store = PresetStore(directory: dir)
        XCTAssertEqual(store.state, .missing)
        for list in [[Preset(name: "a", steps: ["image.strip"]), Preset(name: "A", steps: ["image.strip"])],
                     [Preset(name: " padded", steps: ["image.strip"])],
                     [Preset(name: "x", steps: ["folder.removeDuplicate?mode=all"])],
                     [Preset(name: "x", steps: [])]] {
            XCTAssertThrowsError(try store.save(list))
        }
        XCTAssertFalse(FileManager.default.fileExists(atPath: store.url.path))
        XCTAssertTrue(PresetFile.isValidName("写真 → Web"))
        XCTAssertFalse(PresetFile.isValidName("gpj.\u{202E}exe"))
        XCTAssertFalse(PresetFile.isValidName(String(repeating: "a", count: 65)))
    }

    // MARK: favorites and "Run again"

    func testFavoritesAreBoundedOrderedAndCheckedOnLoad() throws {
        var m = PickerMemory()
        for i in 0..<PickerMemory.maxFavorites { XCTAssertTrue(m.pin(.task("image.resize.p\(i)"))) }
        XCTAssertFalse(m.pin(.task("image.strip")), "full")
        XCTAssertFalse(m.pin(.task("image.resize.p0")), "already pinned")
        XCTAssertFalse(PickerMemory.canPin(.step("image.strip")))
        m.move(.task("image.resize.p5"), by: -10)
        XCTAssertEqual(m.favorites.first, .task("image.resize.p5"))
        XCTAssertFalse(m.toggle(.task("image.resize.p5")))
        XCTAssertEqual(m.favorites.count, PickerMemory.maxFavorites - 1)
        let id = UUID()
        XCTAssertTrue(m.pin(.preset(id)))
        m.remember(.preset(id))
        let back = PickerMemory.decode(try m.encode())
        XCTAssertEqual(back, m)
        m.keepPresets([])
        XCTAssertFalse(m.isPinned(.preset(id)))
        XCTAssertNil(m.last)

        // a hostile picker.json: only values that pass the same checks survive
        let hostile = #"{"version":1,"favorites":["task:../../x","task:image.strip","step:image.strip","preset:nope","rm -rf"],"last":"step:folder.removeDuplicate?mode=all"}"#
        let h = PickerMemory.decode(Data(hostile.utf8))
        XCTAssertEqual(h.favorites, [.task("image.strip")])
        XCTAssertNil(h.last)
        XCTAssertNil(PickerRef(raw: "task:folder.removeEmpty?confirm=1"))
        XCTAssertEqual(PickerRef(raw: "step:image.resize?scale=50"), .step("image.resize?scale=50"))
        XCTAssertEqual(PickerMemory.decode(Data("garbage".utf8)), PickerMemory())
    }

    func testShortcutsShowOnlyWhereTheTaskIsOffered() throws {
        let png = try selection(["a.png", "b.png"])
        let json = try selection(["d.json"])
        let txt = try selection(["n.txt"])
        let video = try selection(["v.mp4"])
        XCTAssertEqual(PickerShortcuts.section(for: "image.resize", in: png)?.category, .image)
        XCTAssertEqual(PickerShortcuts.section(for: "image.resize.p1", in: png)?.paths, ["/Users/a/a.png", "/Users/a/b.png"])
        XCTAssertNil(PickerShortcuts.section(for: "image.resize", in: json))
        XCTAssertNotNil(PickerShortcuts.section(for: "json.pretty", in: json))
        XCTAssertNil(PickerShortcuts.section(for: "json.pretty", in: txt), "the picker's all-JSON rule applies")
        XCTAssertNil(PickerShortcuts.section(for: "video.trim", in: try selection(["v.mp4", "w.mp4"])), "single-file rule applies")

        let web = Preset(name: "Web", steps: ["image.resize?scale=50", "image.strip"])
        let audio = Preset(name: "Audio", steps: ["video.convert?to=M4A", "audio.convert?to=MP3"])
        XCTAssertEqual(PickerShortcuts.presets([web, audio], selection: png).map { $0.preset?.name }, ["Web"])
        XCTAssertEqual(PickerShortcuts.presets([web, audio], selection: video).map { $0.preset?.name }, ["Audio"])

        var m = PickerMemory(favorites: [.task("json.pretty"), .preset(web.id), .task("image.strip")])
        XCTAssertEqual(PickerShortcuts.favorites(m, presets: [web], selection: png).map { $0.ref }, [.preset(web.id), .task("image.strip")])
        XCTAssertEqual(PickerShortcuts.favorites(m, presets: [], selection: png).map { $0.ref }, [.task("image.strip")], "a deleted preset is not shown")
        m.remember(.step("image.resize?scale=50"))
        XCTAssertEqual(PickerShortcuts.runAgain(m, presets: [], selection: png)?.section.category, .image)
        XCTAssertNil(PickerShortcuts.runAgain(m, presets: [], selection: json))
    }
}

private extension Result {
    var failureValue: Failure? { if case .failure(let f) = self { return f }; return nil }
}
