import XCTest
@testable import FlectCore

final class CoreTests: XCTestCase {
    func tmp() -> URL {
        let d = FileManager.default.temporaryDirectory.appendingPathComponent("flect-test-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    func testEventParsing() {
        let line = #"{"event":"done","outcome":"cancelled","summary_key":null,"summary_args":["a"]}"#
        XCTAssertEqual(EngineEvent.parse(line: Data(line.utf8)), .done(outcome: .cancelled, summaryKey: nil, summaryArgs: ["a"]))
        XCTAssertEqual(EngineEvent.parse(line: Data(#"{"event":"step","done":2,"total":5}"#.utf8)), .step(done: 2, total: 5))
        XCTAssertEqual(EngineEvent.parse(line: Data(#"{"event":"file_step","permille":500}"#.utf8)), .fileStep(permille: 500))
        XCTAssertEqual(EngineEvent.parse(line: Data(#"{"event":"needs","what":"trash","paths":["/a","/b"]}"#.utf8)), .needs(what: "trash", paths: ["/a", "/b"]))
        XCTAssertEqual(EngineEvent.parse(line: Data(#"{"event":"error","key":"msg.ioError","args":["x"]}"#.utf8)), .error(key: "msg.ioError", args: ["x"]))
        XCTAssertNil(EngineEvent.parse(line: Data("garbage".utf8)))
        XCTAssertNil(EngineEvent.parse(line: Data(#"{"event":"nope"}"#.utf8)))
        if case .data(let name, let d)? = EngineEvent.parse(line: Data(#"{"event":"data","name":"tree","data":{"text":"a\nb"}}"#.utf8)) {
            XCTAssertEqual(name, "tree"); XCTAssertEqual(d["text"]?.string, "a\nb")
        } else { XCTFail() }
    }

    func testLineBufferKeepsPartialLines() {
        let b = LineBuffer()
        XCTAssertEqual(b.append(Data("{\"a\":1}\n{\"b".utf8)).count, 1)
        let rest = b.append(Data("\":2}\n\n".utf8))
        XCTAssertEqual(rest.map { String(data: $0, encoding: .utf8)! }, ["{\"b\":2}"])
    }

    func testLaunchRequest() throws {
        let dir = tmp()
        let file = dir.appendingPathComponent("sel.txt")
        try "\u{FEFF}/Users/a/写真 1.png\r\n/Users/a/b.png\n\n".write(to: file, atomically: true, encoding: .utf8)
        let r = LaunchRequest.parse(arguments: ["--task", "image.ocr", "--input", file.path])
        XCTAssertEqual(try r.get(), LaunchRequest(task: "image.ocr", paths: ["/Users/a/写真 1.png", "/Users/a/b.png"]))
        XCTAssertFalse(FileManager.default.fileExists(atPath: file.path), "the selection file is deleted after reading")

        let bad = dir.appendingPathComponent("bad.txt")
        try "relative/path.png\n".write(to: bad, atomically: true, encoding: .utf8)
        XCTAssertEqual(LaunchRequest.parse(arguments: ["--task", "x", "--input", bad.path]), .failure(.badPath("relative/path.png")))
        XCTAssertEqual(LaunchRequest.parse(arguments: ["--task", "x"]), .failure(.missingArguments))
        XCTAssertEqual(LaunchRequest.parse(arguments: ["--task", "x", "--input", "/nonexistent/file"]), .failure(.unreadable))
        let empty = dir.appendingPathComponent("empty.txt")
        try "\n \n".write(to: empty, atomically: true, encoding: .utf8)
        XCTAssertEqual(LaunchRequest.parse(arguments: ["--task", "x", "--input", empty.path]), .failure(.empty))
    }

    func testOutputPathNeverOverwrites() throws {
        let dir = tmp()
        let src = dir.appendingPathComponent("a.png")
        FileManager.default.createFile(atPath: src.path, contents: Data())
        XCTAssertEqual(OutputPath.derive(source: src, suffix: "_x", ext: "jpg").lastPathComponent, "a_x.jpg")
        let first = try OutputPath.writeNew(Data("1".utf8), source: src, suffix: "_x", ext: "jpg")
        let second = try OutputPath.writeNew(Data("2".utf8), source: src, suffix: "_x", ext: ".jpg")
        XCTAssertEqual(first.lastPathComponent, "a_x.jpg")
        XCTAssertEqual(second.lastPathComponent, "a_x (2).jpg")
        XCTAssertEqual(try String(contentsOf: first), "1")
    }

    func testSettingsKeepUnknownKeysAndUseDefaults() throws {
        let dir = tmp()
        try Data("\u{FEFF}{\"Image\":{\"Presets\":{\"ResizeA\":80}},\"Mystery\":{\"x\":[1,2]}}".utf8).write(to: dir.appendingPathComponent("settings.json"))
        let s = SettingsStore(directory: dir)
        XCTAssertEqual(s.double("Image", "Presets", "ResizeA", default: 50), 80)
        XCTAssertEqual(s.double("Image", "Presets", "ResizeB", default: 25), 25)
        s.set(["Video", "Codec"], "H265")
        try s.save()
        let again = SettingsStore(directory: dir)
        XCTAssertEqual(again.string("Video", "Codec"), "H265")
        XCTAssertNotNil(again.get("Mystery"))
        let attrs = try FileManager.default.attributesOfItem(atPath: dir.appendingPathComponent("settings.json").path)
        XCTAssertEqual((attrs[.posixPermissions] as? NSNumber)?.intValue, 0o600)
    }

    func testPlanner() {
        let s = SettingsStore(directory: tmp())
        XCTAssertEqual(Planner.resolve("image.resize.p1", settings: s), .engine("image.resize?scale=50"))
        XCTAssertEqual(Planner.resolve("image.resize.p2", settings: s), .engine("image.resize?scale=25"))
        XCTAssertEqual(Planner.resolve("image.compress.p4", settings: s), .engine("image.compress?bytes=1048576"))
        XCTAssertEqual(Planner.resolve("video.compress.discord", settings: s), .engine("video.compress?preset=discord"))
        XCTAssertEqual(Planner.resolve("image.convert.WEBP", settings: s), .engine("image.convert?to=WEBP"))
        XCTAssertEqual(Planner.resolve("text.encode.UTF8BOM", settings: s), .engine("text.encode?to=UTF8BOM"))
        XCTAssertEqual(Planner.resolve("video.extractAudio.MP3", settings: s), .engine("video.extractAudio?to=MP3"))
        XCTAssertEqual(Planner.resolve("image.convert", settings: s), .choose(group: "image.resize".replacingOccurrences(of: "resize", with: "convert")))
        XCTAssertEqual(Planner.resolve("image.convert.HEIC", settings: s), .native(.heicConvert))
        XCTAssertEqual(Planner.resolve("pdf.convert.TXT", settings: s), .native(.pdfText))
        XCTAssertEqual(Planner.resolve("image.resize.custom", settings: s), .dialog(.pixels(base: "image.resize")))
        XCTAssertEqual(Planner.resolve("folder.rename", settings: s), .dialog(.rename))
        XCTAssertEqual(Planner.resolve("video.subtitles", settings: s), .engine("video.subtitles"))
        XCTAssertEqual(Planner.resolve("audio.subtitles", settings: s), .engine("audio.subtitles"))
        XCTAssertEqual(Planner.resolve("json.sort", settings: s), .engine("json.sort"))
        XCTAssertEqual(Planner.resolve("nope.nothing", settings: s), .unknown)
        XCTAssertEqual(Planner.resolve("image.resize?scale=10", settings: s), .engine("image.resize?scale=10"))
        s.set(["Image", "Presets", "ResizeA"], 12.5)
        XCTAssertEqual(Planner.resolve("image.resize.p1", settings: s), .engine("image.resize?scale=12.5"))
    }

    func testBackgroundRemovalFollowsTheMode() {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("flect-bg-\(UUID().uuidString)")
        let s = SettingsStore(directory: dir)
        XCTAssertEqual(Planner.resolve("image.chromakey", settings: s), .native(.removeBackground(fallback: true)))
        s.set(["Image", "BackgroundMode"], "AiOnly")
        XCTAssertEqual(Planner.resolve("image.chromakey", settings: s), .native(.removeBackground(fallback: false)))
        s.set(["Image", "BackgroundMode"], "ChromaKeyOnly")
        XCTAssertEqual(Planner.resolve("image.chromakey", settings: s), .engine("image.chromakey"))
    }

    func testPlannerParameterEncoding() {
        XCTAssertEqual(Planner.resizeCustom(base: "image.resize", width: 800, height: 0, keepAspect: true), "image.resize?scale=custom&w=800&h=0&keep=1")
        XCTAssertEqual(Planner.rename(prefix: "旅行 ", name: "", suffix: "&", numbered: true, dryRun: true),
                       "folder.rename?prefix=%E6%97%85%E8%A1%8C%20&name=&suffix=%26&numbered=1&dry=1")
        XCTAssertEqual(Planner.trim(start: 1, end: 2.5), "video.trim?start=1.000&end=2.500")
    }

    func testLocalizationAndFormatting() {
        let en = Loc(language: .english), ja = Loc(language: .japanese), zh = Loc(language: .chinese)
        XCTAssertEqual(en.t("common.ok"), "OK")
        XCTAssertNotEqual(ja.t("msg.ioError"), en.t("msg.ioError"))
        XCTAssertNotEqual(zh.t("msg.ioError"), en.t("msg.ioError"))
        XCTAssertEqual(en.f("msg.processing", ["a.png"]), "Processing a.png")
        XCTAssertEqual(en.t("no.such.key"), "no.such.key")
        XCTAssertEqual(en.n("count.file", 1), "1 file")
        XCTAssertEqual(en.n("count.file", 3), "3 files")
        XCTAssertEqual(AppLanguage.from(setting: "System", preferred: ["ja-JP"]), .japanese)
        XCTAssertEqual(AppLanguage.from(setting: "System", preferred: ["zh-Hans-CN"]), .chinese)
        XCTAssertEqual(AppLanguage.from(setting: "English", preferred: ["ja-JP"]), .english)
    }

    func testEveryEngineKeyHasATranslation() throws {
        for (code, lang) in [("ja", AppLanguage.japanese), ("zh", .chinese)] {
            let loc = Loc(language: lang)
            for key in ["msg.processing", "msg.archiveBomb", "msg.ffmpegMissing", "msg.pdfNeedTwo", "app.keepWhich", "task.unimplemented", "msg.whisperMissing", "msg.noSpeech"] {
                XCTAssertNotEqual(loc.t(key), Loc(language: .english).t(key), "\(code) lacks \(key)")
            }
        }
    }

    func testFormatChooserLabelsAreTranslated() {
        for lang in [AppLanguage.english, .japanese, .chinese] {
            let loc = Loc(language: lang)
            XCTAssertTrue(loc.has("menu.to"))
            XCTAssertTrue(loc.f("menu.to", ["PNG"]).contains("PNG"), "\(lang) menu.to lacks {0}")
        }
    }

    /// Every language must carry every key the English table has (a missing key shows up as raw text in the UI).
    func testAllLanguagesHaveAllKeys() throws {
        func keys(_ code: String) throws -> Set<String> {
            let url = try XCTUnwrap(Bundle.module.url(forResource: "strings.\(code)", withExtension: "json", subdirectory: "Resources"))
            let obj = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: String])
            return Set(obj.keys)
        }
        let en = try keys("en").filter { !$0.hasSuffix(".one") } // singular forms exist only in English
        for code in ["ja", "zh"] {
            let missing = en.subtracting(try keys(code)).sorted()
            XCTAssertTrue(missing.isEmpty, "\(code) is missing: \(missing.prefix(10))")
        }
    }

    /// Drives the real client against a stand-in engine script.
    func testEngineClientRoundTrip() async throws {
        let dir = tmp()
        let script = dir.appendingPathComponent("flect-engine")
        try """
        #!/bin/sh
        read line
        echo '{"event":"status","key":"msg.processing","args":["x"]}'
        echo '{"event":"step","done":1,"total":1}'
        echo '{"event":"done","outcome":"succeeded","summary_key":null,"summary_args":[]}'
        sleep 5
        """.write(to: script, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: script.path)
        let client = EngineClient(executable: script)
        var events: [EngineEvent] = []
        for await e in client.run(task: "json.sort", paths: ["/a"]) { events.append(e) }
        XCTAssertEqual(events.count, 3)
        XCTAssertEqual(events.last, .done(outcome: .succeeded, summaryKey: nil, summaryArgs: []))
    }

    func testEngineClientReportsACrash() async throws {
        let dir = tmp()
        let script = dir.appendingPathComponent("flect-engine")
        try "#!/bin/sh\nexit 3\n".write(to: script, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: script.path)
        var events: [EngineEvent] = []
        for await e in EngineClient(executable: script).run(task: "x", paths: ["/a"]) { events.append(e) }
        XCTAssertEqual(events, [.crashed])
    }

    /// Closing the window after the engine has gone used to raise an Objective-C exception (EPIPE) and kill the app.
    func testCancelAfterTheEngineExitedDoesNotCrash() async throws {
        let dir = tmp()
        let script = dir.appendingPathComponent("flect-engine")
        try "#!/bin/sh\nexit 0\n".write(to: script, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: script.path)
        let client = EngineClient(executable: script)
        for await _ in client.run(task: "x", paths: ["/a"]) {}
        try await Task.sleep(nanoseconds: 500_000_000)
        for _ in 0..<3 { client.cancel() }
        try await Task.sleep(nanoseconds: 300_000_000)
    }

    // MARK: dependency check

    func testDependencyCheckReportsWhatIsInstalled() throws {
        let fm = FileManager.default
        let root = fm.temporaryDirectory.appendingPathComponent("flect-deps-\(UUID().uuidString)")
        let bin = root.appendingPathComponent("bin"), data = root.appendingPathComponent("data")
        try fm.createDirectory(at: bin, withIntermediateDirectories: true)
        try fm.createDirectory(at: data.appendingPathComponent("deps/whisper"), withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: root) }
        for t in ["ffmpeg", "ffprobe"] {
            let p = bin.appendingPathComponent(t).path
            fm.createFile(atPath: p, contents: Data("#!/bin/sh\n".utf8), attributes: [.posixPermissions: 0o755])
        }
        fm.createFile(atPath: data.appendingPathComponent("deps/whisper/ggml-tiny.bin").path, contents: Data("x".utf8))
        let r = DependencyCheck.check(dataDir: data, modelSetting: "Tiny", scripts: "/app/scripts", dirs: [bin.path], hasWhisper: { _ in true }, hasJava: { false })
        func item(_ id: String) -> DependencyStatus { r.first { $0.id == id }! }
        XCTAssertTrue(item("ffmpeg").ok)
        XCTAssertTrue(item("whisper").ok)
        XCTAssertTrue(item("model").ok)
        XCTAssertFalse(item("7zz").ok)
        XCTAssertTrue(item("7zz").required)
        XCTAssertFalse(item("java").ok)
        XCTAssertFalse(item("cfr").ok)
        XCTAssertEqual(item("cfr").command, "\"/app/scripts/fetch-cfr.sh\"")
        XCTAssertEqual(item("model").command, "\"/app/scripts/fetch-whisper-model.sh\" tiny")
        XCTAssertEqual(item("7zz").command, "brew install sevenzip")
    }

    func testFilterListingParsing() {
        let listing = " T. adelay            A->A       Delay one or more audio channels.\n .. whisper           A->A       Transcribe audio using whisper.cpp.\n"
        XCTAssertTrue(DependencyCheck.hasFilter(in: listing, "whisper"))
        XCTAssertFalse(DependencyCheck.hasFilter(in: listing, "delay"))
        XCTAssertTrue(DependencyCheck.hasFilter(in: listing, "adelay"))
        XCTAssertFalse(DependencyCheck.hasFilter(in: "", "whisper"))
    }
}
