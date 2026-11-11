//! Port of `ShellWord.swift`: turns a string into one word that `sh` and `zsh` both read
//! back unchanged.

/// Whether `ch` is one neither `sh` nor `zsh` expands: Python's `shlex.quote` bare set minus
/// `=`, which zsh expands (`=ls` becomes `/bin/ls`, and `a==ls` too under
/// `magicequalsubst`).
fn is_bare(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || "@%+:,./-_".contains(ch)
}

/// `text` bare when nothing in it can expand, otherwise single-quoted, with each embedded
/// quote closed, escaped, and reopened as `'\''`.
#[must_use]
pub fn quote(text: &str) -> String {
    if !text.is_empty() && text.chars().all(is_bare) {
        return text.to_owned();
    }
    format!("'{}'", text.replace('\'', r"'\''"))
}
