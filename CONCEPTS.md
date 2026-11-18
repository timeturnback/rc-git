# Concepts

Vocabulary with a precise meaning in rc-git. Definitions stand on their own; the code
implements them, not the other way around.

## Guard

The single decision-and-write path for every mutation rc-git performs. Nothing else in
the project may change a repository or talk to the network; a feature that needs either
asks the guard's policy first and runs the action inside it. rc-git runs no `git` process
for repository operations; only the in-app updater runs `git`, on rc-git's own checkout.

The policy's answer is allow, confirm (with a challenge), or deny — and a deny is final
inside rc-git: the message points the user at git itself rather than offering a way
through.

## Challenge

What the user must do before a risky action runs: at minimum press a confirm button,
and for most destructive actions also type an exact phrase (usually the name of the thing
being destroyed). Some confirm-only actions are destructive too: discarding working changes
asks for the button alone, though nothing keeps the bytes it removes. In the CLI the button is
an explicit `--yes`, and a confirm-only action refuses to run without it. Verifying a
challenge is the only way to obtain the proof the write path demands, so an unconfirmed
action cannot be expressed.

## Backup ref

A ref recorded under rc-git's own namespace before a destructive action runs, pointing
at whatever is about to be deleted, so the previous state can be restored by hand.
Backups are numbered rather than overwritten, so repeating an action never destroys an
earlier backup.

## Dirty overlap

The paths an action is about to write that the working tree or the index has already
changed. Any dirty overlap refuses the whole action before a single byte is written, and
the refusal names the first such path, so uncommitted work is never overwritten and never
partially applied. Paths the action does not touch are left alone, changes and all.

An action that moves the working tree to another commit — a fast-forward pull, a branch
switch — decides this in a fixed order: plan the per-path changes, check them all for
overlap, write the files, and move the ref last. The ordering is what keeps an interruption
legible: it leaves visibly modified files on the commit the user started from, never
something that reads as lost work. A stash restore refuses on overlap the same way, by its
own route, and moves no ref.

## Highlight span

One colored region of a file: a half-open byte range plus a class naming what the region
means. A file's spans are ordered by start and never overlap — nested captures flatten to
the innermost one — because the view applies exactly one color per range. A producer that
combines several grammar layers, as markdown does for its inline prose and fenced code,
must re-impose this contract before the spans leave the highlighter.

## Import link

A region of a source file that leads to another module: the specifier of an import, a name
the import binds, or a JSX element named after one. A link carries two byte ranges of the
file and nothing else — the region to click and the specifier as written — so it never
holds a path, and what it leads to is decided when it is followed, against the tree on
screen at that moment.

Links match names by spelling. A local that shadows an import is linked like the import;
telling them apart needs scope analysis the viewer does not do.

## Block and run

The markdown preview's document model. A *run* is one stretch of inline text that shares a
single appearance — a style set, and at most one link — and concatenating a block's runs
reproduces exactly what the reader sees. A *block* is one paragraph, heading, code block,
table, or list item, carrying its runs plus the depth it sits at.

Blocks are flat, never a tree: nesting is two integers, a quote depth and a list depth, so a
renderer indents from them rather than recursing, and a whole file is one array. Parsing
produces blocks and runs and nothing else; deciding how either looks is the renderer's, and
the renderer never re-reads the source.

## Hidden file

An entry the file lists leave out until the user asks to see hidden files: version-control
metadata and operating-system clutter such as `.git` or `.DS_Store`, named in one fixed list
and matched by name at any depth, so everything inside a hidden folder is hidden with it.

Hidden is not ignored. A gitignored file like `.env` is one the user may well want to read,
so it stays visible, marked as ignored; and a hidden file is hidden whether git tracks it or
not. The one exception is a change the user made: a tracked hidden file that changed stays in
the change list, since hiding it would hide an edit. An untracked hidden file does not, so it
can never ride along in a commit the user did not see.

## Isolated open

