import XCTest
@testable import FlectCore

/// The app moved whatever path the engine named to the Trash, checking only that it started with `/`.
final class TrashGuardTests: XCTestCase {
    private func scratch() -> URL {
        let d = FileManager.default.temporaryDirectory.appendingPathComponent("flect-trash-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    private func file(_ url: URL, _ text: String = "x") {
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        FileManager.default.createFile(atPath: url.path, contents: Data(text.utf8))
    }

    private func dir(_ url: URL) { try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true) }

    func testKindComesFromTheTask() {
        XCTAssertEqual(TrashGuard.kind(task: "folder.removeEmpty?confirm=1", steps: nil), .emptyFolders)
        XCTAssertEqual(TrashGuard.kind(task: "folder.removeEmpty", steps: nil), .emptyFolders)
        XCTAssertEqual(TrashGuard.kind(task: "folder.removeDuplicate?mode=all", steps: nil), .files)
        XCTAssertEqual(TrashGuard.kind(task: "folder.removeEmptyX", steps: nil), .files)
        XCTAssertEqual(TrashGuard.kind(task: "folder.removeEmpty", steps: ["folder.removeEmpty"]), .files)
    }

    func testOnlyPathsStrictlyInsideTheSelection() throws {
        let root = scratch()
        let sel = root.appendingPathComponent("sel"), sibling = root.appendingPathComponent("selX")
        let inside = sel.appendingPathComponent("a/dup.txt"), outside = sibling.appendingPathComponent("dup.txt")
        file(inside); file(outside); file(root.appendingPathComponent("top.txt"))
        let inputs = [sel.path]
        XCTAssertNil(TrashGuard.check(inside.path, inputs: inputs, kind: .files))
        XCTAssertEqual(TrashGuard.check(outside.path, inputs: inputs, kind: .files), .outsideSelection, "`selX` is not inside `sel`")
        XCTAssertEqual(TrashGuard.check(root.appendingPathComponent("top.txt").path, inputs: inputs, kind: .files), .outsideSelection)
        XCTAssertEqual(TrashGuard.check(sel.path, inputs: inputs, kind: .emptyFolders), .isSelected)
        XCTAssertEqual(TrashGuard.check(sel.path + "/", inputs: inputs, kind: .emptyFolders), .invalidPath)
        XCTAssertEqual(TrashGuard.check(inside.path, inputs: [], kind: .files), .outsideSelection)
        // `..`, `.`, empty components, relative and NUL paths are refused before anything is looked at
        for p in [sel.path + "/a/..", sel.path + "/a/../../selX/dup.txt", sel.path + "/./a/dup.txt", sel.path + "//a/dup.txt",
                  "a/dup.txt", "", "/", sel.path + "/a\0/dup.txt"] {
            XCTAssertNotNil(TrashGuard.check(p, inputs: inputs, kind: .files), p)
        }
        XCTAssertEqual(TrashGuard.check(sel.path + "/a/..", inputs: inputs, kind: .emptyFolders), .invalidPath)
        // another spelling of the same folder (/var vs /private/var) is the same folder
        let canon = try XCTUnwrap(LaunchRequestPaths.real(sel.path))
        XCTAssertNil(TrashGuard.check(canon + "/a/dup.txt", inputs: inputs, kind: .files))
        XCTAssertNil(TrashGuard.check(inside.path, inputs: [canon], kind: .files))
        XCTAssertEqual(TrashGuard.check(sel.appendingPathComponent("gone.txt").path, inputs: inputs, kind: .files), .missing)
    }

    func testLinksAreNeverTrashedOrFollowed() throws {
        let root = scratch()
        let sel = root.appendingPathComponent("sel"), elsewhere = root.appendingPathComponent("elsewhere")
        file(elsewhere.appendingPathComponent("precious.txt")); dir(sel)
        let linkToDir = sel.appendingPathComponent("link"), linkToFile = sel.appendingPathComponent("file-link")
        XCTAssertEqual(symlink(elsewhere.path, linkToDir.path), 0)
        XCTAssertEqual(symlink(elsewhere.appendingPathComponent("precious.txt").path, linkToFile.path), 0)
        XCTAssertEqual(TrashGuard.check(linkToDir.path, inputs: [sel.path], kind: .emptyFolders), .symlink)
        XCTAssertEqual(TrashGuard.check(linkToFile.path, inputs: [sel.path], kind: .files), .symlink)
        // through a link the path resolves outside the selection
        XCTAssertEqual(TrashGuard.check(linkToDir.path + "/precious.txt", inputs: [sel.path], kind: .files), .outsideSelection)
        // a selected link is not a folder the engine scanned: nothing below it is inside the selection
        XCTAssertEqual(TrashGuard.check(linkToDir.path + "/precious.txt", inputs: [linkToDir.path], kind: .files), .outsideSelection)
    }

