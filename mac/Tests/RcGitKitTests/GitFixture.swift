import Foundation

/// Mirrors `rc-git-testkit`: the same standard repository, built with the git CLI in a hermetic
/// environment. Keep the two in step; `docs/testing.md` lists the layout.
struct GitFixture {
    let path: String

    static func emptyDirectory() throws -> GitFixture {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("rc-git-swift-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return GitFixture(path: url.path)
    }

    static func standard() throws -> GitFixture {
        let fixture = try emptyDirectory()
        try fixture.git(["init", "-q", "-b", "main"])
        try fixture.write("README.md", "# fixture\n\nhello\n")
        try fixture.write("src/main.rs", "fn main() {\n    println!(\"hi\");\n}\n")
        try fixture.writeBytes(
            "bin.dat", [0x89, 0x50, 0x4e, 0x47, 0, 0, 0x62, 0x69, 0x6e, 0x61, 0x72, 0x79])
        try fixture.commit("Initial import", date: "2024-01-01T00:00:00+0000")
        try fixture.git(["tag", "v0.9"])
        try fixture.git(["checkout", "-q", "-b", "feature"])
        try fixture.write("feature.txt", "feature\n")
        try fixture.commit("Feature work", date: "2024-01-02T00:00:00+0000")
        try fixture.git(["checkout", "-q", "main"])
        try fixture.write("docs/guide.md", "guide\n")
        try fixture.write("src/lib.rs", "pub fn one() -> u32 {\n    1\n}\n")
        try fixture.write("alpha.txt", "alpha\n")
        try fixture.write("Zeta.txt", "zeta\n")
        try FileManager.default.createSymbolicLink(
            atPath: fixture.path + "/link", withDestinationPath: "README.md")
        try fixture.commit("Add docs and library", date: "2024-01-03T00:00:00+0000")
        try fixture.git(["tag", "-a", "v1.0", "-m", "Release 1.0"])
        try fixture.git(["update-ref", "refs/remotes/origin/main", "HEAD"])
        return fixture
    }

    func remove() {
        try? FileManager.default.removeItem(atPath: path)
    }

    func revParse(_ spec: String) -> String {
        (try? gitOutput(["rev-parse", spec])) ?? ""
    }

    func write(_ relative: String, _ contents: String) throws {
        try writeBytes(relative, Array(contents.utf8))
    }

    func writeBytes(_ relative: String, _ bytes: [UInt8]) throws {
        let url = URL(fileURLWithPath: path).appendingPathComponent(relative)
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data(bytes).write(to: url)
    }

    func commit(_ title: String, date: String) throws {
        try git(["add", "-A"])
        try git(
            ["commit", "-q", "-m", title, "--date", date],
            extraEnvironment: ["GIT_COMMITTER_DATE": date])
    }

    func git(_ args: [String], extraEnvironment: [String: String] = [:]) throws {
        _ = try gitOutput(args, extraEnvironment: extraEnvironment)
    }

    @discardableResult
    func gitOutput(_ args: [String], extraEnvironment: [String: String] = [:]) throws -> String {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        process.arguments = ["git", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"] + args
        process.currentDirectoryURL = URL(fileURLWithPath: path)
        var environment = [
            "PATH": ProcessInfo.processInfo.environment["PATH"] ?? "/usr/bin:/bin",
            "HOME": path,
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_AUTHOR_NAME": "Test Author",
            "GIT_AUTHOR_EMAIL": "author@example.com",
            "GIT_COMMITTER_NAME": "Test Committer",
            "GIT_COMMITTER_EMAIL": "committer@example.com",
        ]
        environment.merge(extraEnvironment) { _, new in new }
        process.environment = environment
        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr
        try process.run()
        let output = stdout.fileHandleForReading.readDataToEndOfFile()
        let errors = stderr.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            throw FixtureError(
                "git \(args.joined(separator: " ")) failed: \(String(decoding: errors, as: UTF8.self))"
            )
        }
        return String(decoding: output, as: UTF8.self).trimmingCharacters(
            in: .whitespacesAndNewlines)
    }
}

struct FixtureError: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}
