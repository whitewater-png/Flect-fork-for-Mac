import Foundation

/// `--task <id> --input <file>`: the contract between a Quick Action and the app.
public struct LaunchRequest: Equatable {
    public static let maxPaths = 20_000
    public static let maxPathBytes = 4096

    public let task: String
    public let paths: [String]

    public enum Failure: Error, Equatable {
        case missingArguments
        case unreadable
        case empty
        case tooMany
        case badPath(String)
    }

    /// Arguments without the program name. The selection file is read and then deleted.
    public static func parse(arguments: [String], deleteInput: Bool = true) -> Result<LaunchRequest, Failure> {
        var task: String?, input: String?
        var i = 0
        while i + 1 < arguments.count {
            switch arguments[i] {
            case "--task": task = arguments[i + 1]; i += 2
            case "--input": input = arguments[i + 1]; i += 2
            default: i += 1
            }
        }
        guard let task = task, !task.trimmingCharacters(in: .whitespaces).isEmpty, let input = input else { return .failure(.missingArguments) }
        defer { if deleteInput { try? FileManager.default.removeItem(atPath: input) } }
        guard let data = FileManager.default.contents(atPath: input), data.count < 64 << 20, var text = String(data: data, encoding: .utf8) else { return .failure(.unreadable) }
        if text.hasPrefix("\u{FEFF}") { text.removeFirst() }
        let lines = text.components(separatedBy: .newlines).map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        if lines.isEmpty { return .failure(.empty) }
        if lines.count > maxPaths { return .failure(.tooMany) }
        for l in lines where !l.hasPrefix("/") || l.utf8.count > maxPathBytes || l.contains("\0") { return .failure(.badPath(String(l.prefix(80)))) }
        return .success(LaunchRequest(task: task, paths: lines))
    }
}
