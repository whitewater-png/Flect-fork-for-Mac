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
        if let failure = validate(paths: lines) { return .failure(failure) }
        return .success(LaunchRequest(task: task, paths: lines))
    }

    /// One path as the engine accepts it: absolute, bounded, no NUL.
    public static func isValidPath(_ p: String) -> Bool {
        p.hasPrefix("/") && p.utf8.count <= maxPathBytes && !p.contains("\0")
    }

    /// The rules every selection must pass before it reaches the planner, whichever entry point it came through
    /// (Quick Action file, Dock drop, Open panel, Services).
    public static func validate(paths: [String]) -> Failure? {
        if paths.isEmpty { return .empty }
        if paths.count > maxPaths { return .tooMany }
        for l in paths where !isValidPath(l) { return .badPath(String(l.prefix(80))) }
        return nil
    }
}
