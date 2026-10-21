import Foundation

public enum Outcome: String {
    case succeeded, failed, cancelled
}

/// One line of engine output (see docs/mac-port/08-engine-protocol.ja.md).
/// The copy of a duplicate that stays, as the engine checked it.
public struct TrashKept: Equatable {
    public let path: String
    public let fingerprint: String
    public init(path: String, fingerprint: String) { self.path = path; self.fingerprint = fingerprint }
}

public enum EngineEvent: Equatable {
    case status(key: String, args: [String])
    case step(done: Int, total: Int)
    case fileStep(permille: Int)
    case info(key: String, args: [String])
    case warn(key: String, args: [String])
    case error(key: String, args: [String])
    /// `fingerprints`: for `trash`, path -> the fingerprint the engine verified (see `TrashGuard.fingerprint`).
    /// `kept`: path -> the copy that stays (a duplicate's), which must still match too. `ack`: the engine waits for
    /// `EngineClient.ackTrash()` before it goes on.
    case needs(what: String, paths: [String], fingerprints: [String: String], kept: [String: TrashKept], ack: Bool)
    case menu(JSONValue)
    case data(name: String, data: JSONValue)
    case done(outcome: Outcome, summaryKey: String?, summaryArgs: [String])
    /// Not sent by the engine: the process ended without a `done` event.
    case crashed

    public static func parse(line: Data) -> EngineEvent? {
        guard let v = JSONValue.parse(line), case .object = v, let kind = v["event"]?.string else { return nil }
        let key = v["key"]?.string ?? ""
        let args = (v["args"]?.array ?? []).compactMap { $0.string }
        switch kind {
        case "status": return .status(key: key, args: args)
        case "step": return .step(done: v["done"]?.int ?? 0, total: v["total"]?.int ?? 0)
        case "file_step": return .fileStep(permille: v["permille"]?.int ?? 0)
        case "info": return .info(key: key, args: args)
        case "warn": return .warn(key: key, args: args)
        case "error": return .error(key: key, args: args)
        case "needs":
            var fps: [String: String] = [:], kept: [String: TrashKept] = [:]
            for item in v["items"]?.array ?? [] {
                guard let p = item["path"]?.string else { continue }
                if let f = item["fingerprint"]?.string { fps[p] = f }
                if let kp = item["kept"]?["path"]?.string, let kf = item["kept"]?["fingerprint"]?.string {
                    kept[p] = TrashKept(path: kp, fingerprint: kf)
                }
            }
            return .needs(what: v["what"]?.string ?? "", paths: (v["paths"]?.array ?? []).compactMap { $0.string },
                          fingerprints: fps, kept: kept, ack: v["ack"]?.bool ?? false)
        case "menu": return v["menu"].map { .menu($0) }
        case "data": return .data(name: v["name"]?.string ?? "", data: v["data"] ?? .null)
        case "done":
            let outcome = Outcome(rawValue: v["outcome"]?.string ?? "") ?? .failed
            return .done(outcome: outcome, summaryKey: v["summary_key"]?.string, summaryArgs: (v["summary_args"]?.array ?? []).compactMap { $0.string })
        default: return nil
        }
    }
}