    func testRemoveEmptyRechecksTheFolderRightBeforeTrashing() {
        let sel = scratch()
        let empty = sel.appendingPathComponent("empty"), litter = sel.appendingPathComponent("litter")
        let nested = sel.appendingPathComponent("nested"), full = sel.appendingPathComponent("full")
        let zero = sel.appendingPathComponent("zero"), linked = sel.appendingPathComponent("linked")
        dir(empty)
        for n in [".DS_Store", "._x", ".localized", "Icon\r", "desktop.ini", "Thumbs.db", "DESKTOP.INI"] { file(litter.appendingPathComponent(n)) }
        dir(nested.appendingPathComponent("a/b")); file(nested.appendingPathComponent("a/.DS_Store"))
        file(full.appendingPathComponent("deep/deeper/note.txt"))
        file(zero.appendingPathComponent("blank"), "")
        dir(linked); XCTAssertEqual(symlink("/etc", linked.appendingPathComponent("l").path), 0)
        let check = { (u: URL, zeroByte: Bool) in TrashGuard.check(u.path, inputs: [sel.path], kind: .emptyFolders, zeroByteIsLitter: zeroByte) }
        XCTAssertNil(check(empty, false))
        XCTAssertNil(check(litter, false))
        XCTAssertNil(check(nested, false))
        XCTAssertEqual(check(full, false), .notEmpty, "content anywhere below makes it non-empty")
        XCTAssertEqual(check(zero, false), .notEmpty)
        XCTAssertNil(check(zero, true), "0-byte files only when that setting is on")
        XCTAssertEqual(check(linked, false), .notEmpty, "a link is content")
        // a file where a folder was planned, and a folder where files are expected
        XCTAssertEqual(TrashGuard.check(full.appendingPathComponent("deep/deeper/note.txt").path, inputs: [sel.path], kind: .emptyFolders), .unexpectedType)
        XCTAssertEqual(TrashGuard.check(empty.path, inputs: [sel.path], kind: .files), .unexpectedType)
        // content added after the engine's check is seen
        file(empty.appendingPathComponent("new.txt"))
        XCTAssertEqual(check(empty, false), .notEmpty)
        // an unreadable folder below is not known to be empty
        let locked = sel.appendingPathComponent("locked"), inner = locked.appendingPathComponent("inner")
        dir(inner)
        XCTAssertEqual(chmod(inner.path, 0), 0)
        XCTAssertEqual(check(locked, false), .unreadable)
        chmod(inner.path, 0o755)
    }

    func testLitterNamesMatchTheEngine() {
        for n in [".DS_Store", ".ds_store", "._anything", ".localized", "Icon\r", "desktop.ini", "Desktop.ini", "Thumbs.db", "thumbs.DB"] {
            XCTAssertTrue(TrashGuard.isLitter(name: n, size: 10, zeroByteIsLitter: false), n)
        }
        for n in ["Icon", "icon\r", "_x", "DS_Store", "notes.txt", ".hidden"] {
            XCTAssertFalse(TrashGuard.isLitter(name: n, size: 10, zeroByteIsLitter: false), n)
        }
        XCTAssertFalse(TrashGuard.isLitter(name: "a", size: 0, zeroByteIsLitter: false))
        XCTAssertTrue(TrashGuard.isLitter(name: "a", size: 0, zeroByteIsLitter: true))
    }

    func testRefusalMessagesExistInEveryLanguage() throws {
        for code in ["en", "ja", "zh"] {
            let url = try XCTUnwrap(Bundle.module.url(forResource: "strings.ux.\(code)", withExtension: "json", subdirectory: "Resources"))
            let obj = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: String])
            XCTAssertNotNil(obj["app.trashRefused"], code)
            for r in TrashGuard.Refusal.allCases { XCTAssertNotNil(obj[r.key], "\(code): \(r.key)") }
        }
    }
}

/// realpath(3) for the tests.
enum LaunchRequestPaths {
    static func real(_ p: String) -> String? {
        guard let r = realpath(p, nil) else { return nil }
        defer { free(r) }
        return String(cString: r)
    }
}
