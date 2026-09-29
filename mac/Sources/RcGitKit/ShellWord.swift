import Foundation

/// Turns a string into one word a POSIX shell reads back unchanged.
public enum ShellWord {
    /// Characters a shell never expands, the same set Python's `shlex.quote` leaves bare.
    private static let bareCharacters = CharacterSet(
        charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789@%+=:,./-_")

    /// `text` bare when nothing in it can expand, otherwise single-quoted, with each embedded
    /// quote closed, escaped, and reopened as `'\''`.
    public static func quote(_ text: String) -> String {
        if !text.isEmpty, text.unicodeScalars.allSatisfy(bareCharacters.contains) {
            return text
        }
        return "'" + text.replacingOccurrences(of: "'", with: #"'\''"#) + "'"
    }
}
