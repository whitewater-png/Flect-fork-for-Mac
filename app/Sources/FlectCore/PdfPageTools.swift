import Foundation
import PDFKit

/// Page operations on a PDF. Every function builds a NEW document from copies of the pages and never changes
/// the document it is given. Kept here, apart from the window code, so the tests can run it with the real PDFKit.
public enum PdfPageTools {
    /// A copy that belongs to no document, so it can go into a new one and the source stays intact.
    static func freePage(_ page: PDFPage) -> PDFPage { (page.copy() as? PDFPage) ?? page }

    /// A new document with the pages at `indexes` (zero-based, in the order given; an index outside the document is
    /// skipped), each turned clockwise by `degrees` (a multiple of 90).
    public static func subset(of doc: PDFDocument, _ indexes: [Int], turn degrees: Int = 0) -> PDFDocument {
        let out = PDFDocument()
        for i in indexes {
            guard let page = doc.page(at: i) else { continue }
            let copy = freePage(page)
            if degrees != 0 { copy.rotation = ((copy.rotation + degrees) % 360 + 360) % 360 }
            out.insert(copy, at: out.pageCount)
        }
        return out
    }

    /// The page indexes that stay when `removed` goes. Empty when nothing would be left.
    public static func indexes(keepingAllExcept removed: [Int], pageCount: Int) -> [Int] {
        let gone = Set(removed)
        return (0..<max(0, pageCount)).filter { !gone.contains($0) }
    }

    /// One new document per page, in page order.
    public static func eachPage(of doc: PDFDocument) -> [PDFDocument] {
        (0..<doc.pageCount).map { subset(of: doc, [$0]) }
    }
}
