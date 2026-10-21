import Foundation

// Run history and "Undo": what a run created, remembered precisely enough that undoing it can only ever
// move those exact files to the Trash, and only while they are still exactly as the run left them.

/// What a file (or folder) looked like when the run finished. Undo compares against it, so anything the
/// user edited, replaced or filled up afterwards is left alone.
public struct OutputFingerprint: Codable, Equatable {
    public var isDirectory: Bool
    /// File: byte size. Folder: sum of the sizes of everything inside.
    public var size: Int64
    /// File: modification time. Folder: the newest modification time in the tree (seconds since 1970).
    public var modified: Double
    /// Folder: number of entries inside. File: 0.
    public var count: Int

    public static let maxFolderEntries = 100_000

    /// `nil` for anything that is not a plain file or folder (missing, symlink, device, too big to scan...).
    /// Uses lstat semantics: a symlink is never followed.
    public static func of(_ path: String, fm: FileManager = .default) -> OutputFingerprint? {
        guard let a = try? fm.attributesOfItem(atPath: path), let type = a[.type] as? FileAttributeType else { return nil }
        let mtime = (a[.modificationDate] as? Date)?.timeIntervalSince1970 ?? 0
        switch type {
        case .typeRegular:
            return OutputFingerprint(isDirectory: false, size: (a[.size] as? NSNumber)?.int64Value ?? 0, modified: mtime, count: 0)
        case .typeDirectory:
            var size: Int64 = 0, newest = mtime, n = 0
            guard let walker = fm.enumerator(atPath: path) else { return nil }
            for case let rel as String in walker {
                n += 1
                if n > maxFolderEntries { return nil }
                guard let ia = try? fm.attributesOfItem(atPath: (path as NSString).appendingPathComponent(rel)) else { return nil }
                if let m = (ia[.modificationDate] as? Date)?.timeIntervalSince1970, m > newest { newest = m }
                if (ia[.type] as? FileAttributeType) == .typeRegular { size += (ia[.size] as? NSNumber)?.int64Value ?? 0 }
            }
            return OutputFingerprint(isDirectory: true, size: size, modified: newest, count: n)
        default:
            return nil
        }
    }

    /// Times are compared to the millisecond (they round-trip through JSON as doubles).
    public func matches(_ other: OutputFingerprint) -> Bool {
        isDirectory == other.isDirectory && size == other.size && count == other.count && abs(modified - other.modified) < 0.001
    }
}

public enum UndoStatus: String, Codable, Equatable {
    case trashed    // moved to the Trash
    case missing    // already gone
    case modified   // changed (or replaced) after the run: left alone
    case refused    // failed a safety check: left alone
    case failed     // the Trash move itself failed
}

public struct RunOutput: Codable, Equatable {
    public var path: String
    public var fingerprint: OutputFingerprint
    public var status: UndoStatus?
    public var detail: String?
}

public struct RunRecord: Codable, Equatable, Identifiable {
    public var id: UUID
    public var task: String
    public var outcome: String          // succeeded / partial / failed / cancelled
    public var startedAt: Date
    public var finishedAt: Date
    public var inputCount: Int
    public var summary: String?
    public var outputs: [RunOutput]
    /// Outputs the engine reported that were not tracked (failed a safety check, or over the cap).
    public var untracked: Int
    public var undoneAt: Date?
    /// The (standardized) input paths of the run, so `RunHistoryStore.load` can re-check that no output is an input.
    /// Absent in records written by older versions.
    public var inputs: [String]? = nil
    /// Deleting tasks (Remove Duplicate, Remove Empty): items removed or moved to the Trash, and items left alone
    /// (changed, refused, or could not be removed). Absent for other tasks and in older records.
    public var removed: Int? = nil
    public var leftAlone: Int? = nil

    /// Outputs that undo can still act on.
    public var undoableCount: Int { outputs.filter { $0.status != .trashed }.count }
}

// MARK: recording

public enum OutputRejection: Equatable {
    case notAbsolute, tooShallow, homeOrAbove, isInput, containsInput, unreadable, createdBeforeRun
}

public enum RunRecorder {
    public static let maxOutputsPerRun = 2000

    /// Why an output the engine reported must not be tracked for undo (`nil` = fine).
    /// The point is to never put a file in the undo list that this run did not create.
    public static func rejection(of path: String, inputs: Set<String>, startedAt: Date, home: String, fm: FileManager = .default) -> OutputRejection? {
        guard LaunchRequest.isValidPath(path) else { return .notAbsolute }
        let std = URL(fileURLWithPath: path).standardizedFileURL.path
        if std.split(separator: "/").count < 2 { return .tooShallow }
        if std == home || home.hasPrefix(std + "/") { return .homeOrAbove }
        if inputs.contains(std) { return .isInput }
        if inputs.contains(where: { $0.hasPrefix(std + "/") }) { return .containsInput }
        guard OutputFingerprint.of(std, fm: fm) != nil,
              let created = (try? fm.attributesOfItem(atPath: std))?[.creationDate] as? Date else { return .unreadable }
        // Created by this run: not before it started (2 s of slack for coarse clocks).
        if created < startedAt.addingTimeInterval(-2) { return .createdBeforeRun }
        return nil
    }