How every repository is opened: only the repository's own configuration is read — no
global or system git config, no environment variables, no include directives. Nor can
that config aim rc-git at files outside the repository: a `core.worktree` that resolves
anywhere but the opened directory is refused at open, and the repository's own
`core.excludesFile` is never read. The one outside pointer followed is
`objects/info/alternates`, which lends objects from another repository read-only, as
`git clone --shared` relies on.
A side effect: anything the repository's own config does not provide (an identity for
reflog lines, credentials for a remote) must be supplied by rc-git itself per
operation. What only the user's own config holds — a preferred pull strategy, a commit
identity, a signing setup — is invisible to rc-git: it asks for an identity where one is
needed, writes its commits unsigned, and never picks rebase or merge for a pull. The identity
it asks for is a fallback saved for that one repository (Repository > Commit Identity… shows,
edits or clears it; the defaults key is `rc-git.commitIdentities`), and a repository whose own
config sets `user.name`/`user.email` always commits as that. The one
thing read from outside the repository is the global ignore file at git's default location
(`$XDG_CONFIG_HOME/git/ignore`, else `~/.config/git/ignore`), so a file the user ignores
globally does not show as untracked; a `core.excludesFile` set in `~/.gitconfig` is not read.

## Remote operations

The guard's network actions: fetch, fast-forward pull, plain push, and their sync
composition. They speak HTTPS and SSH, both in-process so no child process is ever
spawned, and are never force: a push the remote would have to discard history to
accept is refused before anything is sent. A pull is the mirror case: when the branch
and its upstream have both moved (diverged), it is refused with the counts on each
side, and reconciling — rebase or merge — is left to git. Credentials are per-host,
supplied by the caller per call, and never stored by the core.

A branch's upstream is the remote and branch its own config names. When the config names
none — a branch pushed without setting one — rc-git takes the same-named branch on
`origin`, and the ahead/behind counts and pull compare against it. Push picks its remote
the way git does — `branch.<name>.pushRemote`, then `remote.pushDefault`, then the
upstream's remote — and sends to that remote's `pushurl` when it has one. It uploads only to
the same-named branch, as git's `push.default=simple` does, and when it pushes to the
upstream's remote it refuses a branch whose config names an upstream under another name, so
what push uploads to is always what the counts and pull compare against.

A branch whose `branch.<name>.remote` is a bare URL rather than a remote's name can be
pushed, but not fetched or pulled: with no remote there are no fetch refspecs and no
remote-tracking ref to read. Fetch and pull refuse it with a message that names the URL and
says to add a named remote (`git remote add`) and point the branch at it.

Every one of them ends. The app carries a cancel flag into the call that the user can
raise from the other side while the transfer holds the thread, and each transport keeps
an idle budget — the gap it tolerates between bytes, not the length of the transfer, so
a slow link is never punished and a dead one does not wait forever. A cancelled
operation stops between steps and leaves nothing half-applied.

## GitHub account

A host and a login the user signed into from Settings or `rc-git auth`, with how they signed
in (a GitHub App or an OAuth App through a device flow, or a pasted token). The record lives in
app settings and its token in the login keychain under the same host and login, and that
host alone decides where the token is sent: a repository's remote URL never does. A remote
picks an account only by naming a repository on exactly the account's host, compared whole
after parsing; with several accounts on one host the repository uses the one the user chose for
it, else rc-git asks, and a second account's token is never tried.

## Device flow

How a GitHub App or an OAuth App signs in without a client secret: rc-git asks the host for a
short user code, the user enters it on the host's own `/login/device` page (the only page rc-git
opens; a server naming another is refused), and rc-git polls until the user approves, declines,
or the code expires. A GitHub App's token then lasts eight hours and renews with a single-use
refresh token; renewals of one account never run two at once, and a refused renewal signs the
account out only when no other writer (the CLI, the app) renewed it first.

## Repository link

