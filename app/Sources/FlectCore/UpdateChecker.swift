import Foundation

/// A release version such as `0.1.1` (a leading `v` and a `-suffix` are accepted; the suffix marks a pre-release).
public struct ReleaseVersion: Comparable, Equatable, CustomStringConvertible {
    public let major: Int, minor: Int, patch: Int
    public let preRelease: Bool

    public init?(_ text: String) {
        var s = text.trimmingCharacters(in: .whitespaces)
        if s.hasPrefix("v") || s.hasPrefix("V") { s.removeFirst() }
        let suffixStart = s.firstIndex(of: "-")
        let core = suffixStart.map { String(s[..<$0]) } ?? s
        if let i = suffixStart {
            let suffix = s[s.index(after: i)...]
            guard !suffix.isEmpty, suffix.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "." || $0 == "-") }) else { return nil }
        }
        let parts = core.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.count == 3 else { return nil }
        let numbers = parts.map { p -> Int? in p.isEmpty || p.count > 6 || !p.allSatisfy({ $0.isASCII && $0.isNumber }) ? nil : Int(p) }
        guard let a = numbers[0], let b = numbers[1], let c = numbers[2] else { return nil }
        major = a; minor = b; patch = c
        preRelease = suffixStart != nil
    }

    public static func < (l: ReleaseVersion, r: ReleaseVersion) -> Bool {
        if (l.major, l.minor, l.patch) != (r.major, r.minor, r.patch) { return (l.major, l.minor, l.patch) < (r.major, r.minor, r.patch) }
        return l.preRelease && !r.preRelease   // 0.1.1-dev is older than 0.1.1
    }

    public var description: String { "\(major).\(minor).\(patch)" }
}

public struct ReleaseInfo: Equatable {
    public let tag: String
    public let version: ReleaseVersion
    public let url: URL
}

public enum UpdateResult: Equatable {
    case upToDate
    case available(ReleaseInfo)
    case failed
}

/// "Is there a newer release?" One HTTPS GET to GitHub's public API; nothing is sent about the user or the Mac.
/// It only ever runs when the user asks for it (or has switched on the check at launch) and never installs anything.
public enum UpdateChecker {
    public static let repository = "whitewater-png/Flect-fork-for-Mac"
    public static let latestReleaseURL = URL(string: "https://api.github.com/repos/\(repository)/releases/latest")!
    /// UserDefaults key (app preference, not part of settings.json): check once a day when the settings window opens.
    public static let autoCheckKey = "CheckForUpdatesAtLaunch"
    public static let lastCheckKey = "LastUpdateCheck"
    /// The newest release tag a check found (shown as a banner until the app is updated; no network needed).
    public static let knownLatestKey = "KnownLatestVersion"
    static let maxResponseBytes = 262_144

    /// Parses GitHub's `releases/latest` JSON. Only a plain `vX.Y.Z` tag of this repository's own release page is accepted,
    /// so a changed or forged response can neither show odd text nor send the browser somewhere else.
    public static func parse(_ data: Data) -> ReleaseInfo? {
        guard data.count <= maxResponseBytes,
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let tag = obj["tag_name"] as? String, let urlText = obj["html_url"] as? String,
              (obj["draft"] as? Bool) != true, (obj["prerelease"] as? Bool) != true,
              let version = ReleaseVersion(tag), !version.preRelease,
              tag.range(of: #"^v?\d{1,6}\.\d{1,6}\.\d{1,6}$"#, options: .regularExpression) != nil,
              let url = URL(string: urlText), url.scheme == "https", url.host == "github.com",
              url.path.hasPrefix("/\(repository)/releases/") else { return nil }
        return ReleaseInfo(tag: tag, version: version, url: url)
    }

    /// The release page for a tag that already passed `parse` (so it is a plain vX.Y.Z of this repository).
    public static func releaseInfo(forKnownTag tag: String) -> ReleaseInfo? {
        guard tag.range(of: #"^v?\d{1,6}\.\d{1,6}\.\d{1,6}$"#, options: .regularExpression) != nil,
              let version = ReleaseVersion(tag), let url = URL(string: "https://github.com/\(repository)/releases/tag/\(tag)") else { return nil }
        return ReleaseInfo(tag: tag, version: version, url: url)
    }

    public static func decide(current: String, latest: ReleaseInfo) -> UpdateResult {
        guard let cur = ReleaseVersion(current) else { return .available(latest) }   // a development build: offer the release
        return latest.version > cur ? .available(latest) : .upToDate
    }

    /// Once a day at most, and only when the user has switched the check on.
    public static func shouldAutoCheck(enabled: Bool, last: Date?, now: Date = Date()) -> Bool {
        guard enabled else { return false }
        guard let last else { return true }
        return now.timeIntervalSince(last) >= 24 * 3600
    }

    public static func request(appVersion: String) -> URLRequest {
        var r = URLRequest(url: latestReleaseURL, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 10)
        r.httpShouldHandleCookies = false
        r.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        r.setValue("Flect-for-Mac/\(appVersion)", forHTTPHeaderField: "User-Agent")   // GitHub asks API clients to send one
        return r
    }

    public static func check(current: String, session: URLSession? = nil) async -> UpdateResult {
        let config = URLSessionConfiguration.ephemeral
        config.httpCookieStorage = nil
        config.urlCredentialStorage = nil
        config.timeoutIntervalForRequest = 10
        let s = session ?? URLSession(configuration: config)
        do {
            let (data, response) = try await s.data(for: request(appVersion: current))
            guard (response as? HTTPURLResponse)?.statusCode == 200, let info = parse(data) else { return .failed }
            return decide(current: current, latest: info)
        } catch {
            return .failed
        }
    }
}
