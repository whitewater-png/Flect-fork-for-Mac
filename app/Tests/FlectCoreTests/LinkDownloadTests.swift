import XCTest
@testable import FlectCore

/// Right-click downloads: link files (.webloc / .url) and selected text. No window or network involved.
final class LinkDownloadTests: XCTestCase {
    private func scratch() throws -> URL {
        let d = FileManager.default.temporaryDirectory.appendingPathComponent("flect-link-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: d) }
        return d
    }

    // MARK: link files

    func testWeblocFilesGiveTheirAddressInBothPlistFormats() throws {
        for format in [PropertyListSerialization.PropertyListFormat.xml, .binary] {
            let data = try PropertyListSerialization.data(fromPropertyList: ["URL": "https://example.com/watch?v=abc"], format: format, options: 0)
            XCTAssertEqual(LinkFile.address(in: data, fileExtension: "webloc"), "https://example.com/watch?v=abc")
            XCTAssertEqual(LinkFile.address(in: data, fileExtension: "WEBLOC"), "https://example.com/watch?v=abc")
        }
    }

    func testUrlFilesWithSectionBomAndWindowsLineEnds() {
        let text = "\u{FEFF}[InternetShortcut]\r\nURL=https://example.com/v\r\nIconIndex=0\r\n"
        XCTAssertEqual(LinkFile.address(in: Data(text.utf8), fileExtension: "url"), "https://example.com/v")
        XCTAssertEqual(LinkFile.address(in: Data("  url = ignored\nurl=https://a.example/x\n".utf8), fileExtension: "url"), "https://a.example/x")
    }

    func testOnlyPlainWebAddressesAreAccepted() throws {
        for bad in ["file:///etc/passwd", "ftp://example.com/a", "javascript:alert(1)", "https://user:pw@example.com/", "https://", "not a link", ""] {
            let plist = try PropertyListSerialization.data(fromPropertyList: ["URL": bad], format: .xml, options: 0)
            XCTAssertNil(LinkFile.address(in: plist, fileExtension: "webloc"), bad)
            XCTAssertNil(LinkFile.address(in: Data("[InternetShortcut]\nURL=\(bad)\n".utf8), fileExtension: "url"), bad)
        }
        let withoutKey = try PropertyListSerialization.data(fromPropertyList: ["Other": "https://example.com/"], format: .xml, options: 0)
        let wrongType = try PropertyListSerialization.data(fromPropertyList: ["URL": 5], format: .xml, options: 0)
        XCTAssertNil(LinkFile.address(in: withoutKey, fileExtension: "webloc"))
        XCTAssertNil(LinkFile.address(in: wrongType, fileExtension: "webloc"))
        XCTAssertNil(LinkFile.address(in: Data("garbage".utf8), fileExtension: "webloc"))
        XCTAssertNil(LinkFile.address(in: Data("URL=https://example.com/\n".utf8), fileExtension: "txt"), "only .webloc and .url")
        XCTAssertNil(LinkFile.address(in: Data(repeating: 0x20, count: LinkFile.maxBytes + 1), fileExtension: "url"))
    }

    func testReadingFromDiskReadsPlainFilesOnly() throws {
        let dir = try scratch()
        let good = dir.appendingPathComponent("Clip.webloc")
        try PropertyListSerialization.data(fromPropertyList: ["URL": "https://example.com/c"], format: .xml, options: 0).write(to: good)
        XCTAssertEqual(LinkFile.address(atPath: good.path), "https://example.com/c")

        let link = dir.appendingPathComponent("Alias.webloc")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: good)
        XCTAssertNil(LinkFile.address(atPath: link.path), "a symbolic link is not followed")

        let folder = dir.appendingPathComponent("Folder.webloc")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        XCTAssertNil(LinkFile.address(atPath: folder.path))
        XCTAssertNil(LinkFile.address(atPath: dir.appendingPathComponent("missing.url").path))

        let big = dir.appendingPathComponent("Big.url")
        var bigData = Data("URL=https://example.com/".utf8)
        bigData.append(Data(repeating: 0x20, count: LinkFile.maxBytes))
        try bigData.write(to: big)
        XCTAssertNil(LinkFile.address(atPath: big.path), "larger than 64 KiB is refused")
    }

    // MARK: selected text

    func testAddressesAreFoundInTextWithoutTheirPunctuation() {
        let text = "Watch https://a.example/v?x=1, then (https://b.example/w). See HTTPS://C.example/Z! And https://a.example/v?x=1 again."
        XCTAssertEqual(DownloadURL.addresses(inText: text), ["https://a.example/v?x=1", "https://b.example/w", "HTTPS://C.example/Z"])
        XCTAssertEqual(DownloadURL.addresses(inText: "https://en.wikipedia.org/wiki/Foo_(bar)"), ["https://en.wikipedia.org/wiki/Foo_(bar)"])
        XCTAssertEqual(DownloadURL.addresses(inText: "<https://a.example/x> \"https://b.example/y\""), ["https://a.example/x", "https://b.example/y"])
    }

    func testTextWithoutWebAddressesGivesNothing() {
        for text in ["", "no links here", "ftp://a.example/x file:///etc/passwd", "https://", "see example.com/video"] {
            XCTAssertEqual(DownloadURL.addresses(inText: text), [], text)
        }
    }

    func testTheNumberOfAddressesIsLimited() {
        let many = (1...50).map { "https://a.example/\($0)" }.joined(separator: " ")
        XCTAssertEqual(DownloadURL.addresses(inText: many).count, DownloadURL.maxPerRequest)
        XCTAssertEqual(DownloadURL.addresses(inText: many, limit: 3).count, 3)
    }

    // MARK: how it is reached

    func testLinkFilesAreAKindOfSelectionAndGetTheDownloadTask() throws {
        XCTAssertEqual(SelectionClassifier.classify(path: "/a/Page.WEBLOC", isDirectory: false), Classification(category: .link, format: "WEBLOC"))
        XCTAssertEqual(SelectionClassifier.classify(path: "/a/page.url", isDirectory: false), Classification(category: .link, format: "URL"))
        XCTAssertNil(SelectionClassifier.classify(path: "/a/.webloc", isDirectory: false), "a dotfile has no extension")

        let s = SettingsStore(directory: try scratch())
        XCTAssertEqual(Planner.resolve("link.download", settings: s), .native(.downloadLinks))
        XCTAssertEqual(Planner.resolveCatalogID("link.download", settings: s), .native(.downloadLinks))
        XCTAssertEqual(Planner.resolveCatalogID("link.download?url=https://a.example/", settings: s), .unknown, "no address can ride in as a parameter")

        let entries: [[String: Any]] = [["id": "link", "children": [["id": "link.download"]]], ["id": "pdf", "children": [["id": "pdf.preview"]]]]
        let catalog = try XCTUnwrap(MenuCatalog(menu: JSONValue(any: ["entries": entries])))
        let items = [SelectionItem(path: "/Users/a/Clip.webloc", isDirectory: false), SelectionItem(path: "/Users/a/Other.url", isDirectory: false),
                     SelectionItem(path: "/Users/a/doc.pdf", isDirectory: false)]
        let picked = TaskPicker.selection(for: items, catalog: catalog)
        XCTAssertEqual(picked.sections.map { $0.category }, [.pdf, .link])
        let link = try XCTUnwrap(picked.sections.first { $0.category == .link })
        XCTAssertEqual(link.paths, ["/Users/a/Clip.webloc", "/Users/a/Other.url"])
        XCTAssertEqual(link.entries.map { $0.id }, ["link.download"])
    }
}
