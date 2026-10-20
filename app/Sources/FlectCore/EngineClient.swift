import Foundation

public enum EngineError: Error {
    case notFound
    case launchFailed(String)
}

/// Runs `flect-engine` as a child process and speaks JSON Lines with it. One process per run, so a
/// crash or a hostile file can only ever take down that one run.
public final class EngineClient {
    public let executable: URL
    private let queue = DispatchQueue(label: "flect.engine")
    private var process: Process?
    private var stdin: FileHandle?

    public init(executable: URL) {
        self.executable = executable
        // Writing to an engine that already exited must fail with an error, not kill the app.
        signal(SIGPIPE, SIG_IGN)
    }

    /// Where the bundled engine lives: next to the app's executable or in its Resources folder.
    public static func locate(bundle: Bundle = .main) -> URL? {
        var candidates: [URL] = []
        if let r = bundle.resourceURL { candidates.append(r.appendingPathComponent("flect-engine")) }
        if let e = bundle.executableURL { candidates.append(e.deletingLastPathComponent().appendingPathComponent("flect-engine")) }
        return candidates.first { FileManager.default.isExecutableFile(atPath: $0.path) }
    }

    /// Start a run and stream its events. The stream ends after `.done` (or `.crashed`).
    public func run(task: String, paths: [String], environment: [String: String] = [:]) -> AsyncStream<EngineEvent> {
        send(request: ["op": "run", "task": task, "paths": paths], environment: environment)
    }

    public func fetchMenu() async -> JSONValue? {
        for await event in send(request: ["op": "export-menu"], environment: [:], finishOn: { if case .menu = $0 { return true }; return false }) {
            if case .menu(let m) = event { return m }
        }
        return nil
    }

    public func cancel() {
        queue.async { [weak self] in
            guard let h = self?.stdin else { return }
            // The throwing API: the legacy write(_:) raises an Objective-C exception on a closed pipe.
            try? h.write(contentsOf: Data("{\"op\":\"cancel\"}\n".utf8))
        }
    }

    private func send(request: [String: Any], environment: [String: String], finishOn: @escaping (EngineEvent) -> Bool = { if case .done = $0 { return true }; return false }) -> AsyncStream<EngineEvent> {
        AsyncStream { continuation in
            let p = Process()
            p.executableURL = executable
            var env = ProcessInfo.processInfo.environment
            for (k, v) in environment { env[k] = v }
            p.environment = env
            let inPipe = Pipe(), outPipe = Pipe()
            p.standardInput = inPipe
            p.standardOutput = outPipe
            p.standardError = FileHandle.nullDevice

            let buffer = LineBuffer()
            var finished = false
            let finish = { (event: EngineEvent?) in
                if finished { return }
                finished = true
                if let e = event { continuation.yield(e) }
                outPipe.fileHandleForReading.readabilityHandler = nil
                self.queue.async { self.stdin = nil } // a late cancel() must not touch a closed pipe
                continuation.finish()
            }
            outPipe.fileHandleForReading.readabilityHandler = { handle in
                let chunk = handle.availableData
                if chunk.isEmpty { // EOF
                    self.queue.async { finish(.crashed) }
                    return
                }
                let lines = buffer.append(chunk)
                self.queue.async {
                    for line in lines {
                        guard !finished, let event = EngineEvent.parse(line: line) else { continue }
                        continuation.yield(event)
                        if finishOn(event) { finish(nil); try? inPipe.fileHandleForWriting.close(); return }
                    }
                }
            }
            p.terminationHandler = { _ in
                // Give the reader a moment to drain, then make sure the stream ends.
                self.queue.asyncAfter(deadline: .now() + 0.3) { finish(.crashed) }
            }
            continuation.onTermination = { _ in
                if p.isRunning { p.terminate() }
            }
            do {
                try p.run()
            } catch {
                finish(.crashed)
                return
            }
            self.queue.async {
                self.process = p
                self.stdin = inPipe.fileHandleForWriting
                if let data = try? JSONSerialization.data(withJSONObject: request) {
                    try? inPipe.fileHandleForWriting.write(contentsOf: data + Data("\n".utf8))
                }
            }
        }
    }
}

/// Splits a byte stream into lines, keeping partial lines between reads.
final class LineBuffer {
    private var pending = Data()
    private let lock = NSLock()

    func append(_ chunk: Data) -> [Data] {
        lock.lock(); defer { lock.unlock() }
        pending.append(chunk)
        var lines: [Data] = []
        while let nl = pending.firstIndex(of: 0x0A) {
            let line = pending[pending.startIndex..<nl]
            if !line.isEmpty { lines.append(Data(line)) }
            pending = Data(pending[pending.index(after: nl)...])
        }
        // Never let a hostile or broken engine grow the buffer without bound.
        if pending.count > 8 << 20 { pending.removeAll() }
        return lines
    }
}
