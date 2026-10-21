import XCTest
@testable import FlectCore

/// Entry points (dropped / opened files -> action picker), run history and the safety rules of Undo.
final class EntryPointTests: XCTestCase {
    // MARK: fixtures

    func tmp() -> URL {
        let d = FileManager.default.temporaryDirectory.appendingPathComponent("flect-entry-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    func std(_ u: URL) -> String { u.standardizedFileURL.path }

    /// A small stand-in for the engine's menu.json (same shape and ids as core/flect-core/src/menu.rs).
    func catalog() throws -> MenuCatalog {
        func leaf(_ id: String) -> [String: Any] { ["id": id] }
        func group(_ id: String, _ kids: [[String: Any]]) -> [String: Any] { ["id": id, "children": kids] }
        let entries: [[String: Any]] = [
            group("folder", [leaf("folder.removeDuplicate"), leaf("folder.tree"), leaf("folder.rename")]),
            group("archive", [leaf("archive.extract")]),
            group("pdf", [leaf("pdf.preview"), leaf("pdf.merge"), leaf("pdf.convert.PNG")]),
            group("image", [group("image.resize", [leaf("image.resize.p1")]), group("image.convert", [leaf("image.convert.PNG")]), leaf("image.ocr")]),
            group("video", [leaf("video.trim"), leaf("video.thumbnail"), group("video.compress", [leaf("video.compress.p1")])]),
            group("audio", [group("audio.convert", [leaf("audio.convert.MP3")])]),
            group("text", [group("text.convert", [leaf("text.convert.TXT")]), group("text.encode", [leaf("text.encode.UTF8")]), leaf("json.pretty"), leaf("json.sort")]),
            group("jar", [leaf("jar.decompile")]),
        ]
        return try XCTUnwrap(MenuCatalog(menu: JSONValue(any: ["entries": entries])))
    }

    func items(_ names: [String]) -> [SelectionItem] { names.map { SelectionItem(path: "/Users/a/\($0)", isDirectory: false) } }

    func ids(_ s: PickerSelection, _ c: SelectionCategory) -> [String] { s.sections.first { $0.category == c }?.entries.map { $0.id } ?? [] }

    // MARK: classification and the picker's task selection

    func testClassificationMatchesTheEngineRules() {
        func c(_ p: String, dir: Bool = false) -> Classification? { SelectionClassifier.classify(path: p, isDirectory: dir) }
        XCTAssertEqual(c("/a/B.JPEG"), Classification(category: .image, format: "JPG"))
        XCTAssertEqual(c("/a/b.tar.gz")?.format, "TAR.GZ")
        XCTAssertEqual(c("/a/b.rs"), Classification(category: .text, format: "TEXT"))
        XCTAssertEqual(c("/a/b.mov")?.category, .video)
        XCTAssertEqual(c("/a/b.m4a")?.category, .video, "m4a is a video-family container for Flect, as in the engine")
        XCTAssertEqual(c("/a/b.flac")?.category, nil)
        XCTAssertNil(c("/a/noext"))
        XCTAssertNil(c("/a/.json"), "a dotfile has no extension")
        XCTAssertNil(c("/a/b.exe"))
        XCTAssertEqual(c("/a", dir: true)?.category, .folder)
        XCTAssertEqual(SelectionClassifier.textTargets(from: "CSV"), ["TXT", "JSON", "XML", "HTML", "MD"])
        XCTAssertEqual(SelectionClassifier.textTargets(from: "TXT"), [])
    }

    func testPickerOffersImageTasksForAnImageAndNothingForPdfOnlyRules() throws {
        let s = TaskPicker.selection(for: items(["a.png"]), catalog: try catalog())
        XCTAssertEqual(s.sections.map { $0.category }, [.image])
        XCTAssertEqual(ids(s, .image), ["image.resize", "image.convert", "image.ocr"])
        XCTAssertEqual(s.sections[0].paths, ["/Users/a/a.png"])
        XCTAssertEqual(s.unsupported, 0)
        XCTAssertEqual(s.firstName, "a.png")
    }

    func testPdfPreviewIsForOneAndMergeForSeveral() throws {
        let one = TaskPicker.selection(for: items(["a.pdf"]), catalog: try catalog())
        XCTAssertEqual(ids(one, .pdf), ["pdf.preview", "pdf.convert.PNG"])
        let two = TaskPicker.selection(for: items(["a.pdf", "b.pdf"]), catalog: try catalog())
        XCTAssertEqual(ids(two, .pdf), ["pdf.merge", "pdf.convert.PNG"])
    }

    func testTrimAndThumbnailNeedExactlyOneVideo() throws {
        let one = TaskPicker.selection(for: items(["a.mp4"]), catalog: try catalog())
        XCTAssertEqual(ids(one, .video), ["video.trim", "video.thumbnail", "video.compress"])
        let two = TaskPicker.selection(for: items(["a.mp4", "b.mov"]), catalog: try catalog())
        XCTAssertEqual(ids(two, .video), ["video.compress"])
    }

    func testMixedSelectionGivesOneSectionPerKindAndCountsUnsupportedItems() throws {
        let sel = items(["a.png", "b.jpg", "c.mp4", "d.exe", "e"]) + [SelectionItem(path: "/Users/a/dir", isDirectory: true)]
        let s = TaskPicker.selection(for: sel, catalog: try catalog())
        XCTAssertEqual(s.sections.map { $0.category }, [.folder, .image, .video], "stable order, only kinds that have tasks")
        XCTAssertEqual(s.sections.first { $0.category == .image }?.paths, ["/Users/a/a.png", "/Users/a/b.jpg"], "each section acts on its own files only")
        XCTAssertEqual(s.unsupported, 2)
        XCTAssertEqual(s.total, 6)
        let none = TaskPicker.selection(for: items(["x.exe"]), catalog: try catalog())
        XCTAssertTrue(none.sections.isEmpty)
    }

    func testJsonAndTextConversionRules() throws {
        let cat = try catalog()
        XCTAssertEqual(ids(TaskPicker.selection(for: items(["a.json", "b.json"]), catalog: cat), .text), ["text.convert", "text.encode", "json.pretty", "json.sort"])
        XCTAssertEqual(ids(TaskPicker.selection(for: items(["a.json", "b.csv"]), catalog: cat), .text), ["text.encode"], "mixed formats: no JSON-only or single-format tasks")
        XCTAssertEqual(ids(TaskPicker.selection(for: items(["a.txt"]), catalog: cat), .text), ["text.encode"], "TXT has no conversion targets")
        XCTAssertEqual(ids(TaskPicker.selection(for: items(["a.csv"]), catalog: cat), .text), ["text.convert", "text.encode"])
    }

    func testAnimatedGifGetsNoConvert() throws {
        let cat = try catalog()
        let s = TaskPicker.selection(for: items(["a.gif", "b.png"]), catalog: cat, isAnimatedGIF: { $0.hasSuffix("a.gif") })
        XCTAssertEqual(ids(s, .image), ["image.resize", "image.ocr"])
        let still = TaskPicker.selection(for: items(["a.gif"]), catalog: cat, isAnimatedGIF: { _ in false })
        XCTAssertTrue(ids(still, .image).contains("image.convert"))
    }

    func testPickerOnlyOffersIdsThePlannerKnows() throws {
        // What is offered must resolve through Planner (the same path as a Quick Action), never to .unknown.
        let s = SettingsStore(directory: tmp())
        let all = items(["a.png", "a.pdf", "b.pdf", "a.mp4", "a.mp3", "a.json", "a.zip", "a.jar"]) + [SelectionItem(path: "/Users/a/d", isDirectory: true)]
        let sel = TaskPicker.selection(for: all, catalog: try catalog())
        XCTAssertGreaterThan(sel.sections.count, 5)
        for section in sel.sections {
            for entry in section.entries {
                let r = Planner.resolve(entry.id, settings: s)
                XCTAssertNotEqual(r, .unknown, "\(entry.id) must be a task the planner understands")
            }
        }
    }

    // MARK: validation of paths from the new entry points

    func testEntryRequestAppliesTheQuickActionPathRules() throws {
        let d = tmp()
        let file = d.appendingPathComponent("a.png"); try Data("x".utf8).write(to: file)
        let folder = d.appendingPathComponent("sub"); try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let link = d.appendingPathComponent("link"); try FileManager.default.createSymbolicLink(at: link, withDestinationURL: folder)
        let missing = d.appendingPathComponent("missing.png")
        let web = try XCTUnwrap(URL(string: "https://example.com/a.png"))
        let long = URL(fileURLWithPath: "/" + String(repeating: "a", count: LaunchRequest.maxPathBytes + 1))

        let r = try EntryRequest.items(from: [file, file, folder, link, missing, web, long]).get()
        XCTAssertEqual(r.items.map { $0.path }, [std(file), std(folder), std(link)], "duplicates dropped, order kept")
        XCTAssertEqual(r.items.map { $0.isDirectory }, [false, true, false], "a symlink is not treated as the folder it points to")
        XCTAssertEqual(r.rejected, 3, "missing, non-file URL and over-long path")
        XCTAssertEqual(EntryRequest.items(from: [missing, web]), .failure(.empty))
    }

    func testLaunchRequestValidationIsSharedAndUnchanged() {
        XCTAssertNil(LaunchRequest.validate(paths: ["/a"]))
        XCTAssertEqual(LaunchRequest.validate(paths: []), .empty)
        XCTAssertEqual(LaunchRequest.validate(paths: ["rel"]), .badPath("rel"))
        XCTAssertEqual(LaunchRequest.validate(paths: ["/a\0b"]), .badPath("/a\0b"))
        XCTAssertEqual(LaunchRequest.validate(paths: Array(repeating: "/a", count: LaunchRequest.maxPaths + 1)), .tooMany)
        XCTAssertFalse(LaunchRequest.isValidPath("/" + String(repeating: "é", count: LaunchRequest.maxPathBytes)), "the limit is in bytes")
    }

    // MARK: recording outputs

    /// A run that started now in `dir` with `input`; `make` creates the outputs while "running".
    func record(dir: URL, inputs: [URL], outputs: [URL], startedAt: Date, fm: FileManager = .default) -> RunRecord {
        RunRecorder.record(task: "image.resize?scale=50", outcome: "succeeded", startedAt: startedAt, finishedAt: Date(), inputs: inputs.map { std($0) },
                           reportedOutputs: outputs.map { $0.path }, summary: nil, home: std(dir).appending("-home"), fm: fm)
    }

    func testRecordKeepsOnlyFilesTheRunCreated() throws {
        let d = tmp()
        let input = d.appendingPathComponent("in.png"); try Data("old".utf8).write(to: input)
        let older = d.appendingPathComponent("older.png"); try Data("old".utf8).write(to: older)
        let start = Date().addingTimeInterval(5) // everything above predates the run
        let out = d.appendingPathComponent("in_resized.png"); try Data("new".utf8).write(to: out)
        try FileManager.default.setAttributes([.creationDate: start.addingTimeInterval(1)], ofItemAtPath: out.path)
        let r = record(dir: d, inputs: [input], outputs: [out, out, input, older, d.appendingPathComponent("gone.png")], startedAt: start)
        XCTAssertEqual(r.outputs.map { $0.path }, [std(out)], "duplicates, the input itself, pre-existing files and missing files are not tracked")
        XCTAssertEqual(r.untracked, 3)
        XCTAssertEqual(r.inputCount, 1)
    }

    func testRecordRefusesDangerousPaths() throws {
        let d = tmp()
        let home = d.appendingPathComponent("home"); try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        let start = Date().addingTimeInterval(-60)
        let inputInFolder = home.appendingPathComponent("folder/in.txt")
        try FileManager.default.createDirectory(at: inputInFolder.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("x".utf8).write(to: inputInFolder)
        func why(_ p: String, inputs: Set<String> = []) -> OutputRejection? {
            RunRecorder.rejection(of: p, inputs: inputs, startedAt: start, home: std(home))
        }
        XCTAssertEqual(why("relative/x"), .notAbsolute)
        XCTAssertEqual(why("/"), .tooShallow)
        XCTAssertEqual(why("/Users"), .tooShallow)
        XCTAssertEqual(why(std(home)), .homeOrAbove)
        XCTAssertEqual(why(std(home.deletingLastPathComponent())), .homeOrAbove)
        XCTAssertEqual(why(std(inputInFolder), inputs: [std(inputInFolder)]), .isInput)
        XCTAssertEqual(why(std(inputInFolder.deletingLastPathComponent()), inputs: [std(inputInFolder)]), .containsInput,
                       "an output folder that holds an input must never be trashed")
        let link = home.appendingPathComponent("link")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: inputInFolder)
        XCTAssertEqual(why(std(link)), .unreadable, "a symlink is never tracked")
    }

    // MARK: undo

    /// Trash stand-in: really moves the item out of the way, and remembers what it was asked to move.
    final class FakeTrash {
        var moved: [String] = []
        let bin: URL
        var failFor: Set<String> = []
        init(bin: URL) { self.bin = bin; try? FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true) }
        func callAsFunction(_ url: URL) throws {
            if failFor.contains(url.path) { throw CocoaError(.fileWriteNoPermission) }
            moved.append(url.path)
            try FileManager.default.moveItem(at: url, to: bin.appendingPathComponent(UUID().uuidString))
        }
    }

    func makeRun() throws -> (dir: URL, input: URL, outs: [URL], bystander: URL, record: RunRecord) {
        let d = tmp()
        let input = d.appendingPathComponent("in.png"); try Data("input".utf8).write(to: input)
        let bystander = d.appendingPathComponent("in_resized (copy).png"); try Data("mine".utf8).write(to: bystander)
        let start = Date().addingTimeInterval(-1)
        let a = d.appendingPathComponent("in_resized.png"); try Data("a".utf8).write(to: a)
        let b = d.appendingPathComponent("in_resized (2).png"); try Data("bb".utf8).write(to: b)
        let rec = record(dir: d, inputs: [input], outputs: [a, b], startedAt: start)
        XCTAssertEqual(rec.outputs.count, 2)
        return (d, input, [a, b], bystander, rec)
    }

    func testUndoMovesOnlyTheRecordedOutputsToTheTrash() throws {
        let run = try makeRun()
        let trash = FakeTrash(bin: tmp())
        let (updated, results) = OutputUndo.undo(run.record, trash: { try trash($0) })
        XCTAssertEqual(results.map { $0.status }, [.trashed, .trashed])
        XCTAssertEqual(Set(trash.moved), Set(run.outs.map { std($0) }))
        XCTAssertTrue(FileManager.default.fileExists(atPath: run.input.path), "the input is never touched")
        XCTAssertTrue(FileManager.default.fileExists(atPath: run.bystander.path), "neither is a similarly named file the run did not create")
        XCTAssertNotNil(updated.undoneAt)
        XCTAssertEqual(updated.undoableCount, 0)
        // a second undo has nothing left to do
        let again = OutputUndo.undo(updated, trash: { try trash($0) })
        XCTAssertTrue(again.results.isEmpty)
        XCTAssertEqual(trash.moved.count, 2)
    }

    func testUndoLeavesAlonePerFileWhatChangedAfterTheRun() throws {
        let run = try makeRun()
        // a: edited after the run (the same size, a later mtime); b: deleted by the user
        try Data("A".utf8).write(to: run.outs[0])
        try FileManager.default.setAttributes([.modificationDate: Date().addingTimeInterval(30)], ofItemAtPath: run.outs[0].path)
        try FileManager.default.removeItem(at: run.outs[1])
        let trash = FakeTrash(bin: tmp())
        let (updated, results) = OutputUndo.undo(run.record, trash: { try trash($0) })
        XCTAssertEqual(results.map { $0.status }, [.modified, .missing])
        XCTAssertTrue(trash.moved.isEmpty)
        XCTAssertTrue(FileManager.default.fileExists(atPath: run.outs[0].path))
        XCTAssertNil(updated.undoneAt)
    }

    func testUndoRefusesAnOutputThatWasReplacedByASymlink() throws {
        let run = try makeRun()
        try FileManager.default.removeItem(at: run.outs[0])
        try FileManager.default.createSymbolicLink(at: run.outs[0], withDestinationURL: run.input)
        let trash = FakeTrash(bin: tmp())
        let (_, results) = OutputUndo.undo(run.record, trash: { try trash($0) })
        XCTAssertEqual(results[0].status, .refused)
        XCTAssertEqual(results[1].status, .trashed, "one problem does not stop the others")
        XCTAssertEqual(try String(contentsOf: run.input, encoding: .utf8), "input")
        XCTAssertFalse(trash.moved.contains(run.outs[0].path))
    }

    func testUndoReportsAFailedMovePerFileAndCanBeRetried() throws {
        let run = try makeRun()
        let trash = FakeTrash(bin: tmp())
        trash.failFor = [std(run.outs[1])]
        let first = OutputUndo.undo(run.record, trash: { try trash($0) })
        XCTAssertEqual(first.results.map { $0.status }, [.trashed, .failed])
        XCTAssertNotNil(first.results[1].detail)
        XCTAssertEqual(first.record.undoableCount, 1)
        trash.failFor = []
        let second = OutputUndo.undo(first.record, trash: { try trash($0) })
        XCTAssertEqual(second.results.map { $0.status }, [.trashed], "only the one that failed is tried again")
    }

    func testFolderOutputIsLeftAloneOnceSomethingWasAddedToIt() throws {
        let d = tmp()
        let start = Date().addingTimeInterval(-1)
        let folder = d.appendingPathComponent("archive")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        try Data("1".utf8).write(to: folder.appendingPathComponent("one.txt"))
        let rec = RunRecorder.record(task: "archive.extract", outcome: "succeeded", startedAt: start, finishedAt: Date(), inputs: [std(d.appendingPathComponent("archive.zip"))],
                                     reportedOutputs: [folder.path], summary: nil, home: std(d) + "-home")
        XCTAssertEqual(rec.outputs.first?.fingerprint.isDirectory, true)
        XCTAssertEqual(rec.outputs.first?.fingerprint.count, 1)
        try Data("mine".utf8).write(to: folder.appendingPathComponent("added-by-user.txt"))
        let trash = FakeTrash(bin: tmp())
        XCTAssertEqual(OutputUndo.undo(rec, trash: { try trash($0) }).results.map { $0.status }, [.modified])
        XCTAssertTrue(trash.moved.isEmpty)
    }

    func testRealTrashIsUsedByDefaultAndNothingIsDeletedPermanently() throws {
        // The default trasher is FileManager.trashItem; checked on a throwaway file the test itself created.
        let run = try makeRun()
        let single = RunRecord(id: UUID(), task: run.record.task, outcome: "succeeded", startedAt: run.record.startedAt, finishedAt: run.record.finishedAt,
                               inputCount: 1, summary: nil, outputs: [run.record.outputs[0]], untracked: 0, undoneAt: nil)
        let (_, results) = OutputUndo.undo(single)
        if results[0].status == .trashed {
            XCTAssertFalse(FileManager.default.fileExists(atPath: run.outs[0].path))
        } else {
            XCTAssertEqual(results[0].status, .failed, "an environment without a Trash reports the failure instead of deleting")
            XCTAssertTrue(FileManager.default.fileExists(atPath: run.outs[0].path))
        }
        XCTAssertTrue(FileManager.default.fileExists(atPath: run.input.path))
    }

    // MARK: history

    func fake(_ minutesAgo: Double, id: UUID = UUID()) -> RunRecord {
        RunRecord(id: id, task: "t", outcome: "succeeded", startedAt: Date(timeIntervalSinceNow: -minutesAgo * 60 - 1), finishedAt: Date(timeIntervalSinceNow: -minutesAgo * 60),
                  inputCount: 1, summary: nil, outputs: [], untracked: 0, undoneAt: nil)
    }

    func testHistoryIsCappedNewestFirstAndSurvivesRestart() throws {
        let url = tmp().appendingPathComponent("sub/run-history.json")
        let store = RunHistoryStore(url: url)
        XCTAssertEqual(store.load(), [])
        for i in (0..<30).reversed() { store.upsert(fake(Double(i))) } // oldest inserted first
        let loaded = RunHistoryStore(url: url).load()
        XCTAssertEqual(loaded.count, RunHistoryStore.maxRuns)
        XCTAssertEqual(loaded, loaded.sorted { $0.finishedAt > $1.finishedAt })
        XCTAssertGreaterThan(loaded[0].finishedAt, loaded[19].finishedAt)
        let attrs = try FileManager.default.attributesOfItem(atPath: url.path)
        XCTAssertEqual((attrs[.posixPermissions] as? NSNumber)?.intValue, 0o600)
    }

    func testLoadingRevalidatesOutputsSoATamperedFileCannotAimUndoAtOtherFiles() throws {
        let run = try makeRun()
        var rec = run.record
        let home = tmp().appendingPathComponent("home")
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        // edited history: an extra "output" that is the run's own input, the home folder itself, one relative path
        let bogus = [run.input.path, home.path, "relative/x"].compactMap { path -> RunOutput? in
            guard let fp = OutputFingerprint.of(path) else { return RunOutput(path: path, fingerprint: rec.outputs[0].fingerprint, status: nil, detail: nil) }
            return RunOutput(path: path, fingerprint: fp, status: nil, detail: nil)
        }
        let genuine = rec.outputs.count
        rec.outputs += bogus
        let store = RunHistoryStore(url: tmp().appendingPathComponent("h.json"))
        store.upsert(rec)
        let loaded = store.load(home: home.path).first
        XCTAssertEqual(loaded?.outputs.count, genuine)
        XCTAssertFalse(loaded?.outputs.contains { $0.path == run.input.path } ?? true)
        XCTAssertFalse(loaded?.outputs.contains { $0.path == home.path } ?? true)
    }

    func testHistoryUpsertReplacesByIdAndCappingHelperHandlesSmallLimits() {
        let url = tmp().appendingPathComponent("h.json")
        let store = RunHistoryStore(url: url)
        let id = UUID()
        store.upsert(fake(5, id: id))
        var changed = fake(5, id: id); changed.summary = "second"
        let list = store.upsert(changed)
        XCTAssertEqual(list.count, 1)
        XCTAssertEqual(list[0].summary, "second")
        XCTAssertEqual(RunHistoryStore.capped([fake(1), fake(2), fake(3)], limit: 2).count, 2)
        XCTAssertEqual(RunHistoryStore.capped([fake(1)], limit: 0).count, 0)
    }

    func testDamagedHistoryFileReadsAsEmptyAndIsReplacedOnNextSave() throws {
        let url = tmp().appendingPathComponent("h.json")
        try Data("{ not json".utf8).write(to: url)
        let store = RunHistoryStore(url: url)
        XCTAssertEqual(store.load(), [])
        XCTAssertEqual(store.upsert(fake(1)).count, 1)
        XCTAssertEqual(store.load().count, 1)
    }

    func testRecordedOutputsPerRunAreCapped() throws {
        let d = tmp()
        let start = Date().addingTimeInterval(-1)
        var outs: [String] = []
        for i in 0..<(RunRecorder.maxOutputsPerRun + 5) {
            let f = d.appendingPathComponent("o\(i).txt"); try Data("x".utf8).write(to: f); outs.append(f.path)
        }
        let r = RunRecorder.record(task: "pdf.convert.PNG", outcome: "succeeded", startedAt: start, finishedAt: Date(), inputs: [], reportedOutputs: outs, summary: nil, home: std(d) + "-home")
        XCTAssertEqual(r.outputs.count, RunRecorder.maxOutputsPerRun)
        XCTAssertEqual(r.untracked, 5)
    }

    // MARK: notifications and strings

    func testNotificationTextIsLocalized() throws {
        let en = Loc(language: .english), ja = Loc(language: .japanese)
        let a = NotificationText.make(outcome: "succeeded", summary: "Resized.", outputCount: 3, loc: en)
        XCTAssertEqual(a.title, "Flect: finished")
        XCTAssertEqual(a.body, "Resized. 3 files created")
        XCTAssertEqual(NotificationText.make(outcome: "succeeded", summary: nil, outputCount: 1, loc: en).body, "1 file created")
        XCTAssertEqual(NotificationText.make(outcome: "cancelled", summary: nil, outputCount: 0, loc: en).title, "Flect: cancelled")
        XCTAssertEqual(NotificationText.make(outcome: "failed", summary: nil, outputCount: 0, loc: en).body, en.t("progress.failed"))
        // a run whose Trash requests were not all carried out is recorded and announced as having problems
        XCTAssertEqual(RecordedOutcome.string("succeeded", problems: true), "partial")
        XCTAssertEqual(RecordedOutcome.string("succeeded", problems: false), "succeeded")
        XCTAssertEqual(RecordedOutcome.string("failed", problems: true), "failed")
        XCTAssertEqual(RecordedOutcome.string("cancelled", problems: true), "cancelled")
        let p = NotificationText.make(outcome: "partial", summary: "Moved 1 of 2 items to the Trash; 1 was left in place.", outputCount: 0, loc: en)
        XCTAssertEqual(p.title, "Flect: finished with problems")
        XCTAssertEqual(p.body, "Moved 1 of 2 items to the Trash; 1 was left in place.")
        XCTAssertEqual(NotificationText.make(outcome: "partial", summary: nil, outputCount: 0, loc: en).body, en.t("progress.failed"))
        let rec = RunRecorder.record(task: "folder.removeDuplicate?mode=oldest", outcome: RecordedOutcome.string("succeeded", problems: true),
                                     startedAt: Date(), finishedAt: Date(), inputs: [], reportedOutputs: [], summary: nil)
        XCTAssertEqual(rec.outcome, "partial")
        // deleting runs: recorded with counts even without outputs
        XCTAssertTrue(DeletionCounts.isDeletingTask("folder.removeDuplicate?mode=oldest"))
        XCTAssertTrue(DeletionCounts.isDeletingTask("folder.removeEmpty"))
        XCTAssertFalse(DeletionCounts.isDeletingTask("folder.removeEmptyX"))
        XCTAssertFalse(DeletionCounts.isDeletingTask("image.resize?scale=50"))
        XCTAssertEqual(DeletionCounts.removed(summaryKey: "msg.deletedPermanentlyPartial", args: ["7", "70", "2"], trash: .init()), 7)
        XCTAssertEqual(DeletionCounts.removed(summaryKey: "msg.cancelledAfterDeleting", args: ["3", "30"], trash: .init()), 3)
        XCTAssertEqual(DeletionCounts.removed(summaryKey: "msg.recycled", args: ["5", "50"], trash: .init(requested: 5, moved: 4)), 4)
        XCTAssertEqual(DeletionCounts.removed(summaryKey: "msg.duplicatesAllChanged", args: ["3"], trash: .init()), 0)
        XCTAssertTrue(DeletionCounts.leftAloneKeys.isSuperset(of: ["msg.duplicateChanged", "msg.emptyRemoveFailed", "msg.emptySkipped"]))
        var counted = rec
        counted.removed = 7; counted.leftAlone = 2
        let data = try JSONEncoder().encode(counted)
        XCTAssertEqual(try JSONDecoder().decode(RunRecord.self, from: data).leftAlone, 2)
        // records written before these fields existed still load
        var old = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        old["removed"] = nil; old["leftAlone"] = nil
        let legacy = try JSONDecoder().decode(RunRecord.self, from: JSONSerialization.data(withJSONObject: old))
        XCTAssertNil(legacy.removed)
        let j = NotificationText.make(outcome: "succeeded", summary: nil, outputCount: 2, loc: ja)
        XCTAssertEqual(j.title, "Flect: 完了しました")
        XCTAssertEqual(j.body, "2 個のファイルを作成")
    }

    func testEveryUxStringExistsInAllLanguages() throws {
        func keys(_ code: String) throws -> Set<String> {
            let url = try XCTUnwrap(Bundle.module.url(forResource: "strings.ux.\(code)", withExtension: "json", subdirectory: "Resources"))
            let obj = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: String])
            return Set(obj.keys)
        }
        let en = try keys("en").filter { !$0.hasSuffix(".one") }
        XCTAssertFalse(en.isEmpty)
        for code in ["ja", "zh"] { XCTAssertEqual(en.subtracting(try keys(code)).sorted(), [], "\(code) lacks keys") }
        // the ux table is merged into the normal lookup
        XCTAssertNotEqual(Loc(language: .japanese).t("ux.picker.none"), "ux.picker.none")
        XCTAssertTrue(Loc(language: .english).has("ux.results.undo"))
    }
}
