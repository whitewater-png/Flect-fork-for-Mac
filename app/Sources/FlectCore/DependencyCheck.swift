import Foundation

/// A tool Flect calls but does not bundle (see NOTICES.md), and whether it is installed.
public struct DependencyStatus: Identifiable, Equatable {
    public let id: String
    public let name: String
    public let ok: Bool
    public let required: Bool
    /// Loc key describing what needs it.
    public let neededForKey: String
    /// What to run to install it (copyable).
    public let command: String
}

public enum DependencyCheck {
    /// The only places Flect looks for ffmpeg and 7zz (same as the engine).
    public static let toolDirs = ["/opt/homebrew/bin", "/usr/local/bin"]

    public static func find(_ name: String, dirs: [String] = toolDirs, fm: FileManager = .default) -> String? {
        for d in dirs {
            let p = (d as NSString).appendingPathComponent(name)
            if fm.isExecutableFile(atPath: p) { return p }
        }
        return nil
    }

    /// `ffmpeg -filters` lists the filter (Homebrew's `ffmpeg-full` has `whisper`). Runs the tool with no shell.
    public static func ffmpegHasFilter(_ ffmpeg: String, _ filter: String) -> Bool {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: ffmpeg)
        p.arguments = ["-hide_banner", "-filters"]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        p.standardInput = FileHandle.nullDevice
        guard (try? p.run()) != nil else { return false }
        let data = out.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        return hasFilter(in: String(decoding: data, as: UTF8.self), filter)
    }

    /// Pure part of the check: is the second column of some line the filter name?
    public static func hasFilter(in listing: String, _ filter: String) -> Bool {
        listing.split(separator: "\n").contains { $0.split(separator: " ", omittingEmptySubsequences: true).dropFirst().first.map(String.init) == filter }
    }

    public static func javaPresent(fm: FileManager = .default) -> Bool {
        if ["/opt/homebrew/opt/openjdk", "/usr/local/opt/openjdk"].contains(where: { fm.fileExists(atPath: $0) }) { return true }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/libexec/java_home")
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        guard (try? p.run()) != nil else { return false }
        p.waitUntilExit()
        return p.terminationStatus == 0
    }

    public static func modelFile(for setting: String) -> String {
        switch setting {
        case "Tiny": return "ggml-tiny.bin"
        case "Base": return "ggml-base.bin"
        case "Medium": return "ggml-medium.bin"
        case "LargeV3": return "ggml-large-v3.bin"
        case "Turbo": return "ggml-large-v3-turbo.bin"
        default: return "ggml-small.bin"
        }
    }

    public static func modelArgument(for setting: String) -> String {
        switch setting {
        case "Tiny": return "tiny"
        case "Base": return "base"
        case "Medium": return "medium"
        case "LargeV3": return "large-v3"
        case "Turbo": return "turbo"
        default: return "small"
        }
    }

    /// `scripts` is the folder holding the fetch scripts (inside the app bundle, or the repository's `scripts`).
    public static func check(dataDir: URL, modelSetting: String, scripts: String, dirs: [String] = toolDirs, fm: FileManager = .default,
                             hasWhisper: (String) -> Bool = { ffmpegHasFilter($0, "whisper") },
                             hasJava: () -> Bool = { javaPresent() }) -> [DependencyStatus] {
        let ffmpeg = find("ffmpeg", dirs: dirs, fm: fm)
        let whisperOK = ffmpeg.map(hasWhisper) ?? false
        let model = modelFile(for: modelSetting)
        let q = { (s: String) in "\"" + s.replacingOccurrences(of: "\"", with: "") + "\"" }
        return [
            DependencyStatus(id: "ffmpeg", name: "ffmpeg / ffprobe", ok: ffmpeg != nil && find("ffprobe", dirs: dirs, fm: fm) != nil, required: true,
                             neededForKey: "deps.for.video", command: "brew install ffmpeg-full"),
            DependencyStatus(id: "whisper", name: "ffmpeg (whisper filter)", ok: whisperOK, required: false,
                             neededForKey: "deps.for.subtitles", command: "brew install ffmpeg-full"),
            DependencyStatus(id: "model", name: model, ok: fm.fileExists(atPath: dataDir.appendingPathComponent("deps/whisper/\(model)").path), required: false,
                             neededForKey: "deps.for.subtitles", command: q(scripts + "/fetch-whisper-model.sh") + " " + modelArgument(for: modelSetting)),
            DependencyStatus(id: "7zz", name: "7-Zip (7zz)", ok: find("7zz", dirs: dirs, fm: fm) != nil, required: true,
                             neededForKey: "deps.for.archive", command: "brew install sevenzip"),
            DependencyStatus(id: "java", name: "Java", ok: hasJava(), required: false,
                             neededForKey: "deps.for.jar", command: "brew install openjdk"),
            DependencyStatus(id: "cfr", name: "CFR", ok: fm.fileExists(atPath: dataDir.appendingPathComponent("deps/cfr.jar").path), required: false,
                             neededForKey: "deps.for.jar", command: q(scripts + "/fetch-cfr.sh")),
        ]
    }
}
