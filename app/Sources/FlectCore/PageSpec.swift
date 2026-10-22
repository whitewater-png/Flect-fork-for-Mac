import Foundation

/// Page lists typed by the user, such as `1-3, 5, 8-` (pages count from 1; `8-` runs to the last page).
/// Pure text handling so it can be tested without PDFKit.
public enum PageSpec {
    public enum Failure: Error, Equatable {
        /// Not a list of pages and ranges.
        case syntax
        /// A page beyond the document (or a document with no pages).
        case outOfRange
    }

    enum Token: Equatable {
        case page(Int)
        /// `from-to`, or `from-` when `to` is nil.
        case span(from: Int, to: Int?)
    }

    static let maxTokens = 1000
    static let maxDigits = 6

    /// Full-width digits, commas and hyphens (Japanese input) become plain ones.
    static func normalized(_ text: String) -> String {
        text.applyingTransform(.fullwidthToHalfwidth, reverse: false) ?? text
    }

    static func number(_ s: String) -> Int? {
        guard !s.isEmpty, s.count <= maxDigits, s.allSatisfy({ $0.isASCII && $0.isNumber }), let v = Int(s), v >= 1 else { return nil }
        return v
    }

    static func tokens(_ text: String) -> [Token]? {
        let separators = CharacterSet(charactersIn: ",;、；")
        let parts = normalized(text).components(separatedBy: separators)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        guard !parts.isEmpty, parts.count <= maxTokens else { return nil }
        var out: [Token] = []
        for part in parts {
            let sides = part.components(separatedBy: "-").map { $0.trimmingCharacters(in: .whitespaces) }
            switch sides.count {
            case 1:
                guard let p = number(sides[0]) else { return nil }
                out.append(.page(p))
            case 2:
                guard let from = number(sides[0]) else { return nil }
                if sides[1].isEmpty {
                    out.append(.span(from: from, to: nil))
                } else {
                    guard let to = number(sides[1]), to >= from else { return nil }
                    out.append(.span(from: from, to: to))
                }
            default:
                return nil
            }
        }
        return out
    }

    /// True when the text is a well-formed list (the page count is not known yet, e.g. while typing in the dialog).
    public static func isValidSyntax(_ text: String) -> Bool { tokens(text) != nil }

    /// The pages named by `text`, as zero-based indexes in ascending order without repeats.
    public static func pages(_ text: String, pageCount: Int) -> Swift.Result<[Int], Failure> {
        guard let list = tokens(text) else { return .failure(.syntax) }
        guard pageCount >= 1 else { return .failure(.outOfRange) }
        var chosen = Set<Int>()
        for token in list {
            switch token {
            case .page(let p):
                guard p <= pageCount else { return .failure(.outOfRange) }
                chosen.insert(p - 1)
            case .span(let from, let to):
                let last = to ?? pageCount
                guard from <= pageCount, last <= pageCount else { return .failure(.outOfRange) }
                for p in from...last { chosen.insert(p - 1) }
            }
        }
        return .success(chosen.sorted())
    }
}
