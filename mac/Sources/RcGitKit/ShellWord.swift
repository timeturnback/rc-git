import Foundation

/// Turns a string into one word that `sh` and `zsh` both read back unchanged.
public enum ShellWord {
    /// Characters neither `sh` nor `zsh` expands: Python's `shlex.quote` bare set minus `=`,
    /// which zsh expands (`=ls` becomes `/bin/ls`, and `a==ls` too under `magicequalsubst`).
    private static let bareCharacters = CharacterSet(
        charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789@%+:,./-_")

    /// `text` bare when nothing in it can expand, otherwise single-quoted, with each embedded
    /// quote closed, escaped, and reopened as `'\''`.
    public static func quote(_ text: String) -> String {
        if !text.isEmpty, text.unicodeScalars.allSatisfy(bareCharacters.contains) {
            return text
        }
        return "'" + text.replacingOccurrences(of: "'", with: #"'\''"#) + "'"
    }
}
