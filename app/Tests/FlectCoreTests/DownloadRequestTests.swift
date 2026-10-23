import XCTest
@testable import FlectCore

final class DownloadRequestTests: XCTestCase {
    func testPlainWebAddressesAreAccepted() {
        XCTAssertEqual(DownloadURL.normalized("  https://example.com/watch?v=abc123  "), "https://example.com/watch?v=abc123")
        XCTAssertEqual(DownloadURL.normalized("HTTP://Example.com"), "HTTP://Example.com")
        XCTAssertTrue(DownloadURL.isPlausible("https://a.example:8080/x#y"))
    }

    func testEverythingElseIsRefused() {
        for text in ["", "   ", "--exec=rm", "file:///etc/passwd", "ftp://example.com/a", "example.com/video",
                     "https://", "https:///path", "https://user:pass@example.com/", "https://example.com/a b",
                     "https://example.com/\n--exec=x", "https://example.com/\u{0007}"] {
            XCTAssertNil(DownloadURL.normalized(text), text)
        }
        XCTAssertNil(DownloadURL.normalized("https://example.com/" + String(repeating: "a", count: DownloadURL.maxBytes)))
    }

    func testChoicesMatchWhatTheEngineAccepts() {
        // the engine's `download::kind_from`: video with no cap or one of these caps, or audio with none
        let allowed: Set<Int> = [480, 720, 1080, 1440, 2160]
        for c in DownloadChoice.allCases {
            XCTAssertTrue(c.kind == "video" || c.kind == "audio")
            if c.kind == "audio" { XCTAssertNil(c.maxHeight) }
            if let h = c.maxHeight { XCTAssertTrue(allowed.contains(h), c.rawValue) }
        }
        XCTAssertEqual(DownloadChoice.allCases.map { $0.rawValue }, ["best", "p1080", "p720", "p480", "audio"])
        XCTAssertEqual(DownloadChoice.p720.labelKey, "dialog.download.q.p720")
    }
}