    public static func record(task: String, outcome: String, startedAt: Date, finishedAt: Date, inputs: [String], reportedOutputs: [String],
                              summary: String?, home: String = FileManager.default.homeDirectoryForCurrentUser.path, fm: FileManager = .default) -> RunRecord {
        let inputSet = Set(inputs.map { URL(fileURLWithPath: $0).standardizedFileURL.path })
        var seen = Set<String>()
        var outputs: [RunOutput] = []
        var untracked = 0
        for raw in reportedOutputs {
            let std = URL(fileURLWithPath: raw).standardizedFileURL.path
            if !seen.insert(std).inserted { continue }
            if outputs.count >= maxOutputsPerRun { untracked += 1; continue }
            guard rejection(of: raw, inputs: inputSet, startedAt: startedAt, home: home, fm: fm) == nil,
                  let fp = OutputFingerprint.of(std, fm: fm) else { untracked += 1; continue }
            outputs.append(RunOutput(path: std, fingerprint: fp, status: nil, detail: nil))
        }
        return RunRecord(id: UUID(), task: task, outcome: outcome, startedAt: startedAt, finishedAt: finishedAt, inputCount: inputs.count,
                         summary: summary, outputs: outputs, untracked: untracked, undoneAt: nil,
                         inputs: Array(inputSet.sorted().prefix(maxOutputsPerRun)))
    }
}

// MARK: undo

public struct UndoResult: Equatable {
    public let path: String
    public let status: UndoStatus
    public let detail: String?
}

public enum OutputUndo {
    public typealias Trasher = (URL) throws -> Void

    /// Moves to the Trash (recoverable). Never a permanent delete.
    public static let systemTrash: Trasher = { url in
        try FileManager.default.trashItem(at: url, resultingItemURL: nil)
    }

    /// Moves the outputs of `record` to the Trash, one by one, each only if it still exists, is still a plain
    /// file/folder (not swapped for a symlink) and still matches its fingerprint from the end of the run.
    /// Nothing outside `record.outputs` is ever touched. Returns the updated record and one result per output tried.
    public static func undo(_ record: RunRecord, now: Date = Date(), fm: FileManager = .default, trash: Trasher = systemTrash) -> (record: RunRecord, results: [UndoResult]) {
        var updated = record
        var results: [UndoResult] = []
        for i in updated.outputs.indices {
            let out = updated.outputs[i]
            if out.status == .trashed { continue }
            let (status, detail) = attempt(out, fm: fm, trash: trash)
            updated.outputs[i].status = status
            updated.outputs[i].detail = detail
            results.append(UndoResult(path: out.path, status: status, detail: detail))
        }
        if updated.outputs.contains(where: { $0.status == .trashed }) { updated.undoneAt = now }
        return (updated, results)
    }

    private static func attempt(_ out: RunOutput, fm: FileManager, trash: Trasher) -> (UndoStatus, String?) {
        guard LaunchRequest.isValidPath(out.path), URL(fileURLWithPath: out.path).standardizedFileURL.path == out.path else { return (.refused, nil) }
        guard (try? fm.attributesOfItem(atPath: out.path)) != nil else { return (.missing, nil) }
        guard let now = OutputFingerprint.of(out.path, fm: fm) else { return (.refused, nil) } // symlink, device, unreadable
        guard now.matches(out.fingerprint) else { return (.modified, nil) }
        do {
            try trash(URL(fileURLWithPath: out.path))
            return (.trashed, nil)
        } catch {
            return (.failed, error.localizedDescription)
        }
    }
}

// MARK: storage

/// The last few runs, newest first, in `run-history.json` next to settings.json.
public final class RunHistoryStore {
    public static let maxRuns = 20
    public let url: URL

    public static var defaultURL: URL { SettingsStore.defaultDirectory.appendingPathComponent("run-history.json") }

    public init(url: URL = RunHistoryStore.defaultURL) { self.url = url }

    private struct File: Codable { var version = 1; var runs: [RunRecord] }

