import Foundation
import Testing

@testable import RcGitKit

@Suite struct ShellWordTests {
    /// Both shells a quoted word reaches: `sh` for scripts, `zsh` for the updater and Terminal.
    /// `zsh -f` skips startup files, so the user's own config cannot change the result.
    private static let shells: [[String]] = [["/bin/sh"], ["/bin/zsh", "-f"]]

    @Test(arguments: shells)
    func aWordWithNothingToExpandStaysBare(_ shell: [String]) throws {
        let word = "/Users/me/rc-git_2.0/a+b@c"
        #expect(ShellWord.quote(word) == word)
        #expect(try Self.shellPrints(word, in: shell) == word)
    }

    @Test(arguments: shells)
    func anEmptyWordIsStillAWord(_ shell: [String]) throws {
        #expect(ShellWord.quote("") == "''")
        #expect(try Self.shellPrints(ShellWord.quote(""), in: shell) == "")
    }

    /// The shell must read back exactly what it was given, whatever the text holds.
    @Test(
        arguments: shells,
        [
            "/Users/me/My Repos/app",
            "/Users/me/it's",
            "/tmp/a\"b",
            "/tmp/$HOME",
            "/tmp/`id`",
            "/tmp/a;b && c",
            "/tmp/a\\b",
            "/tmp/*",
            "/tmp/line\nbreak",
            "/tmp/'''",
            "/Users/me/Dự án",
            "/Users/me/rc-git_2.0/a+b@c",
            "=ls",
            "/tmp/a==ls",
            "~/x",
            "/tmp/a?",
            "/tmp/[ab]",
            "/tmp/x{a,b}",
            "#x",
            "!x",
        ])
    func theShellReadsItBack(_ shell: [String], _ text: String) throws {
        #expect(try Self.shellPrints(ShellWord.quote(text), in: shell) == text)
    }

    /// What `shell` prints for `printf %s <word>`: the word after quote removal and expansion.
    /// Fails with the shell's own message when it exits non-zero.
    private static func shellPrints(_ word: String, in shell: [String]) throws -> String {
        let executable = try #require(shell.first)
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = Array(shell.dropFirst()) + ["-c", "printf %s \(word)"]
        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr
        try process.run()
        let output = stdout.fileHandleForReading.readDataToEndOfFile()
        let errData = stderr.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        try #require(
            process.terminationStatus == 0,
            "\(executable) rejected \(word): \(String(decoding: errData, as: UTF8.self))")
        return String(decoding: output, as: UTF8.self)
    }
}
