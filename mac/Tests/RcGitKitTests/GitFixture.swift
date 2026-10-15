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

    /// A fresh copy of the standard repository. git builds it once per test process and each
    /// test copies that: a spawn costs up to a second under load, and every git-backed test
    /// building its own dozen-commit fixture at once ran past the suites' time limits.
    static func standard() async throws -> GitFixture {
        let template = try await standardTemplate.value
        let path = FileManager.default.temporaryDirectory
            .appendingPathComponent("rc-git-swift-\(UUID().uuidString)").path
        try FileManager.default.copyItem(atPath: template.path, toPath: path)
        return GitFixture(path: path)
    }

    private static let standardTemplate = Task { try await buildStandard() }

    /// Builds the history in one `git fast-import` stream and checks `main` out, three spawns
    /// in all: every test waits on this build, and one spawn per commit, tag and checkout took
    /// tens of seconds of each test's time limit on a loaded machine. Below git's
    /// `fastimport.unpackLimit` the objects land loose, as a commit-by-commit build leaves them.
    private static func buildStandard() async throws -> GitFixture {
        let fixture = try emptyDirectory()
        try await fixture.git(["init", "-q", "-b", "main"])
        let stream = fixture.path + "/.git/fixture.stream"
        try Data(standardStream()).write(to: URL(fileURLWithPath: stream))
        try await fixture.git(["fast-import", "--quiet"], input: stream)
        try FileManager.default.removeItem(atPath: stream)
        try await fixture.git(["reset", "-q", "--hard"])
        return fixture
    }

    /// The standard history as a `git fast-import` stream: what `git commit` and `git tag -a`
    /// would record with the fixture's identities and dates, messages ending in a newline.
    private static func standardStream() -> [UInt8] {
        var stream: [UInt8] = []
        func line(_ text: String) { stream += Array((text + "\n").utf8) }
        func data(_ bytes: [UInt8]) {
            line("data \(bytes.count)")
            stream += bytes
            line("")
        }
        func file(_ path: String, _ contents: String, mode: String = "100644") {
            line("M \(mode) inline \(path)")
            data(Array(contents.utf8))
        }
        func commit(_ ref: String, mark: Int, from: Int?, title: String, seconds: Int) {
            line("commit \(ref)")
            line("mark :\(mark)")
            line("author Test Author <author@example.com> \(seconds) +0000")
            line("committer Test Committer <committer@example.com> \(seconds) +0000")
            data(Array((title + "\n").utf8))
            if let from { line("from :\(from)") }
        }
        let january1 = 1_704_067_200
        let day = 86_400

        commit("refs/heads/main", mark: 1, from: nil, title: "Initial import", seconds: january1)
        file("README.md", "# fixture\n\nhello\n")
        file("src/main.rs", "fn main() {\n    println!(\"hi\");\n}\n")
        line("M 100644 inline bin.dat")
        data([0x89, 0x50, 0x4e, 0x47, 0, 0, 0x62, 0x69, 0x6e, 0x61, 0x72, 0x79])
        line("reset refs/tags/v0.9")
        line("from :1")

        commit(
            "refs/heads/feature", mark: 2, from: 1, title: "Feature work",
            seconds: january1 + day)
        file("feature.txt", "feature\n")

        commit(
            "refs/heads/main", mark: 3, from: 1, title: "Add docs and library",
            seconds: january1 + 2 * day)
        file("docs/guide.md", "guide\n")
        file("src/lib.rs", "pub fn one() -> u32 {\n    1\n}\n")
        file("alpha.txt", "alpha\n")
        file("Zeta.txt", "zeta\n")
        file("link", "README.md", mode: "120000")
        line("tag v1.0")
        line("from :3")
        line("tagger Test Committer <committer@example.com> \(january1 + 2 * day) +0000")
        data(Array("Release 1.0\n".utf8))
        line("reset refs/remotes/origin/main")
        line("from :3")
        return stream
    }

    func remove() {
        try? FileManager.default.removeItem(atPath: path)
    }

    func revParse(_ spec: String) async throws -> String {
        try await gitOutput(["rev-parse", "--verify", spec])
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

    func commit(_ title: String, date: String) async throws {
        try await git(["add", "-A"])
        try await git(
            ["commit", "-q", "-m", title, "--date", date],
            extraEnvironment: ["GIT_COMMITTER_DATE": date])
    }

    func git(
        _ args: [String], extraEnvironment: [String: String] = [:], input: String? = nil
    ) async throws {
        _ = try await gitOutput(args, extraEnvironment: extraEnvironment, input: input)
    }

    /// Runs git in the fixture, reading stdin from the file at `input` when one is given, and
    /// returns its trimmed stdout. Every wait is a continuation, so a test awaiting git holds
    /// no cooperative-pool thread.
    func gitOutput(
        _ args: [String], extraEnvironment: [String: String] = [:], input: String? = nil
    ) async throws -> String {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        process.arguments = ["git", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"] + args
        process.currentDirectoryURL = URL(fileURLWithPath: path)
        var environment = [
            "PATH": ProcessInfo.processInfo.environment["PATH"] ?? "/usr/bin:/bin",
            "HOME": path,
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_AUTHOR_NAME": "Test Author",
            "GIT_AUTHOR_EMAIL": "author@example.com",
            "GIT_COMMITTER_NAME": "Test Committer",
            "GIT_COMMITTER_EMAIL": "committer@example.com",
        ]
        environment.merge(extraEnvironment) { _, new in new }
        process.environment = environment
        if let input {
            guard let handle = FileHandle(forReadingAtPath: input) else {
                throw FixtureError("cannot open \(input) as stdin for git \(args.first ?? "")")
            }
            process.standardInput = handle
        } else {
            process.standardInput = FileHandle.nullDevice
        }
        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr
        let exited = OneShot<Bool>()
        process.terminationHandler = { _ in exited.fulfil(true) }
        let output = PipeDrain(stdout.fileHandleForReading)
        let errors = PipeDrain(stderr.fileHandleForReading)
        try process.run()
        let command = "git \(args.joined(separator: " "))"
        guard await exited.value(within: .seconds(GitFixture.timeoutSeconds), else: false) else {
            process.terminate()
            throw FixtureError("\(command) timed out after \(GitFixture.timeoutSeconds) s")
        }
        guard process.terminationStatus == 0 else {
            throw FixtureError(
                "\(command) failed: " + String(decoding: await errors.wait(), as: UTF8.self))
        }
        return String(decoding: await output.wait(), as: UTF8.self).trimmingCharacters(
            in: .whitespacesAndNewlines)
    }

    private static let timeoutSeconds = 60
}

/// A value set once, from any thread, and awaited without holding a thread. The first
/// `fulfil` wins; later ones are ignored.
final class OneShot<Value: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    // Guarded by `lock`.
    private var value: Value?
    private var waiters: [CheckedContinuation<Value, Never>] = []

    func fulfil(_ newValue: Value) {
        let resumed = lock.withLock { () -> [CheckedContinuation<Value, Never>] in
            guard value == nil else { return [] }
            value = newValue
            defer { waiters = [] }
            return waiters
        }
        for waiter in resumed { waiter.resume(returning: newValue) }
    }

    func value() async -> Value {
        await withCheckedContinuation { (continuation: CheckedContinuation<Value, Never>) in
            let ready = lock.withLock { () -> Value? in
                if let value { return value }
                waiters.append(continuation)
                return nil
            }
            if let ready { continuation.resume(returning: ready) }
        }
    }

    /// The value, or `fallback` once `limit` passes without one.
    func value(within limit: DispatchTimeInterval, else fallback: Value) async -> Value {
        DispatchQueue.global().asyncAfter(deadline: .now() + limit) {
            self.fulfil(fallback)
        }
        return await value()
    }
}

/// Reads one pipe to end of file on its own thread, so a child that fills one pipe while the
/// other is being read cannot block, and a hung child cannot outlast the timed wait. The
/// blocking read holds a dedicated `Thread`, never a cooperative-pool one.
private final class PipeDrain: Sendable {
    private let data = OneShot<Data>()

    init(_ handle: FileHandle) {
        let data = data
        Thread {
            data.fulfil(handle.readDataToEndOfFile())
        }.start()
    }

    func wait() async -> Data {
        await data.value()
    }
}

struct FixtureError: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}
