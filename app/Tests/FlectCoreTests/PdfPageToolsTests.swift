import XCTest
import PDFKit
@testable import FlectCore

/// Runs the real PDFKit (macOS only, so these run on the macOS CI job and on a Mac).
final class PdfPageToolsTests: XCTestCase {
    /// A PDF whose page `i` (0-based) is `100 + 10 * i` points wide, so every page can be told by its size.
    private func makePdf(pages: Int = 5) throws -> PDFDocument {
        let data = NSMutableData()
        let consumer = try XCTUnwrap(CGDataConsumer(data: data as CFMutableData))
        var first = CGRect(x: 0, y: 0, width: 100, height: 200)
        let context = try XCTUnwrap(CGContext(consumer: consumer, mediaBox: &first, nil))
        for i in 0..<pages {
            var box = CGRect(x: 0, y: 0, width: CGFloat(100 + 10 * i), height: 200)
            let boxData = Data(bytes: &box, count: MemoryLayout<CGRect>.size)
            let pageInfo: [CFString: Any] = [kCGPDFContextMediaBox: boxData]
            context.beginPDFPage(pageInfo as CFDictionary)
            context.setFillColor(CGColor(red: 0.2, green: 0.4, blue: 0.8, alpha: 1))
            context.fill(CGRect(x: 10, y: 10, width: 40, height: 40))
            context.endPDFPage()
        }
        context.closePDF()
        return try XCTUnwrap(PDFDocument(data: data as Data))
    }

    private func widths(_ doc: PDFDocument) -> [Int] {
        (0..<doc.pageCount).map { Int((doc.page(at: $0)?.bounds(for: .mediaBox).width ?? 0).rounded()) }
    }

    private func rotations(_ doc: PDFDocument) -> [Int] {
        (0..<doc.pageCount).map { doc.page(at: $0)?.rotation ?? -1 }
    }

    func testTheTestPdfHasFivePagesThatCanBeToldApart() throws {
        let doc = try makePdf()
        XCTAssertEqual(doc.pageCount, 5)
        XCTAssertEqual(widths(doc), [100, 110, 120, 130, 140])
        XCTAssertEqual(rotations(doc), [0, 0, 0, 0, 0])
    }

    func testSubsetKeepsTheRequestedPagesInTheGivenOrder() throws {
        let doc = try makePdf()
        XCTAssertEqual(widths(PdfPageTools.subset(of: doc, [3, 0])), [130, 100])
        XCTAssertEqual(widths(PdfPageTools.subset(of: doc, [1, 9, 2])), [110, 120], "an index outside the document is skipped")
        XCTAssertEqual(PdfPageTools.subset(of: doc, []).pageCount, 0)
    }

    func testEachPageGivesOneDocumentPerPageInOrder() throws {
        let doc = try makePdf()
        let parts = PdfPageTools.eachPage(of: doc)
        XCTAssertEqual(parts.count, 5)
        XCTAssertEqual(parts.map { $0.pageCount }, [1, 1, 1, 1, 1])
        XCTAssertEqual(parts.flatMap { widths($0) }, [100, 110, 120, 130, 140])
    }

    func testRotationSetsTheGivenTurnOnEveryPage() throws {
        let doc = try makePdf()
        for degrees in [90, 180, 270] {
            let turned = PdfPageTools.subset(of: doc, Array(0..<doc.pageCount), turn: degrees)
            XCTAssertEqual(turned.pageCount, 5)
            XCTAssertEqual(rotations(turned), Array(repeating: degrees, count: 5), "\(degrees)")
        }
        // turning an already turned page adds up, and a full turn is no turn
        let ninety = PdfPageTools.subset(of: doc, [0, 1], turn: 90)
        XCTAssertEqual(rotations(PdfPageTools.subset(of: ninety, [0], turn: 270)), [0])
        XCTAssertEqual(rotations(PdfPageTools.subset(of: ninety, [1], turn: 180)), [270])
        XCTAssertEqual(rotations(PdfPageTools.subset(of: doc, [0], turn: -90)), [270])
    }

    func testTheSourceDocumentIsNeverChanged() throws {
        let doc = try makePdf()
        _ = PdfPageTools.subset(of: doc, [0, 1, 2, 3, 4], turn: 90)
        _ = PdfPageTools.subset(of: doc, [2])
        _ = PdfPageTools.eachPage(of: doc)
        XCTAssertEqual(doc.pageCount, 5)
        XCTAssertEqual(widths(doc), [100, 110, 120, 130, 140])
        XCTAssertEqual(rotations(doc), [0, 0, 0, 0, 0])
    }

    func testIndexesThatStayWhenPagesAreDeleted() {
        XCTAssertEqual(PdfPageTools.indexes(keepingAllExcept: [0, 2], pageCount: 5), [1, 3, 4])
        XCTAssertEqual(PdfPageTools.indexes(keepingAllExcept: [4, 4, 0], pageCount: 5), [1, 2, 3])
        XCTAssertEqual(PdfPageTools.indexes(keepingAllExcept: [9], pageCount: 3), [0, 1, 2])
        XCTAssertEqual(PdfPageTools.indexes(keepingAllExcept: [0, 1, 2], pageCount: 3), [], "nothing would be left")
        XCTAssertEqual(PdfPageTools.indexes(keepingAllExcept: [], pageCount: 0), [])
    }

    func testDeleteAndExtractFollowThePageListText() throws {
        let doc = try makePdf()
        guard case .success(let removed) = PageSpec.pages("2, 4-", pageCount: doc.pageCount) else { return XCTFail("page list") }
        let kept = PdfPageTools.subset(of: doc, PdfPageTools.indexes(keepingAllExcept: removed, pageCount: doc.pageCount))
        XCTAssertEqual(widths(kept), [100, 120])
        guard case .success(let wanted) = PageSpec.pages("1-2, 5", pageCount: doc.pageCount) else { return XCTFail("page list") }
        XCTAssertEqual(widths(PdfPageTools.subset(of: doc, wanted)), [100, 110, 140])
    }

    func testTheResultSurvivesBeingWrittenAndReadAgain() throws {
        let doc = try makePdf()
        let turned = PdfPageTools.subset(of: doc, [4, 1, 3], turn: 180)
        let data = try XCTUnwrap(turned.dataRepresentation())
        let again = try XCTUnwrap(PDFDocument(data: data))
        XCTAssertEqual(again.pageCount, 3)
        XCTAssertEqual(widths(again), [140, 110, 130])
        XCTAssertEqual(rotations(again), [180, 180, 180])
        XCTAssertFalse(again.isLocked)
    }
}
