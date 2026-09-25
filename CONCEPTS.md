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

## Highlight span

One colored region of a file: a half-open byte range plus a class naming what the region
means. A file's spans are ordered by start and never overlap — nested captures flatten to
the innermost one — because the view applies exactly one color per range. A producer that
combines several grammar layers, as markdown does for its inline prose and fenced code,
must re-impose this contract before the spans leave the highlighter.

## Isolated open

How every repository is opened: only the repository's own configuration is read — no
global or system git config, no environment variables, no include directives. A
repository someone else produced therefore cannot point rc-git at files outside itself.
A side effect: anything the repository's own config does not provide (an identity for
reflog lines, credentials for a remote) must be supplied by rc-git itself per
operation.

## Remote operations

The guard's network actions: fetch, fast-forward pull, plain push, and their sync
composition. They speak HTTPS and SSH, both in-process so no child process is ever
spawned, and are never force: a push the remote would have to discard history to
accept is refused before anything is sent. Credentials are per-host, supplied by the
caller per call, and never stored by the core.

Every one of them ends. The app carries a cancel flag into the call that the user can
raise from the other side while the transfer holds the thread, and each transport keeps
an idle budget — the gap it tolerates between bytes, not the length of the transfer, so
a slow link is never punished and a dead one does not wait forever. A cancelled
operation stops between steps and leaves nothing half-applied.

## Host-key trust

How an SSH server proves its identity to rc-git. The user's `known_hosts` is consulted
read-only and never written; a host it does not know is refused until the user approves
the offered key's fingerprint, and that approval is remembered by the app itself, per
host.

A key that contradicts `known_hosts` is refused with no way to approve it inside
rc-git — like a guard deny, the message points the user at ssh itself. Lookup is keyed
by the name actually dialed, so a config alias shares the trust of the real host behind
it.

## Tick

What the user includes in the next commit or stash. Every changed file starts ticked, and an
untick survives a rescan by path rather than by position. A tick is whole-file by default; a
file whose hunks are ticked individually carries only those hunks, and ticking every hunk
collapses back to a whole-file tick while unticking the last one unticks the file. A row that
stands for several files — a folder in the change list — is ticked when all of them are, mixed
when only some are, and one click moves the whole set.

## Preflight

The checks a self-update runs before it touches anything: the login shell must find the
build's tools, and the checkout must be readable — plus, for a release update, free of
uncommitted tracked changes. Both halves run outside the process, so both are bounded and
both are logged before they run: a preflight that cannot answer fails with a reason rather
than leaving the window spinning on one line.

## Session handover

What a self-update passes to the build that replaces it: the open tabs, in on-screen
order, with the selected one. It is a handover rather than a save because both processes
are alive at once — the new build reads the state while the old one is still running, so
the write is flushed before the launch and the old process stops writing afterwards. The
state is scoped by bundle identifier, which is why the identifier never depends on a
version or a build number.
