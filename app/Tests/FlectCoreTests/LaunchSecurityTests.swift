import XCTest
@testable import FlectCore

extension XCTestCase {
    /// A selection file where and how a Quick Action's `mktemp -t flect-sel` makes one.
    func makeSelectionFile(_ data: Data) throws -> URL {
        let letters = Array("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789")
        let name = LaunchRequest.inputPrefix + String((0..<10).map { _ in letters.randomElement()! })
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(name)
        guard FileManager.default.createFile(atPath: url.path, contents: data, attributes: [.posixPermissions: 0o600]) else { throw CocoaError(.fileWriteUnknown) }
        return url
    }

    func makeSelectionFile(_ text: String) throws -> URL { try makeSelectionFile(Data(text.utf8)) }
}

/// Regression tests for the launch path (`--task <id> --input <file>`): nothing but a Quick Action's own selection file
/// is read or deleted.
final class LaunchSecurityTests: XCTestCase {
    private func scratch() -> URL {
        let d = FileManager.default.temporaryDirectory.appendingPathComponent("flect-launch-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    private func parse(_ input: String) -> Result<LaunchRequest, LaunchRequest.Failure> {
        LaunchRequest.parse(arguments: ["--task", "image.strip", "--input", input])
    }

    func testInputFolderIsNeverDeleted() throws {
        // `open -a Flect-for-Mac --args --task x --input ~/Documents` used to remove the folder recursively.
        let dir = scratch()
        let inside = dir.appendingPathComponent("keep.txt")
        try "/a\n".write(to: inside, atomically: true, encoding: .utf8)
        XCTAssertEqual(parse(dir.path), .failure(.untrustedInput))
        XCTAssertTrue(FileManager.default.fileExists(atPath: inside.path))

        // Even a folder with the selection file's name in the temp folder.
        let named = FileManager.default.temporaryDirectory.appendingPathComponent("flect-sel.Folder0001")
        try FileManager.default.createDirectory(at: named, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: named) }
        XCTAssertEqual(parse(named.path), .failure(.untrustedInput))
        var isDir: ObjCBool = false
        XCTAssertTrue(FileManager.default.fileExists(atPath: named.path, isDirectory: &isDir) && isDir.boolValue)
    }

    func testFilesOutsideTheTempFolderAreNeitherReadNorDeleted() throws {
        let dir = scratch()
        // right name, wrong folder (a subfolder of the temp folder is not the temp folder)
        let elsewhere = dir.appendingPathComponent("flect-sel.Abc123")
        try "/a\n".write(to: elsewhere, atomically: true, encoding: .utf8)
        XCTAssertEqual(parse(elsewhere.path), .failure(.untrustedInput))
        XCTAssertTrue(FileManager.default.fileExists(atPath: elsewhere.path))
        // right folder, wrong name
        let other = FileManager.default.temporaryDirectory.appendingPathComponent("important-\(UUID().uuidString).txt")
        try "/a\n".write(to: other, atomically: true, encoding: .utf8)
        defer { unlink(other.path) }
        XCTAssertEqual(parse(other.path), .failure(.untrustedInput))
        XCTAssertTrue(FileManager.default.fileExists(atPath: other.path))
        // `..` cannot walk out of the temp folder and back in under another name
        XCTAssertFalse(LaunchRequest.isInTempDirectory(dir.path + "/../flect-sel.x/../../etc/passwd", tempDirectories: LaunchRequest.userTempDirectories))
        XCTAssertEqual(parse("relative/flect-sel.Abc"), .failure(.untrustedInput))
    }

