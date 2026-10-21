import XCTest
@testable import FlectCore

/// Privacy check, "make safe to share" and the duplicate-removal preview (planning and strings; the
/// engine side is covered by the Rust tests).
final class ShareSafeTests: XCTestCase {
    func tmp() -> URL {
        let d = FileManager.default.temporaryDirectory.appendingPathComponent("flect-share-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    func testShortcutsMakeSafeRunsTheVerifiedTaskNotPlainStrip() throws {
        let entries: [[String: Any]] = [["id": "image", "children": [["id": "image.strip"], ["id": "image.makeSafe"]]]]
        let catalog = try XCTUnwrap(MenuCatalog(menu: JSONValue(any: ["entries": entries])))
        let plan = try ShortcutsPlan.plan(.makeSafe, catalog: catalog, settings: SettingsStore(directory: tmp())).get()
        XCTAssertEqual(plan.catalogID, "image.makeSafe")
        XCTAssertEqual(plan.engineTask, "image.makeSafe")
    }

    func testPlannerRoutesTheShareSafeTasks() {
        let s = SettingsStore(directory: tmp())
        XCTAssertEqual(Planner.resolve("image.privacy", settings: s), .engine("image.privacy"))
        XCTAssertEqual(Planner.resolve("image.makeSafe", settings: s), .engine("image.makeSafe"))
        XCTAssertEqual(Planner.resolve("image.makeSafe.resize", settings: s), .dialog(.maxEdge))
        XCTAssertEqual(Planner.makeSafe(maxEdge: 0), "image.makeSafe")
        XCTAssertEqual(Planner.makeSafe(maxEdge: 2048), "image.makeSafe?maxEdge=2048")
        XCTAssertEqual(Planner.makeSafe(maxEdge: 1_000_000), "image.makeSafe?maxEdge=32768")
    }

    func testDuplicateRemovalAlwaysStartsAsAPreview() {
        let s = SettingsStore(directory: tmp())
        XCTAssertEqual(Planner.resolve("folder.removeDuplicate", settings: s), .engine("folder.removeDuplicate?dry=1"))
        XCTAssertEqual(Planner.duplicates(mode: "oldest", dryRun: true), "folder.removeDuplicate?mode=oldest&dry=1")
        // the confirmed run is the same task without dry
        XCTAssertEqual(Planner.duplicates(mode: "oldest"), "folder.removeDuplicate?mode=oldest")
    }

    func testPickerOffersTheShareTasksOnlyForFormatsTheEngineCanRead() {
        func members(_ f: String) -> [(path: String, format: String?)] { [("/a/x", f)] }
        for f in ["JPG", "PNG", "WEBP"] {
            XCTAssertTrue(TaskPicker.applies("image.privacy", to: members(f), isAnimatedGIF: { _ in false }), f)
            XCTAssertTrue(TaskPicker.applies("image.makeSafe", to: members(f), isAnimatedGIF: { _ in false }), f)
        }
        XCTAssertFalse(TaskPicker.applies("image.privacy", to: members("GIF"), isAnimatedGIF: { _ in false }))
        XCTAssertTrue(TaskPicker.applies("image.makeSafe", to: members("GIF"), isAnimatedGIF: { _ in false }))
        for id in ["image.privacy", "image.makeSafe", "image.makeSafe.resize"] {
            XCTAssertFalse(TaskPicker.applies(id, to: members("SVG"), isAnimatedGIF: { _ in false }), id)
            XCTAssertFalse(TaskPicker.applies(id, to: members("HEIC"), isAnimatedGIF: { _ in false }), id)
        }
    }

    func testShareStringsExistInAllLanguagesAndAreMerged() throws {
        func keys(_ code: String) throws -> Set<String> {
            let url = try XCTUnwrap(Bundle.module.url(forResource: "strings.share.\(code)", withExtension: "json", subdirectory: "Resources"))
            let obj = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: String])
            return Set(obj.keys)
        }
        let en = try keys("en")
        XCTAssertFalse(en.isEmpty)
        for code in ["ja", "zh"] { XCTAssertEqual(en.subtracting(try keys(code)).sorted(), [], "\(code) lacks keys") }
        // every privacy kind the engine can report has a label
        for kind in ["gps", "serial", "owner", "camera", "lens", "timestamps", "software", "thumbnail", "makernote", "description", "xmp", "iptc", "comment", "hidden", "extra"] {
            XCTAssertTrue(en.contains("share.kind.\(kind)"), kind)
        }
        for lang in [AppLanguage.english, .japanese, .chinese] {
            let loc = Loc(language: lang)
            for key in ["menu.image.privacy", "menu.image.makeSafe", "menu.image.makeSafe.resize", "msg.privacyFound", "msg.safeRemoved", "msg.duplicatesPreview"] {
                XCTAssertTrue(loc.has(key), "\(lang) \(key)")
            }
        }
        XCTAssertNotEqual(Loc(language: .japanese).t("menu.image.privacy"), "menu.image.privacy")
        XCTAssertEqual(Loc(language: .english).f("msg.safeResized", ["a.jpg", "64x32", "32x16"]), "a.jpg: resized from 64x32 to 32x16.")
    }
}
