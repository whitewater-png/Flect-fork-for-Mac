import XCTest
@testable import FlectCore

final class TaskLabelsTests: XCTestCase {
    func testNativeConversionIdsAreSplitIntoCategoryAndFormat() {
        let jpg = TaskLabels.categoryAndFormat(of: "pdf.convert.JPG")
        XCTAssertEqual(jpg?.category, "pdf")
        XCTAssertEqual(jpg?.format, "JPG")
        XCTAssertEqual(TaskLabels.categoryAndFormat(of: "image.convert.HEIC")?.format, "HEIC")
        XCTAssertEqual(TaskLabels.categoryAndFormat(of: "pdf.convert.TXT")?.category, "pdf")
    }

    func testIdsWithTheirOwnMenuTextOrNoFormatAreLeftAlone() {
        for id in ["pdf.rotate.p1", "pdf.split.each", "pdf.split.range", "pdf.delete", "pdf.merge", "folder.tree",
                   "pdf.convert", "image.resize.p1", "download", "", "pdf.convert.", "a.b.c d"] {
            XCTAssertNil(TaskLabels.categoryAndFormat(of: id), id)
        }
    }

    func testEveryNativeConversionMenuIdGetsALabel() {
        // the ids `AppModel.nativeName` records for native conversions
        for id in ["image.convert.HEIC", "pdf.convert.PNG", "pdf.convert.JPG", "pdf.convert.TXT"] {
            XCTAssertNotNil(TaskLabels.categoryAndFormat(of: id), id)
        }
        // and the text it is built from exists in every language
        for lang in ["en", "ja", "zh"] {
            let url = Bundle.module.url(forResource: "strings.\(lang)", withExtension: "json", subdirectory: "Resources")
            let table = url.flatMap { try? Data(contentsOf: $0) }
                .flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: String] } ?? [:]
            XCTAssertNotNil(table["menu.to"], lang)
            XCTAssertNotNil(table["menu.pdf"], lang)
            XCTAssertNotNil(table["menu.image"], lang)
        }
    }
}
