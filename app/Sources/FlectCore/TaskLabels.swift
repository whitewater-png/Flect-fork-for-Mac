import Foundation

/// Names for the run history. Work the app does itself is recorded under ids such as `pdf.convert.JPG` or
/// `image.convert.HEIC` (the engine's equivalents are recorded as `image.convert?to=PNG`), which have no menu text
/// of their own.
public enum TaskLabels {
    /// `pdf.convert.JPG` -> (`pdf`, `JPG`): an id of three or more parts whose last part is a format name
    /// (capital letters and digits only). Anything else, such as `pdf.rotate.p1` or `folder.tree`, is nil.
    public static func categoryAndFormat(of id: String) -> (category: String, format: String)? {
        let parts = id.split(separator: ".").map(String.init)
        guard parts.count >= 3, let last = parts.last, !last.isEmpty,
              last.allSatisfy({ $0.isASCII && ($0.isUppercase || $0.isNumber) }) else { return nil }
        return (parts[0], last)
    }
}
