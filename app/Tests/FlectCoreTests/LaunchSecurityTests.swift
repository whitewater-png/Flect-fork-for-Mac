import XCTest
@testable import FlectCore

extension XCTestCase {
    /// A selection file where and how a Quick Action's `mktemp -t flect-sel` makes one.
    func makeSelectionFile(_ data: Data) throws -> URL {
        let letters = Array("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789")
        let name = LaunchRequest.inputPrefix + String((0..<10).map { _ in letters.randomElement()! })
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(name)
        guard FileManager.default.createFile(atPath: url.path, contents: data, attributes: [.posixPermissions: 0o600]) else { throw CocoaError(.fileWriteUnknown) }
        return url
    }

    func makeSelectionFile(_ text: String) throws -> URL { try makeSelectionFile(Data(text.utf8)) }
}

/// Regression tests for the launch path (`--task <id> --input <file>`): nothing but a Quick Action's own selection file
/// is read or deleted.
final class LaunchSecurityTests: XCTestCase {
    private func scratch() -> URL {
        let d = FileManager.default.temporaryDirectory.appendingPathComponent("flect-launch-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    private func parse(_ input: String) -> Result<LaunchRequest, LaunchRequest.Failure> {
        LaunchRequest.parse(arguments: ["--task", "image.strip", "--input", input])
    }

    func testInputFolderIsNeverDeleted() throws {
        // `open -a Flect-for-Mac --args --task x --input ~/Documents` used to remove the folder recursively.
        let dir = scratch()
        let inside = dir.appendingPathComponent("keep.txt")
        try "/a\n".write(to: inside, atomically: true, encoding: .utf8)
        XCTAssertEqual(parse(dir.path), .failure(.untrustedInput))
        XCTAssertTrue(FileManager.default.fileExists(atPath: inside.path))

        // Even a folder with the selection file's name in the temp folder.
        let named = FileManager.default.temporaryDirectory.appendingPathComponent("flect-sel.Folder0001")
        try FileManager.default.createDirectory(at: named, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: named) }
        XCTAssertEqual(parse(named.path), .failure(.untrustedInput))
        var isDir: ObjCBool = false
        XCTAssertTrue(FileManager.default.fileExists(atPath: named.path, isDirectory: &isDir) && isDir.boolValue)
    }

    func testFilesOutsideTheTempFolderAreNeitherReadNorDeleted() throws {
        let dir = scratch()
        // right name, wrong folder (a subfolder of the temp folder is not the temp folder)
        let elsewhere = dir.appendingPathComponent("flect-sel.Abc123")
        try "/a\n".write(to: elsewhere, atomically: true, encoding: .utf8)
        XCTAssertEqual(parse(elsewhere.path), .failure(.untrustedInput))
        XCTAssertTrue(FileManager.default.fileExists(atPath: elsewhere.path))
        // right folder, wrong name
        let other = FileManager.default.temporaryDirectory.appendingPathComponent("important-\(UUID().uuidString).txt")
        try "/a\n".write(to: other, atomically: true, encoding: .utf8)
        defer { unlink(other.path) }
        XCTAssertEqual(parse(other.path), .failure(.untrustedInput))
        XCTAssertTrue(FileManager.default.fileExists(atPath: other.path))
        // `..` cannot walk out of the temp folder and back in under another name
        XCTAssertFalse(LaunchRequest.isInTempDirectory(dir.path + "/../flect-sel.x/../../etc/passwd", tempDirectories: LaunchRequest.userTempDirectories))
        XCTAssertEqual(parse("relative/flect-sel.Abc"), .failure(.untrustedInput))
    }

