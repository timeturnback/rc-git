import Foundation
import Testing

@testable import RcGitKit

@Suite struct ShellWordTests {
    @Test func aWordWithNothingToExpandStaysBare() {
        #expect(ShellWord.quote("/Users/me/rc-git_2.0/a+b@c") == "/Users/me/rc-git_2.0/a+b@c")
    }

    @Test func anEmptyWordIsStillAWord() throws {
        #expect(ShellWord.quote("") == "''")
        #expect(try Self.shellPrints(ShellWord.quote("")) == "")
    }

    /// The shell must read back exactly what it was given, whatever the text holds.
    @Test(arguments: [
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
    ])
    func theShellReadsItBack(_ text: String) throws {
        #expect(try Self.shellPrints(ShellWord.quote(text)) == text)
    }

    /// What `/bin/sh` prints for `printf %s <word>`: the word after quote removal and
    /// expansion.
    private static func shellPrints(_ word: String) throws -> String {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/sh")
        process.arguments = ["-c", "printf %s \(word)"]
        let stdout = Pipe()
        process.standardOutput = stdout
        try process.run()
        let output = stdout.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return String(decoding: output, as: UTF8.self)
    }
}
