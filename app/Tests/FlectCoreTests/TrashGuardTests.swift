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

    /// Another spelling of a selected folder is that folder; a case variant of a selected folder still allows the
    /// paths inside it.
    func testSpellingsOfSelectedItems() throws {
        let root = scratch()
        let sel = root.appendingPathComponent("MixedCase"), inside = sel.appendingPathComponent("Sub/dup.txt")
        file(inside)
        let variants = [root.appendingPathComponent("mixedcase").path, root.appendingPathComponent("MIXEDCASE").path, sel.path + "/", sel.path + "//"]
        // the selected folder in any spelling is refused as a trash path...
        for candidate in [root.appendingPathComponent("mixedcase").path, root.appendingPathComponent("MIXEDCASE").path] {
            XCTAssertEqual(TrashGuard.check(candidate, inputs: [sel.path], kind: .emptyFolders), .isSelected, candidate)
            XCTAssertEqual(TrashGuard.check(sel.path, inputs: [candidate], kind: .emptyFolders), .isSelected, candidate)
        }
        // ...and selecting it in any spelling still allows what is inside it
        for input in variants {
            XCTAssertNil(TrashGuard.check(inside.path, inputs: [input], kind: .files), input)
            XCTAssertNil(TrashGuard.check(root.appendingPathComponent("mixedcase/sub/dup.txt").path, inputs: [input], kind: .files), input)
            XCTAssertEqual(TrashGuard.check(sel.path, inputs: [input], kind: .emptyFolders), .isSelected, input)
        }
        // decomposed vs precomposed name of a selected folder
        let nfc = root.appendingPathComponent("caf\u{E9}")
        dir(nfc)
        let nfd = root.appendingPathComponent("cafe\u{301}").path
        XCTAssertEqual(TrashGuard.check(nfd, inputs: [nfc.path], kind: .emptyFolders), .isSelected)
        // a selected file or link is not a folder the engine scanned: nothing is inside it
        let link = root.appendingPathComponent("link")
        XCTAssertEqual(symlink(sel.path, link.path), 0)
        XCTAssertEqual(TrashGuard.check(inside.path, inputs: [link.path], kind: .files), .outsideSelection)
        XCTAssertEqual(TrashGuard.check(inside.path, inputs: [link.path + "/"], kind: .files), .outsideSelection)
        // a selected file is refused itself
        XCTAssertEqual(TrashGuard.check(inside.path, inputs: [inside.path], kind: .files), .isSelected)
    }

    /// `--apply D D/chosen.txt` had the selected file handed to the Trash (only folders were compared).
    func testSelectedFilesAreNeverTrashed() throws {
        let root = scratch()
        let d = root.appendingPathComponent("D"), chosen = d.appendingPathComponent("Chosen.txt"), copy = d.appendingPathComponent("copy.txt")
        file(chosen); file(copy)
        XCTAssertNil(TrashGuard.check(copy.path, inputs: [d.path, chosen.path], kind: .files))
        XCTAssertEqual(TrashGuard.check(chosen.path, inputs: [d.path, chosen.path], kind: .files), .isSelected)
        for spelling in [d.appendingPathComponent("chosen.txt").path, d.appendingPathComponent("CHOSEN.TXT").path,
                         root.appendingPathComponent("d/Chosen.txt").path] {
            XCTAssertEqual(TrashGuard.check(spelling, inputs: [d.path, chosen.path], kind: .files), .isSelected, spelling)
            XCTAssertEqual(TrashGuard.check(chosen.path, inputs: [d.path, spelling], kind: .files), .isSelected, spelling)
        }
        // a hard link to a selected file is that file
        let alias = d.appendingPathComponent("alias.txt")
        XCTAssertEqual(link(chosen.path, alias.path), 0)
        XCTAssertEqual(TrashGuard.check(alias.path, inputs: [d.path, chosen.path], kind: .files), .isSelected)
        // a folder that holds a selected item is never trashed as "empty"
        let holder = d.appendingPathComponent("H"), sel = holder.appendingPathComponent("inner")
        dir(sel)
        XCTAssertEqual(TrashGuard.check(holder.path, inputs: [d.path, sel.path], kind: .emptyFolders), .holdsSelected)
        XCTAssertEqual(TrashGuard.check(sel.path, inputs: [d.path, sel.path], kind: .emptyFolders), .isSelected)
        XCTAssertNil(TrashGuard.check(holder.path, inputs: [d.path], kind: .emptyFolders))
        // ...in any spelling of the holder or of the selected item (case variants)
        let holderVariant = d.appendingPathComponent("h").path
        XCTAssertEqual(TrashGuard.check(holderVariant, inputs: [d.path, sel.path], kind: .emptyFolders), .holdsSelected)
        XCTAssertEqual(TrashGuard.check(holder.path, inputs: [d.path, d.appendingPathComponent("h/INNER").path], kind: .emptyFolders), .holdsSelected)
        // a selected link stands for its target: the target and the folders above it are protected
        let lnk = root.appendingPathComponent("lnk"), lnkDir = root.appendingPathComponent("lnkdir")
        XCTAssertEqual(symlink(copy.path, lnk.path), 0)
        XCTAssertEqual(symlink(sel.path, lnkDir.path), 0)
        XCTAssertEqual(TrashGuard.check(copy.path, inputs: [d.path, lnk.path], kind: .files), .isSelected)
        XCTAssertEqual(TrashGuard.check(sel.path, inputs: [d.path, lnkDir.path], kind: .emptyFolders), .isSelected)
        XCTAssertEqual(TrashGuard.check(holder.path, inputs: [d.path, lnkDir.path], kind: .emptyFolders), .holdsSelected)
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
        let check = { (u: URL, zeroByte: Bool) in TrashGuard.check(u.path, inputs: [sel.path], kind: .emptyFolders, litter: .init(zeroByte: zeroByte)) }
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
            XCTAssertTrue(TrashGuard.isLitter(name: n, size: 10, rules: .init()), n)
        }
        for n in ["Icon", "icon\r", "_x", "DS_Store", "notes.txt", ".hidden"] {
            XCTAssertFalse(TrashGuard.isLitter(name: n, size: 10, rules: .init()), n)
        }
        XCTAssertFalse(TrashGuard.isLitter(name: "a", size: 0, rules: .init()))
        XCTAssertTrue(TrashGuard.isLitter(name: "a", size: 0, rules: .init(zeroByte: true)))
    }

    /// The litter list honours the engine's settings: with a toggle off, those files are content again.
    func testLitterFollowsTheEmptyFolderSettings() throws {
        let noDesktop = TrashGuard.LitterRules(desktopIni: false), noThumbs = TrashGuard.LitterRules(thumbsDb: false)
        for n in [".DS_Store", "._x", ".localized", "Icon\r", "desktop.ini"] {
            XCTAssertFalse(TrashGuard.isLitter(name: n, size: 10, rules: noDesktop), n)
            XCTAssertTrue(TrashGuard.isLitter(name: n, size: 10, rules: noThumbs), n)
        }
        XCTAssertFalse(TrashGuard.isLitter(name: "Thumbs.db", size: 10, rules: noThumbs))
        XCTAssertTrue(TrashGuard.isLitter(name: "Thumbs.db", size: 10, rules: noDesktop))

        let sel = scratch(), store = scratch()
        let ds = sel.appendingPathComponent("ds"), thumbs = sel.appendingPathComponent("thumbs")
        file(ds.appendingPathComponent(".DS_Store")); file(thumbs.appendingPathComponent("Thumbs.db"))
        func rules(_ json: String) throws -> TrashGuard.LitterRules {
            try json.write(to: store.appendingPathComponent("settings.json"), atomically: true, encoding: .utf8)
            return TrashGuard.LitterRules(settings: SettingsStore(directory: store))
        }
        XCTAssertEqual(try rules("{}"), TrashGuard.LitterRules())
        let off = try rules(#"{"Folder":{"Empty":{"IgnoreDesktopIni":false,"IgnoreThumbsDb":false,"IgnoreZeroByteFiles":true}}}"#)
        XCTAssertEqual(off, TrashGuard.LitterRules(zeroByte: true, desktopIni: false, thumbsDb: false))
        XCTAssertEqual(TrashGuard.check(ds.path, inputs: [sel.path], kind: .emptyFolders, litter: off), .notEmpty)
        XCTAssertEqual(TrashGuard.check(thumbs.path, inputs: [sel.path], kind: .emptyFolders, litter: off), .notEmpty)
        XCTAssertNil(TrashGuard.check(ds.path, inputs: [sel.path], kind: .emptyFolders, litter: .init()))
        XCTAssertNil(TrashGuard.check(thumbs.path, inputs: [sel.path], kind: .emptyFolders, litter: .init()))
    }

    /// Refused or failed items used to leave the run looking clean, with the engine's "Moved N to the Trash".
    func testSummaryDoesNotClaimItemsThatWereNotMoved() {
        let loc = Loc(language: .english)
        var t = TrashGuard.Tally()
        XCTAssertEqual(TrashGuard.runSummary("engine says", tally: t, loc: loc), "engine says")
        t.add(.init(requested: 3, moved: 3))
        XCTAssertEqual(t.notMoved, 0)
        XCTAssertEqual(TrashGuard.runSummary("Moved 3 to the Trash.", tally: t, loc: loc), "Moved 3 to the Trash.")
        t.add(.init(requested: 2, moved: 0))
        XCTAssertEqual(t, .init(requested: 5, moved: 3))
        let s = TrashGuard.runSummary("Moved 5 to the Trash.", tally: t, loc: loc)
        XCTAssertEqual(s, "Moved 3 of 5 items to the Trash; 2 were left in place (see the list below).")
        // singular forms
        XCTAssertEqual(TrashGuard.runSummary(nil, tally: .init(requested: 1, moved: 0), loc: loc),
                       "The item was not moved to the Trash; it was left in place (see the list below).")
        XCTAssertEqual(TrashGuard.runSummary(nil, tally: .init(requested: 4, moved: 3), loc: loc),
                       "Moved 3 of 4 items to the Trash; 1 was left in place (see the list below).")
        XCTAssertEqual(TrashGuard.runSummary(nil, tally: .init(requested: 2, moved: 0), loc: loc),
                       "Moved 0 of 2 items to the Trash; 2 were left in place (see the list below).")
        // Japanese / Chinese have one form: their plain text is used, never the English singular
        for lang in [AppLanguage.japanese, .chinese] {
            let l = Loc(language: lang)
            for t in [TrashGuard.Tally(requested: 1, moved: 0), .init(requested: 4, moved: 3)] {
                let text = TrashGuard.runSummary(nil, tally: t, loc: l) ?? ""
                XCTAssertFalse(text.contains("Trash") || text.contains("{"), text)
                XCTAssertTrue(text.contains(String(t.requested)), text)
            }
        }
        for lang in [AppLanguage.japanese, .chinese] {
            let text = TrashGuard.runSummary(nil, tally: .init(requested: 5, moved: 3), loc: Loc(language: lang)) ?? ""
            XCTAssertTrue(text.contains("3") && text.contains("5") && text.contains("2") && !text.contains("{"), text)
        }
    }

    /// Trash requests are tied to the run that produced them: a request from an earlier run (its events still arriving
    /// after a new run started) or after the run ended is refused, whatever the path.
    func testRequestsOnlyFromTheCurrentRun() {
        let sel = scratch(), other = scratch()
        let target = sel.appendingPathComponent("dup.txt"), otherTarget = other.appendingPathComponent("dup.txt")
        file(target); file(otherTarget)
        let first = TrashGuard.Scope(inputs: [sel.path], kind: .files)
        let second = TrashGuard.Scope(inputs: [other.path], kind: .files)
        XCTAssertNotEqual(first.run, second.run)
        XCTAssertNil(TrashGuard.check(target.path, scope: first, currentRun: first.run, fingerprint: TrashGuard.fingerprint(of: target.path)))
        // a new run started: the first run's requests are refused, the second's are checked against its own inputs
        XCTAssertEqual(TrashGuard.check(target.path, scope: first, currentRun: second.run, fingerprint: TrashGuard.fingerprint(of: target.path)), .staleRun)
        XCTAssertNil(TrashGuard.check(otherTarget.path, scope: second, currentRun: second.run, fingerprint: TrashGuard.fingerprint(of: otherTarget.path)))
        XCTAssertEqual(TrashGuard.check(target.path, scope: second, currentRun: second.run, fingerprint: TrashGuard.fingerprint(of: target.path)), .outsideSelection)
        // no run current
        XCTAssertEqual(TrashGuard.check(otherTarget.path, scope: second, currentRun: nil, fingerprint: TrashGuard.fingerprint(of: otherTarget.path)), .staleRun)
        XCTAssertEqual(TrashGuard.kind(task: "folder.removeEmpty?confirm=1", steps: nil), TrashGuard.Scope(inputs: [], kind: .emptyFolders).kind)
    }

    /// The engine hands each item on with the fingerprint it verified; the app moves it only while it still matches.
    func testItemsAreMovedOnlyWhileTheirFingerprintMatches() throws {
        let sel = scratch()
        let f = sel.appendingPathComponent("dup.txt"), d = sel.appendingPathComponent("empty")
        file(f); dir(d)
        let files = TrashGuard.Scope(inputs: [sel.path], kind: .files), folders = TrashGuard.Scope(inputs: [sel.path], kind: .emptyFolders)
        let fp = try XCTUnwrap(TrashGuard.fingerprint(of: f.path)), dp = try XCTUnwrap(TrashGuard.fingerprint(of: d.path))
        XCTAssertTrue(fp.hasPrefix("f:") && fp.split(separator: ":").count == 6, fp)
        XCTAssertTrue(dp.hasPrefix("d:") && dp.split(separator: ":").count == 3, dp)
        XCTAssertNil(TrashGuard.check(f.path, scope: files, currentRun: files.run, fingerprint: fp))
        XCTAssertNil(TrashGuard.check(d.path, scope: folders, currentRun: folders.run, fingerprint: dp))
        // no fingerprint (an older engine, or a forged request): refused
        XCTAssertEqual(TrashGuard.check(f.path, scope: files, currentRun: files.run, fingerprint: nil), .changed)
        // rewritten in place (same size), replaced, or a folder swapped for another: refused
        try "y".write(to: f, atomically: false, encoding: .utf8)
        XCTAssertEqual(TrashGuard.check(f.path, scope: files, currentRun: files.run, fingerprint: fp), .changed)
        try FileManager.default.removeItem(at: d); dir(d)
        XCTAssertEqual(TrashGuard.check(d.path, scope: folders, currentRun: folders.run, fingerprint: dp), .changed)
        XCTAssertNil(TrashGuard.fingerprint(of: sel.appendingPathComponent("gone").path))
        // the format the engine writes: device, inode, size, then times as seconds.nanoseconds
        let now = try XCTUnwrap(TrashGuard.fingerprint(of: f.path)).split(separator: ":")
        let attrs = try FileManager.default.attributesOfItem(atPath: f.path)
        XCTAssertEqual(UInt64(now[2]), (attrs[.systemFileNumber] as? NSNumber)?.uint64Value)
        XCTAssertEqual(UInt64(now[3]), (attrs[.size] as? NSNumber)?.uint64Value)
        XCTAssertTrue(now[4].contains(".") && now[5].contains("."))
    }

    func testRefusalMessagesExistInEveryLanguage() throws {
        for code in ["en", "ja", "zh"] {
            let url = try XCTUnwrap(Bundle.module.url(forResource: "strings.ux.\(code)", withExtension: "json", subdirectory: "Resources"))
            let obj = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: String])
            XCTAssertNotNil(obj["app.trashRefused"], code)
            XCTAssertNotNil(obj["app.trashPartial"], code)
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