    func testSymlinkIsNeitherFollowedNorRemoved() throws {
        let dir = scratch()
        let target = dir.appendingPathComponent("target.txt")
        try "/a\n".write(to: target, atomically: true, encoding: .utf8)
        let link = FileManager.default.temporaryDirectory.appendingPathComponent("flect-sel.Link\(Int.random(in: 1000...9999))")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: target)
        defer { unlink(link.path) }
        XCTAssertEqual(parse(link.path), .failure(.untrustedInput))
        var st = stat()
        XCTAssertEqual(lstat(link.path, &st), 0, "the link itself is left alone")
        XCTAssertTrue(FileManager.default.fileExists(atPath: target.path), "the target is left alone")
        // a link to a folder: the old code removed the folder's contents through it
        let dirLink = FileManager.default.temporaryDirectory.appendingPathComponent("flect-sel.DirLink\(Int.random(in: 1000...9999))")
        try FileManager.default.createSymbolicLink(at: dirLink, withDestinationURL: dir)
        defer { unlink(dirLink.path) }
        XCTAssertEqual(parse(dirLink.path), .failure(.untrustedInput))
        XCTAssertTrue(FileManager.default.fileExists(atPath: target.path))
    }

    func testHardLinkFifoAndOversizedFileAreRefused() throws {
        let dir = scratch()
        let original = dir.appendingPathComponent("original.txt")
        try "/a\n".write(to: original, atomically: true, encoding: .utf8)
        let hard = FileManager.default.temporaryDirectory.appendingPathComponent("flect-sel.Hard\(Int.random(in: 1000...9999))")
        XCTAssertEqual(link(original.path, hard.path), 0)
        defer { unlink(hard.path) }
        XCTAssertEqual(parse(hard.path), .failure(.untrustedInput))
        XCTAssertTrue(FileManager.default.fileExists(atPath: hard.path) && FileManager.default.fileExists(atPath: original.path))

        let fifo = FileManager.default.temporaryDirectory.appendingPathComponent("flect-sel.Fifo\(Int.random(in: 1000...9999))")
        XCTAssertEqual(mkfifo(fifo.path, 0o600), 0)
        defer { unlink(fifo.path) }
        XCTAssertEqual(parse(fifo.path), .failure(.untrustedInput), "refused without opening it (would block)")
        XCTAssertTrue(FileManager.default.fileExists(atPath: fifo.path))

        let big = try makeSelectionFile(Data())
        defer { unlink(big.path) }
        XCTAssertEqual(truncate(big.path, off_t(LaunchRequest.maxInputBytes)), 0)
        XCTAssertEqual(parse(big.path), .failure(.untrustedInput))
        XCTAssertTrue(FileManager.default.fileExists(atPath: big.path), "an oversized file is not read and not deleted")
    }

    func testOwnerAndTypeRules() {
        var st = stat()
        st.st_mode = S_IFREG | 0o600
        st.st_uid = getuid()
        st.st_nlink = 1
        st.st_size = 10
        XCTAssertTrue(LaunchRequest.isTrustedStat(st))
        var other = st; other.st_uid = getuid() &+ 1
        XCTAssertFalse(LaunchRequest.isTrustedStat(other), "a file owned by someone else")
        var dir = st; dir.st_mode = S_IFDIR | 0o700
        XCTAssertFalse(LaunchRequest.isTrustedStat(dir))
        var lnk = st; lnk.st_mode = S_IFLNK | 0o777
        XCTAssertFalse(LaunchRequest.isTrustedStat(lnk))
        var big = st; big.st_size = off_t(LaunchRequest.maxInputBytes)
        XCTAssertFalse(LaunchRequest.isTrustedStat(big))
    }

    func testSelectionFileNamePattern() {
        XCTAssertTrue(LaunchRequest.isSelectionFileName("flect-sel.ETuoaKy26Y"))
        for bad in ["flect-sel.", "flect-sel", "flect-sel.a/b", "flect-sel.a b", "flect-sel.a.b", "xflect-sel.abc", "flect-sel." + String(repeating: "a", count: 33)] {
            XCTAssertFalse(LaunchRequest.isSelectionFileName(bad), bad)
        }
    }

    func testRealSelectionFileIsReadThenUnlinked() throws {
        let file = try makeSelectionFile("/Users/a/x.png\n")
        XCTAssertTrue(LaunchRequest.isSelectionFile(file.path))
        // the temp folder may be given as /var/... or /private/var/...: both resolve to the same folder
        let resolved = LaunchRequest.realPath(FileManager.default.temporaryDirectory.path)!
        XCTAssertTrue(LaunchRequest.isSelectionFile(file.path, tempDirectories: [resolved]))
        XCTAssertEqual(try LaunchRequest.parse(arguments: ["--task", "image.strip", "--input", file.path], deleteInput: false).get().paths, ["/Users/a/x.png"])
        XCTAssertTrue(FileManager.default.fileExists(atPath: file.path), "deleteInput: false keeps it")
        XCTAssertEqual(try parse(file.path).get().paths, ["/Users/a/x.png"])
        XCTAssertFalse(FileManager.default.fileExists(atPath: file.path))
        // not in any of the given temp folders
        let again = try makeSelectionFile("/a\n")
        defer { unlink(again.path) }
        XCTAssertEqual(LaunchRequest.parse(arguments: ["--task", "x", "--input", again.path], tempDirectories: ["/nonexistent"]), .failure(.untrustedInput))
        XCTAssertTrue(FileManager.default.fileExists(atPath: again.path))
    }
}
