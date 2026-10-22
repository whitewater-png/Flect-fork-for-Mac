// Prints the page count and, per page, the size, rotation and first line of text of each PDF given.
//   swift scripts/mac-verify/pdf-info.swift file.pdf [more.pdf ...]
// Needs the Xcode Command Line Tools (the same ones the app is built with). Changes nothing.
import Foundation
import PDFKit

for path in CommandLine.arguments.dropFirst() {
    guard let doc = PDFDocument(url: URL(fileURLWithPath: path)) else {
        print("\(path): cannot be opened as a PDF")
        continue
    }
    print("\(path): \(doc.pageCount) page(s)" + (doc.isEncrypted ? ", encrypted" : ""))
    for i in 0..<doc.pageCount {
        guard let page = doc.page(at: i) else { continue }
        let box = page.bounds(for: .mediaBox)
        let first = (page.string ?? "").split(whereSeparator: { $0 == "\n" || $0 == "\r" }).first.map(String.init) ?? ""
        print("  page \(i + 1): \(Int(box.width.rounded())) x \(Int(box.height.rounded())) pt, rotation \(page.rotation), text: \(first)")
    }
}