    func testSymlinkIsNeitherFollowedNorRemoved() throws {
        let dir = scratch()
        let target = dir.appendingPathComponent("target.txt")
        try "/a\n".write(to: target, atomically: true, encoding: .utf8)
        let link = FileManager.default.temporaryDirectory.appendingPathComponent("flect-sel.Link\(Int.random(in: 1000...9999))")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: target)
        defer { unlink(link.path) }
        XCTAssertEqual(parse(link.path), .failure(.untrustedInput))
        var st = stat()
        XCTAssertEqual(lstat(link.path, &st), 0, "the link itself is left alone")
        XCTAssertTrue(FileManager.default.fileExists(atPath: target.path), "the target is left alone")
        // a link to a folder: the old code removed the folder's contents through it
        let dirLink = FileManager.default.temporaryDirectory.appendingPathComponent("flect-sel.DirLink\(Int.random(in: 1000...9999))")
        try FileManager.default.createSymbolicLink(at: dirLink, withDestinationURL: dir)
        defer { unlink(dirLink.path) }
        XCTAssertEqual(parse(dirLink.path), .failure(.untrustedInput))
        XCTAssertTrue(FileManager.default.fileExists(atPath: target.path))
    }

    func testHardLinkFifoAndOversizedFileAreRefused() throws {
        let dir = scratch()
        let original = dir.appendingPathComponent("original.txt")
        try "/a\n".write(to: original, atomically: true, encoding: .utf8)
        let hard = FileManager.default.temporaryDirectory.appendingPathComponent("flect-sel.Hard\(Int.random(in: 1000...9999))")
        XCTAssertEqual(link(original.path, hard.path), 0)
        defer { unlink(hard.path) }
        XCTAssertEqual(parse(hard.path), .failure(.untrustedInput))
        XCTAssertTrue(FileManager.default.fileExists(atPath: hard.path) && FileManager.default.fileExists(atPath: original.path))

        let fifo = FileManager.default.temporaryDirectory.appendingPathComponent("flect-sel.Fifo\(Int.random(in: 1000...9999))")
        XCTAssertEqual(mkfifo(fifo.path, 0o600), 0)
        defer { unlink(fifo.path) }
        XCTAssertEqual(parse(fifo.path), .failure(.untrustedInput), "refused without opening it (would block)")
        XCTAssertTrue(FileManager.default.fileExists(atPath: fifo.path))

        let big = try makeSelectionFile(Data())
        defer { unlink(big.path) }
        XCTAssertEqual(truncate(big.path, off_t(LaunchRequest.maxInputBytes)), 0)
        XCTAssertEqual(parse(big.path), .failure(.untrustedInput))
        XCTAssertTrue(FileManager.default.fileExists(atPath: big.path), "an oversized file is not read and not deleted")
    }

    func testOwnerAndTypeRules() {
        var st = stat()
        st.st_mode = S_IFREG | 0o600
        st.st_uid = getuid()
        st.st_nlink = 1
        st.st_size = 10
        XCTAssertTrue(LaunchRequest.isTrustedStat(st))
        var other = st; other.st_uid = getuid() &+ 1
        XCTAssertFalse(LaunchRequest.isTrustedStat(other), "a file owned by someone else")
        var dir = st; dir.st_mode = S_IFDIR | 0o700
        XCTAssertFalse(LaunchRequest.isTrustedStat(dir))
        var lnk = st; lnk.st_mode = S_IFLNK | 0o777
        XCTAssertFalse(LaunchRequest.isTrustedStat(lnk))
        var big = st; big.st_size = off_t(LaunchRequest.maxInputBytes)
        XCTAssertFalse(LaunchRequest.isTrustedStat(big))
    }

    func testSelectionFileNamePattern() {
        XCTAssertTrue(LaunchRequest.isSelectionFileName("flect-sel.ETuoaKy26Y"))
        for bad in ["flect-sel.", "flect-sel", "flect-sel.a/b", "flect-sel.a b", "flect-sel.a.b", "xflect-sel.abc", "flect-sel." + String(repeating: "a", count: 33)] {
            XCTAssertFalse(LaunchRequest.isSelectionFileName(bad), bad)
        }
    }

    func testRealSelectionFileIsReadThenUnlinked() throws {
        let x = scratch().appendingPathComponent("x.png").path
        FileManager.default.createFile(atPath: x, contents: Data())
        let file = try makeSelectionFile("\(x)\0")
        XCTAssertTrue(LaunchRequest.isSelectionFile(file.path))
        // the temp folder may be given as /var/... or /private/var/...: both resolve to the same folder
        let resolved = LaunchRequest.realPath(FileManager.default.temporaryDirectory.path)!
        XCTAssertTrue(LaunchRequest.isSelectionFile(file.path, tempDirectories: [resolved]))
        XCTAssertEqual(try LaunchRequest.parse(arguments: ["--task", "image.strip", "--input", file.path], deleteInput: false).get().paths, [x])
        XCTAssertTrue(FileManager.default.fileExists(atPath: file.path), "deleteInput: false keeps it")
        XCTAssertEqual(try parse(file.path).get().paths, [x])
        XCTAssertFalse(FileManager.default.fileExists(atPath: file.path))
        // not in any of the given temp folders
        let again = try makeSelectionFile("/a\n")
        defer { unlink(again.path) }
        XCTAssertEqual(LaunchRequest.parse(arguments: ["--task", "x", "--input", again.path], tempDirectories: ["/nonexistent"]), .failure(.untrustedInput))
        XCTAssertTrue(FileManager.default.fileExists(atPath: again.path))
    }

    // MARK: the selection format (NUL-separated)

    func testLineBreakInAFileNameDoesNotAddAPath() throws {
        // A file named "x\n<folder>" used to add <folder> to the selection.
        let dir = scratch()
        let target = dir.appendingPathComponent("target").path
        FileManager.default.createFile(atPath: target, contents: Data())
        let evil = dir.path + "/x\n" + target   // <dir>/x⏎<dir>/target: folders "x⏎", ..., then a file
        try FileManager.default.createDirectory(atPath: (evil as NSString).deletingLastPathComponent, withIntermediateDirectories: true)
        XCTAssertTrue(FileManager.default.createFile(atPath: evil, contents: Data()))
        let file = try makeSelectionFile("\(evil)\0")
        XCTAssertEqual(try parse(file.path).get().paths, [evil], "one item, with the line break kept in its name")

        // Every separator `.newlines` knew, and spaces, stay part of the name.
        let names = ["cr\rname", "crlf\r\nname", "ls\u{2028}name", "ps\u{2029}name", "nel\u{85}name", "vt\u{0B}ff\u{0C}", " spaced ", "\u{FEFF}bom"]
        let paths = names.map { dir.appendingPathComponent($0).path }
        for p in paths { XCTAssertTrue(FileManager.default.createFile(atPath: p, contents: Data()), p) }
        let many = try makeSelectionFile(paths.map { $0 + "\0" }.joined())
        XCTAssertEqual(try parse(many.path).get().paths, paths)
    }

    func testSelectionPathsSplitOnNulOnly() {
        func paths(_ s: String) -> Result<[String], LaunchRequest.Failure> { LaunchRequest.selectionPaths(Data(s.utf8)) }
        XCTAssertEqual(paths("/a\0/b\0"), .success(["/a", "/b"]))
        XCTAssertEqual(paths("/a\0/b"), .success(["/a", "/b"]), "the last terminator may be missing")
        XCTAssertEqual(paths("/a\n/b\0"), .success(["/a\n/b"]))
        XCTAssertEqual(paths(" /a \0"), .success([" /a "]), "nothing is trimmed")
        XCTAssertEqual(paths("/a\0\0"), .failure(.malformed), "only one trailing empty entry")
        XCTAssertEqual(paths("/a\0\0/b\0"), .failure(.malformed))
        XCTAssertEqual(paths("\0/a\0"), .failure(.malformed))
        XCTAssertEqual(paths("\0"), .failure(.malformed))
        XCTAssertEqual(paths(""), .failure(.empty))
        XCTAssertEqual(LaunchRequest.selectionPaths(Data([0x2F, 0xFF, 0])), .failure(.unreadable))
    }

    func testOldNewlineSelectionFilesAreRefusedUnlessUnambiguous() throws {
        func paths(_ s: String) -> Result<[String], LaunchRequest.Failure> { LaunchRequest.selectionPaths(Data(s.utf8)) }
        // what Quick Actions installed by 0.2.0 and earlier write: printf '%s\n'
        XCTAssertEqual(paths("/a\n/b\n"), .failure(.outdatedQuickAction))
        XCTAssertEqual(paths("/x\n/Users/me\n"), .failure(.outdatedQuickAction))
        XCTAssertEqual(paths("/a\r\n"), .failure(.outdatedQuickAction))
        XCTAssertEqual(paths("/a\u{2028}/b\n"), .failure(.outdatedQuickAction))
        XCTAssertEqual(paths("/a\n\n"), .failure(.outdatedQuickAction))
        XCTAssertEqual(paths("\n"), .failure(.outdatedQuickAction))
        // exactly one path and no line break inside it: unambiguous, accepted as it is
        XCTAssertEqual(paths("/Users/a/one file.png\n"), .success(["/Users/a/one file.png"]))
        XCTAssertEqual(paths("/Users/a/one file.png"), .success(["/Users/a/one file.png"]))

        // end to end: an old multi-line file is refused (and, being our own file, removed); nothing runs
        let dir = scratch()
        let a = dir.appendingPathComponent("a").path, b = dir.appendingPathComponent("b").path
        for p in [a, b] { FileManager.default.createFile(atPath: p, contents: Data()) }
        let old = try makeSelectionFile("\(a)\n\(b)\n")
        XCTAssertEqual(parse(old.path), .failure(.outdatedQuickAction))
        let single = try makeSelectionFile("\(a)\n")
        XCTAssertEqual(try parse(single.path).get().paths, [a])
    }

    func testEveryListedPathMustExist() throws {
        let dir = scratch()
        let real = dir.appendingPathComponent("real").path
        FileManager.default.createFile(atPath: real, contents: Data())
        let gone = dir.appendingPathComponent("gone").path
        let file = try makeSelectionFile("\(real)\0\(gone)\0")
        XCTAssertEqual(parse(file.path), .failure(.missing(String(gone.prefix(80)))))
        // a dangling link still lstats: it is the item the user selected
        let link = dir.appendingPathComponent("dangling").path
        XCTAssertEqual(symlink(gone, link), 0)
        XCTAssertEqual(try parse(try makeSelectionFile("\(link)\0").path).get().paths, [link])
    }

    /// An item macOS privacy protection hides (EACCES / EPERM from lstat) was reported as "no longer exists".
    func testUnreadableIsNotReportedAsMissing() throws {
        let dir = scratch()
        let a = dir.appendingPathComponent("a").path, b = dir.appendingPathComponent("b").path
        for p in [a, b] { FileManager.default.createFile(atPath: p, contents: Data()) }
        func parse(errno code: Int32) throws -> Result<LaunchRequest, LaunchRequest.Failure> {
            let file = try makeSelectionFile("\(a)\0\(b)\0")
            return LaunchRequest.parse(arguments: ["--task", "image.strip", "--input", file.path],
                                       statPath: { $0 == b ? code : LaunchRequest.lstatErrno($0) })
        }
        let shown = String(b.prefix(80))
        XCTAssertEqual(try parse(errno: ENOENT), .failure(.missing(shown)))
        XCTAssertEqual(try parse(errno: ENOTDIR), .failure(.missing(shown)))
        XCTAssertEqual(try parse(errno: EACCES), .failure(.notPermitted(shown)))
        XCTAssertEqual(try parse(errno: EPERM), .failure(.notPermitted(shown)))
        XCTAssertEqual(try parse(errno: EIO), .failure(.unreadablePath(shown)))
        XCTAssertEqual(try parse(errno: ENAMETOOLONG), .failure(.unreadablePath(shown)))
        XCTAssertEqual(try parse(errno: 0).get().paths, [a, b])
        // the real lstat: 0 for an existing item, ENOENT for a gone one
        XCTAssertEqual(LaunchRequest.lstatErrno(a), 0)
        XCTAssertEqual(LaunchRequest.lstatErrno(dir.appendingPathComponent("gone").path), ENOENT)
        XCTAssertEqual(LaunchRequest.lstatErrno(a + "/below-a-file"), ENOTDIR)
    }

    func testOutdatedQuickActionsAreCounted() throws {
        let dir = scratch()
        func workflow(_ name: String, _ script: String?) throws {
            let c = dir.appendingPathComponent(name).appendingPathComponent("Contents")
            try FileManager.default.createDirectory(at: c, withIntermediateDirectories: true)
            if let s = script { try s.write(to: c.appendingPathComponent("document.wflow"), atomically: true, encoding: .utf8) }
        }
        try workflow("Flect Image Resize.workflow", "<string># \(QuickActionInstaller.workflowFormatMarker) ...</string>")
        try workflow("Flect Image OCR.workflow", "<string>for p in \"$@\"; do printf '%s\\n' \"$p\" >> \"$f\"; done</string>")
        try workflow("Flect Folder Tree.workflow", nil)
        try workflow("Other.workflow", "nothing")
        XCTAssertEqual(QuickActionInstaller.installedCount(in: dir), 3)
        XCTAssertEqual(QuickActionInstaller.outdatedCount(in: dir), 2, "the old script and the unreadable one")
        XCTAssertEqual(QuickActionInstaller.outdatedCount(in: dir.appendingPathComponent("missing")), 0)
    }

    func testLaunchMessagesExistInEveryLanguage() throws {
        let keys = ["ux.launch.untrustedInput", "ux.launch.rejectedTask", "ux.launch.reinstall", "ux.launch.missing",
                    "ux.launch.notPermitted", "ux.launch.unreadableItem",
                    "ux.setup.outdated", "ux.setup.outdated.one", "ux.setup.outdatedBanner"]
        for code in ["en", "ja", "zh"] {
            let url = try XCTUnwrap(Bundle.module.url(forResource: "strings.ux.\(code)", withExtension: "json", subdirectory: "Resources"))
            let obj = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: String])
            for k in keys { XCTAssertNotNil(obj[k], "\(code): \(k)") }
        }
    }

    // MARK: --task

    /// The task ids the generated Quick Actions pass (core/flect-tools checks its own list against the same file).
    private func quickActionTasks() throws -> [String] {
        let file = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("quick-action-tasks.txt")
        return try String(contentsOf: file, encoding: .utf8).split(separator: "\n").map(String.init)
    }

    private func catalog(_ ids: [String]) throws -> MenuCatalog {
        let entries: [[String: Any]] = [["id": "all", "children": ids.map { ["id": $0] }]]
        return try XCTUnwrap(MenuCatalog(menu: JSONValue(any: ["entries": entries])))
    }

    func testEveryQuickActionTaskIsAdmittedAndResolves() throws {
        let ids = try quickActionTasks()
        XCTAssertEqual(ids.count, 38)
        let c = try catalog(ids)
        let s = SettingsStore(directory: scratch())
        for id in ids {
            XCTAssertEqual(LaunchRequest.launchTask(id, catalog: c), .catalog(id), id)
            XCTAssertNotEqual(Planner.resolveCatalogID(id, settings: s), .unknown, id)
        }
        let uuid = UUID()
        XCTAssertEqual(LaunchRequest.launchTask("preset:\(uuid.uuidString)", catalog: nil), .preset(uuid))
    }

    func testLaunchArgumentsCannotCarryParametersOrUnknownTasks() throws {
        let c = try catalog(try quickActionTasks())
        let s = SettingsStore(directory: scratch())
        let hostile = [
            "folder.removeDuplicate?mode=all", "folder.removeDuplicate?mode=newest", "folder.removeDuplicate?mode=oldest&dry=0",
            "folder.removeEmpty?confirm=1", "folder.rename?prefix=a&name=b&suffix=&numbered=1&dry=0", "image.resize?scale=10",
            "folder.removeEmpty ", " folder.removeEmpty", "folder.removeEmpty\n", "nope.task", "folder", "all", "", "?", ".folder.tree",
            "preset:not-a-uuid", "preset:\(UUID().uuidString)?x", "preset:", "task:image.strip", "step:image.strip",
        ]
        for t in hostile {
            XCTAssertNil(LaunchRequest.launchTask(t, catalog: c), t)
            if t.contains("?") { XCTAssertEqual(Planner.resolveCatalogID(t, settings: s), .unknown, t) }
        }
        XCTAssertNil(LaunchRequest.launchTask("image.strip", catalog: nil), "no catalog, no catalog id")
        // `resolve` itself still passes a parameterised task through (the Shortcuts plan validates its own).
        XCTAssertEqual(Planner.resolve("image.resize?scale=10", settings: s), .engine("image.resize?scale=10"))
    }

    func testDeletingAndRenamingTasksAlwaysStartWithTheirPreview() throws {
        let s = SettingsStore(directory: scratch())
        XCTAssertEqual(Planner.resolveCatalogID("folder.removeDuplicate", settings: s), .engine("folder.removeDuplicate?dry=1"))
        XCTAssertEqual(Planner.resolveCatalogID("folder.removeEmpty", settings: s), .engine("folder.removeEmpty"), "lists the folders, confirm=1 only after the user confirms")
        XCTAssertEqual(Planner.resolveCatalogID("folder.rename", settings: s), .dialog(.rename))
        // No catalog id of any entry point resolves to a confirmed or non-dry deleting task.
        for id in try quickActionTasks() {
            guard case .engine(let t) = Planner.resolveCatalogID(id, settings: s) else { continue }
            XCTAssertFalse(t.contains("confirm=") || t.contains("dry=0"), t)
            if t.hasPrefix("folder.removeDuplicate") { XCTAssertTrue(t.contains("dry=1"), t) }
            XCTAssertFalse(t.hasPrefix("folder.rename"), t)
        }
    }
}
