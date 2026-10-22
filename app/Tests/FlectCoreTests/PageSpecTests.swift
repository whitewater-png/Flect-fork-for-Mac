import XCTest
@testable import FlectCore

final class PageSpecTests: XCTestCase {
    private func pages(_ text: String, _ count: Int) -> [Int]? {
        if case .success(let p) = PageSpec.pages(text, pageCount: count) { return p }
        return nil
    }

    func testSinglePagesAndRangesAreZeroBasedSortedAndUnique() {
        XCTAssertEqual(pages("1", 5), [0])
        XCTAssertEqual(pages("1-3, 5", 5), [0, 1, 2, 4])
        XCTAssertEqual(pages("5, 1-2, 2-3", 5), [0, 1, 2, 4])
        XCTAssertEqual(pages("3-", 5), [2, 3, 4])
        XCTAssertEqual(pages(" 2 ; 4 ", 4), [1, 3])
    }

    func testFullWidthInputIsAccepted() {
        XCTAssertEqual(pages("１－３、５", 5), [0, 1, 2, 4])
        XCTAssertEqual(pages("１，２", 3), [0, 1])
    }

    func testBadSyntaxIsRejected() {
        for text in ["", "   ", ",", "a", "0", "1-0", "3-2", "-3", "1--3", "1-2-3", "1,,x", "1.5", "+2", "１２３４５６７"] {
            XCTAssertEqual(PageSpec.pages(text, pageCount: 10), .failure(.syntax), "'\(text)'")
            XCTAssertFalse(PageSpec.isValidSyntax(text), "'\(text)'")
        }
        XCTAssertTrue(PageSpec.isValidSyntax("1-3,5,8-"))
    }

    func testPagesBeyondTheDocumentAreRejected() {
        XCTAssertEqual(PageSpec.pages("6", pageCount: 5), .failure(.outOfRange))
        XCTAssertEqual(PageSpec.pages("4-6", pageCount: 5), .failure(.outOfRange))
        XCTAssertEqual(PageSpec.pages("6-", pageCount: 5), .failure(.outOfRange))
        XCTAssertEqual(PageSpec.pages("1", pageCount: 0), .failure(.outOfRange))
        XCTAssertEqual(pages("5", 5), [4])
    }

    func testTooManyEntriesAreRejected() {
        let many = (1...(PageSpec.maxTokens + 1)).map(String.init).joined(separator: ",")
        XCTAssertEqual(PageSpec.pages(many, pageCount: 5000), .failure(.syntax))
    }
}