    /// Newest first. A missing, oversized or damaged file reads as an empty history.
    /// The file is editable by anyone with the user's rights, so what Undo may later trash is re-checked here with
    /// the recording rules (absolute, not shallow, not the home folder or above, not an input of the run, not older
    /// than the run). An output that is merely gone stays: undo reports it as missing.
    public func load(home: String = FileManager.default.homeDirectoryForCurrentUser.path, fm: FileManager = .default) -> [RunRecord] {
        guard let data = try? Data(contentsOf: url), data.count < 32 << 20,
              let file = try? JSONDecoder().decode(File.self, from: data) else { return [] }
        return Self.capped(file.runs.map { Self.revalidated($0, home: home, fm: fm) })
    }

    static func revalidated(_ record: RunRecord, home: String, fm: FileManager) -> RunRecord {
        var r = record
        let inputSet = Set((record.inputs ?? []).map { URL(fileURLWithPath: $0).standardizedFileURL.path })
        r.outputs = record.outputs.filter { out in
            guard URL(fileURLWithPath: out.path).standardizedFileURL.path == out.path else { return false }
            let why = RunRecorder.rejection(of: out.path, inputs: inputSet, startedAt: record.startedAt, home: home, fm: fm)
            return why == nil || why == .unreadable
        }
        r.untracked += record.outputs.count - r.outputs.count
        return r
    }

    public static func capped(_ runs: [RunRecord], limit: Int = maxRuns) -> [RunRecord] {
        Array(runs.sorted { $0.finishedAt > $1.finishedAt }.prefix(max(0, limit)))
    }

    /// Adds (or replaces, by id) a record, keeps the newest `limit`, writes the file, and returns the new list.
    @discardableResult
    public func upsert(_ record: RunRecord, limit: Int = RunHistoryStore.maxRuns) -> [RunRecord] {
        var runs = load().filter { $0.id != record.id }
        runs.append(record)
        let kept = Self.capped(runs, limit: limit)
        try? write(kept)
        return kept
    }

    private func write(_ runs: [RunRecord]) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        let data = try JSONEncoder().encode(File(runs: runs))
        try data.write(to: url, options: .atomic)
        try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
    }
}

// MARK: notification text

/// What a deleting run (Remove Duplicate, Remove Empty) did, for its history record.
public enum DeletionCounts {
    /// Warnings with which the engine reports an item it left alone.
    public static let leftAloneKeys: Set<String> = ["msg.duplicateChanged", "msg.emptyRemoveFailed", "msg.emptySkipped"]

    /// Whether `task` is a deleting task: its runs are recorded even when they created no files.
    public static func isDeletingTask(_ task: String) -> Bool {
        let name = task.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false).first.map(String.init) ?? task
        return name == "folder.removeDuplicate" || name == "folder.removeEmpty"
    }

    /// Items removed (files, or planned folders with everything in them), the same in both modes: in Trash mode those
    /// the app moved; in permanent mode the engine's `removal` count (`items`), or, from an engine that does not send
    /// it, the count its summary gives.
    public static func removed(summaryKey: String?, args: [String], trash: TrashGuard.Tally, items: Int? = nil) -> Int {
        if trash.requested > 0 { return trash.moved }
        if let n = items { return n }
        let permanent: Set<String> = ["msg.deletedPermanently", "msg.deletedPermanentlyPartial", "msg.emptyDeleted",
                                      "msg.emptyDeletedPartial", "msg.cancelledAfterDeleting"]
        guard let k = summaryKey, permanent.contains(k) else { return 0 }
        return Int(args.first ?? "") ?? 0
    }
}

/// The outcome a finished run is recorded and announced with.
public enum RecordedOutcome {
    /// `partial`: the engine succeeded, but the app could not carry out all of it (items it was asked to move to the
    /// Trash were refused or could not be moved). Everything else is the engine's outcome as it is.
    public static func string(_ outcome: String, problems: Bool) -> String {
        outcome == "succeeded" && problems ? "partial" : outcome
    }
}

public enum NotificationText {
    /// Localized title and body for the "run finished" notification.
    public static func make(outcome: String, summary: String?, outputCount: Int, loc: Loc) -> (title: String, body: String) {
        let title: String
        switch outcome {
        case "succeeded": title = loc.t("ux.notify.succeeded")
        case "partial": title = loc.t("ux.notify.failed") // finished with problems
        case "cancelled": title = loc.t("ux.notify.cancelled")
        default: title = loc.t("ux.notify.failed")
        }
        var parts: [String] = []
        if let s = summary, !s.isEmpty { parts.append(s) }
        if outputCount > 0 { parts.append(loc.n("ux.notify.created", outputCount)) }
        if parts.isEmpty { parts.append(outcome == "succeeded" ? loc.t("progress.done") : outcome == "cancelled" ? loc.t("progress.cancelled") : loc.t("progress.failed")) }
        return (title, parts.joined(separator: " "))
    }
}
