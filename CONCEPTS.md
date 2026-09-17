# Concepts

Vocabulary with a precise meaning in rc-git. Definitions stand on their own; the code
implements them, not the other way around.

## Guard

The single decision-and-write path for every mutation rc-git performs. Nothing else in
the project may change a repository or talk to the network; a feature that needs either
asks the guard's policy first and runs the action inside it.

The policy's answer is allow, confirm (with a challenge), or deny — and a deny is final
inside rc-git: the message points the user at git itself rather than offering a way
through.

## Challenge

What the user must do before a risky action runs: at minimum press a confirm button,
and for destructive actions type an exact phrase (usually the name of the thing being
destroyed). Verifying a challenge is the only way to obtain the proof the write path
demands, so an unconfirmed destructive action cannot be expressed.

## Backup ref

A ref recorded under rc-git's own namespace before a destructive action runs, pointing
at whatever is about to be deleted, so the previous state can be restored by hand.
Backups are numbered rather than overwritten, so repeating an action never destroys an
earlier backup.

## Isolated open

How every repository is opened: only the repository's own configuration is read — no
global or system git config, no environment variables, no include directives. A
repository someone else produced therefore cannot point rc-git at files outside itself.
A side effect: anything the repository's own config does not provide (an identity for
reflog lines, credentials for a remote) must be supplied by rc-git itself per
operation.

## Remote operations

The guard's network actions: fetch, fast-forward pull, plain push, and their sync
composition. They speak HTTPS only — the one git transport that needs no child
process — and are never force: a push the remote would have to discard history to
accept is refused before anything is sent. Credentials are per-host, supplied by the
caller per call, and never stored by the core.