Which GitHub repository a repository's pull requests come from, and through which account. It
is read from the remote fetch uses, and the remote names only an owner, a name and a host to
look an account up by; the account's host is where the token goes. When the remote's host is an
alias in the user's own ssh config, the host the alias stands for is looked up instead, under
the same rule: one account on the host, the one chosen for the repository among several, else
ask. A repository whose remote names no host with an account is linked to nothing, and shows no
pull requests; github.com without an account offers the sign-in.

## Pull request stamp

What the user saw of a pull request: its number, head commit, base branch and state. Only a
read that is current makes one; a screen left by a failed refresh is stale and stands for
nothing. It is what an action on the pull request is to compare a fresh read against, refusing
with "changed since you looked" when they differ. The fetch of a pull request head is the first
to use it: it writes nothing unless the remote's head is the stamp's.

## Pull request head

A pull request's head commit, fetched from `refs/pull/<n>/head` on the remote pull requests are
read from into rc-git's own ref for it, `refs/rc-git/pr/<n>/head`, so its changes can be read
from local objects. Fetched only when the user asks, from one refspec built from the number,
force-updated as the pull request is rewritten, and never checked out: the ref keeps the objects
alive and lists as "other", never as a branch or a tag. Its changes run from the merge base of
the local copy of the base branch and the stamp's head, GitHub's three-dot view, and always
belong to that stamp.

## Host-key trust

How an SSH server proves its identity to rc-git. The `known_hosts` files OpenSSH would
read (the config's `UserKnownHostsFile`, else `known_hosts` and `known_hosts2`, then the
system files) are consulted read-only and never written; a host they do not know is
refused until the user approves the offered key's fingerprint, and that approval is
remembered by the app itself, per host.

A key that contradicts an earlier in-app approval is refused the same way, without the trust
sheet: a changed key is never treated as first contact. When the change is a legitimate
rotation, the user removes the host's approval in Settings (Approved SSH Host Keys); the next
fetch then meets the new key as unknown and asks again.

A key that is `@revoked`, or that contradicts every `known_hosts` entry for the host, is
refused with no way to approve it inside rc-git — like a guard deny, the message points the user at ssh itself. Lookup is keyed
by the name actually dialed, so a config alias shares the trust of the real host behind
it — unless the config sets `HostKeyAlias`, which is looked up in its place, bare, as
OpenSSH does.

## Tick

What the user includes in the next commit or stash. Every changed file starts ticked, and an
untick survives a rescan by path rather than by position. A tick is whole-file by default; a
file whose hunks are ticked individually carries only those hunks, and ticking every hunk
collapses back to a whole-file tick while unticking the last one unticks the file. A row that
stands for several files — a folder in the change list — is ticked when all of them are, mixed
when only some are, and one click moves the whole set.

## Preflight

The checks a self-update runs before it touches anything: the login shell must find the build's
tools, and the checkout must be readable. Uncommitted tracked changes are logged, not refused: the
rebuild ships the working tree as it stands. Both halves run outside the process, so both are
bounded and both are logged before they run: a preflight that cannot answer fails with a reason
rather than leaving the window spinning on one line.

## Session handover

What a self-update passes to the build that replaces it: the open tabs, in on-screen
order, with the selected one. It is a handover rather than a save because both processes
are alive at once — the new build reads the state while the old one is still running, so
the write is flushed before the launch and the old process stops writing afterwards. The
state is scoped by bundle identifier, which is why the identifier never depends on a
version or a build number.

## Stop

One place the back/forward list can return to: a tab, the pane it shows, and where its
browser is — the tree it browses, the open file, what the history lists, and the commit in
the detail. A stop names those things and holds none of them, so returning to one reads the
repository as it is then: a ref that moved shows its new tip, and a file that is gone leaves
the pane empty.

The list is one for the whole app, so a step back can cross from one repository to another.
A move to where the list already is records nothing, and a move somewhere new drops the
stops ahead. Only a change records a stop: when returning to one falls short, because what
it names is gone, the list keeps its place and the screen is not recorded in its stead.
Stops live in memory only, and closing a tab drops the ones that name it.
