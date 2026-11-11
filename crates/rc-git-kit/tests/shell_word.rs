//! Ported from `ShellWordTests.swift`: every quoted word goes through the real shells that
//! read it back.
#![expect(
    clippy::panic,
    reason = "the shell helper fails the test with the shell's own message"
)]

use std::process::Command;

use rc_git_kit::shell_word::quote;

/// Both shells a quoted word reaches: `sh` for scripts, `zsh` for the updater and Terminal.
/// `zsh -f` skips startup files, so the user's own config cannot change the result.
const SHELLS: [&[&str]; 2] = [&["/bin/sh"], &["/bin/zsh", "-f"]];

/// What `shell` prints for `printf %s <word>`: the word after quote removal and expansion.
/// Panics with the shell's own message when it exits non-zero.
#[expect(
    clippy::disallowed_methods,
    reason = "the round trip is only proven by the shells that read the word back"
)]
fn shell_prints(word: &str, shell: &[&str]) -> String {
    let output = Command::new(shell[0])
        .args(&shell[1..])
        .arg("-c")
        .arg(format!("printf %s {word}"))
        .env_clear()
        .output()
        .unwrap_or_else(|err| panic!("cannot run {}: {err}", shell[0]));
    assert!(
        output.status.success(),
        "{} rejected {word}: {}",
        shell[0],
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap_or_else(|err| panic!("not UTF-8: {err}"))
}

#[test]
fn a_word_with_nothing_to_expand_stays_bare() {
    let word = "/Users/me/rc-git_2.0/a+b@c";
    assert_eq!(quote(word), word);
    for shell in SHELLS {
        assert_eq!(shell_prints(word, shell), word, "{shell:?}");
    }
}

#[test]
fn an_empty_word_is_still_a_word() {
    assert_eq!(quote(""), "''");
    for shell in SHELLS {
        assert_eq!(shell_prints(&quote(""), shell), "", "{shell:?}");
    }
}

#[test]
fn the_shell_reads_it_back() {
    let texts = [
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
    ];
    for shell in SHELLS {
        for text in texts {
            assert_eq!(
                shell_prints(&quote(text), shell),
                text,
                "{shell:?} {text:?}"
            );
        }
    }
}

#[test]
fn non_ascii_and_equals_are_quoted() {
    assert_eq!(quote("Dự"), "'Dự'");
    assert_eq!(quote("a=b"), "'a=b'");
    assert_eq!(quote("it's"), r"'it'\''s'");
}
