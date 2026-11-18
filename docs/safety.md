# Safety

Two promises: rc-git cannot damage a repository, and rc-git cannot be tricked by one.

## The core cannot write

Four layers enforce it, and each catches what the others miss.

1. **Lint.** `clippy.toml` lists the gix object and ref writes (`edit_reference`,
   `write_object`, `commit`, `tag`, `edit_tree`, `Reference::delete`, the `objects` and `refs`
   stores' write and transaction methods, `gix::init`, ...), the `gix_lock` lock-file acquire
   and commit methods, the `std::fs` mutations (write, create, copy, link, rename, remove,
   permissions, ownership, times), the std and tokio `Command::new` spawns, and the network
   entry points. Clippy rejects a call to any of them in any crate. The test kit opts out per
   function with a written reason, and `rc-git-guard` — the one sanctioned write path — opts
   out the same way at each write it performs: every ref edit, and the working-tree and reflog
   writes that go with them. A write
   primitive the workspace has not used before joins the list in the change that first uses it;
   an unlisted one passes the lint and leaves the guarantee unenforced for that path.
2. **Test.** `crates/rc-git-core/tests/read_only.rs` snapshots every file under `.git`, runs
   every core operation — working-tree listings and file reads included — and compares the
   snapshot byte for byte.
3. **Features.** A standalone build of `rc-git-core` compiles `gix` without checkout,
   transport or credential features; the `core standalone` step in `scripts/check.sh` and CI
   builds it alone, because Cargo unifies features across a workspace build and the linked
   library carries the guard's. `rc-git-guard` alone enables the transports — HTTPS
   (`blocking-http-transport-curl`, system libcurl) and SSH (russh, in-process; no `ssh`
   child) — and clippy bans the network entry points it lists — gix's `remote_at`,
   `find_remote`, `connect`, `handshake`, plus `russh::client::connect` and the std and
   tokio `TcpStream::connect` — outside the guard's `actions/remote` module, and `curl`'s
   `Easy`, `Easy2` and `Multi` constructors outside its GitHub client. The ssh-agent
   socket (`AgentClient::connect_uds`, `connect_env`) is banned the same way: the agent signs
   on the user's behalf, so only the guard's `connect_agent` opens it. The curl
   feature also compiles gix's credential-helper and prompt machinery into the binary;
   it is unreachable: every connection, HTTPS and ssh, installs a credential callback that
   answers from caller-supplied credentials, only to a 401 from the host they were issued
   for (`RemoteAuth::host`, else the remote URL's), and never consults the helper cascade; fetch and
   push build their HTTP options from code-owned defaults, so no repository `http.proxy`
   ever asks the cascade for a proxy password; and `Command::new` stays banned
   workspace-wide. The ssh side reads `~/.ssh` (config,
   `known_hosts`, keys) and writes none of it; host-key approvals live in the app's own
   defaults. The `index` and `excludes` features are on so the working-tree view can
   honor ignore rules; both read.
4. **Unsafe.** `unsafe_code` is forbidden in every crate except `rc-git-mac`, which binds the
   system frameworks; there each block carries a documented contract and clippy denies
   undocumented ones.

## Untrusted repositories

Opening a repository someone else produced must be harmless.

- `Options::isolated()`: only `.git/config` is read. No `~/.gitconfig`, no system config, no
  environment variables, and `include`/`includeIf` directives are ignored. One exception:
  the user's global ignore file at git's default location, `$XDG_CONFIG_HOME/git/ignore` or
  else `$HOME/.config/git/ignore`, is passed in as an API override of `core.excludesFile`.
  It ranks below the repository's `.gitignore` files and `.git/info/exclude`, as in git, and
  outranks the repository's own `core.excludesFile`. A `core.excludesFile` set in
  `~/.gitconfig` is not read.
- A repository's `http.*`, `remote.<name>.proxy` and `credential.*` keys are never applied
  to a transfer. Fetch and push build their HTTP options from code-owned defaults (timeouts
  only), so the repository cannot pick a proxy, switch off certificate checks, add a CA or a
  header, or run a credential helper. libcurl itself still honours `http_proxy`,
  `HTTPS_PROXY`, `ALL_PROXY` and `NO_PROXY` from the process environment, as git does; a
  loopback remote bypasses every proxy, environment ones included.
- What `.git/config` itself can name outside the repository:
  - `core.worktree`: refused. Core and guard both open through `require_work_tree_at`, which
    fails unless the working tree resolves to the opened directory (or the parent of an
    opened `.git` directory), naming the key and its value. A submodule's `core.worktree`
    leads back to its own checkout and opens. Every guard write derives its target from the
    working tree, so a refusal at open is what keeps discard, checkout, pull and commit
    inside the repository.
  - `core.excludesFile`: never read; the ignore stack is built from the global file above and
    `.git/info/exclude` only.
  - `objects/info/alternates`: followed. Objects may be read from another repository on disk
    (`git clone --shared` and `--reference` depend on it); nothing is written there.
- Ignore files are read only when they are regular files (through any symlink) of at most
  4 MiB: the global file and `info/exclude` by rc-git itself, each directory's `.gitignore`
  checked before gix reads it. A FIFO, device or oversized file is skipped with a warning and
  its rules do not apply, so it can neither block a scan nor grow memory.
- File names are bytes, not strings, but paths and ref names travel from the app back to the
  core as UTF-8 only. Non-UTF-8 names display with replacement characters, and a file with one is
  still reachable by object id in tree browsing. By path it is not: blame, path history,
  worktree reads and guard actions would resolve the U+FFFD spelling, which can name a
  different object. So the app refuses blame, file history, Copy Path and reveal on such a
  path, leaves such a status row out of commit, stash and discard, and offers no delete or
  create on such a ref.
- Blob reads and diffs are capped. The header is checked before decoding; the default cap is
  8 MiB, the core clamps any cap to 4 GiB (`MAX_READ_LIMIT`, applied inside every read that
  takes one, so every caller gets it), and the app refuses "Load anyway" above 512 MiB.
- Binary detection follows git: a NUL in the first 8000 bytes. Binary content is never handed
  to the text view.
- Errors are messages, never panics. Clippy denies `unwrap`, `expect`, `panic`, `todo`, and
  `unimplemented` outside tests; release builds abort on panic rather than unwind across the
  C boundary.
- Network I/O exists only behind `rc-git-guard`'s fetch, pull, push, and sync actions,
  speaks HTTPS and SSH only (plain `http://` is accepted for loopback hosts, so tests
  can run a local server end to end), and sends credentials only when the caller passed
  them. SSH host keys are verified against a read-only `known_hosts`: an unknown key is
  refused until the user approves its fingerprint, and a contradicting key is always
  fatal. A key that contradicts an approval made in the app is fatal too; the only way past
  it is for the user to remove that host's approval in Settings, after which the next fetch
  treats the key as unknown and asks again. The app never drops an approval on its own.
  Nothing else in a repository operation leaves the machine; the GitHub API client and the
  updater below are the exceptions.

## Guarding mutations

`rc-git-guard` decides what a user must do before any action that changes a repository, and
its `actions` module is the only code in rc-git that performs one. Today that means branch
and tag creation and deletion from the sidebar; from the Changes tab, committing selected
files or hunks, the stash lifecycle, and discarding uncommitted changes; switching `HEAD` to
another local branch, which the Back-to-default control composes with a fetch and a
fast-forward; and the remote operations — fetch, fast-forward pull, plain push, and their sync
composition — all driven through `rc_git_guard::actions` from rc-git-kit. Every other row of
the table is settled policy
waiting for its feature. `Policy::evaluate` maps an `Action` to a `Decision`:

| Action | Protected branch | Elsewhere |
|---|---|---|
| fetch, plain push | allow | allow |
| checkout | allow (the write path refuses dirty files, and a branch another worktree has checked out) | same |
| pull (fast-forward) | allow (the write path refuses dirty files) | same |
| create branch, create tag | allow (never overwrites) | allow (never overwrites) |
| force push | **deny** | type the branch name |
| delete local branch, merged | **deny** | confirm |
| delete local branch, unmerged | **deny** | type the branch name, backup ref |
| delete remote branch | **deny** | type the branch name, backup ref |
| delete tag | type the tag name, backup ref | same |
| reset --hard | type the branch name, backup ref | same |
| rewrite unpublished history | confirm, backup ref | same |
| rewrite published history | **deny** | type the branch name, backup ref |
| discard working changes | confirm (no backup: the bytes are nowhere else) | same |
| prune unreachable objects | type `prune` | same |
| commit selected changes | allow (adds objects, advances the checked-out branch) | same |
| stash push | confirm (the stash commit is the record) | same |
| stash apply | allow (the write path refuses dirty files) | same |
| stash pop | confirm, backup ref | same |
| stash drop | type `drop`, backup ref | same |

Protected patterns default to `main`, `master`, and `release/*`, matched ignoring ASCII case:
a case-insensitive volume stores `MAIN` and `main` as one file, and the policy has no
repository to ask, so `Main` is protected on every volume. A `Deny` cannot be confirmed
away inside rc-git; the message points the user at git itself. A `Confirm` produces a
`Challenge`; only `Challenge::verify` — with the exact phrase when the challenge names one,
which the rows above spell out — yields a `Confirmation` (an `Allow` yields one directly, from
the policy's own evaluation). Every write is a method on the guard's `Writer`, which can only
be built from a `Confirmation`, and nothing outside the policy can build one: the write path
holds one before it edits anything, so the compiler enforces the flow.
`backup_ref` means a ref under `refs/rc-git/backup/` is written before the deletion — the
branch tip, the tag object, or the dropped stash commit — at
`refs/rc-git/backup/<kind>/<name>`, where `<name>` escapes `%` as `%25`, `/` as `%2F` and `-`
as `%2D` so `feature/x` backs up to `feature%2Fx` beside `feature`'s backup rather than under
it, and `feature-2` to `feature%2D2` rather than into `feature`'s second slot. A repeat takes
the slot one past the highest existing one (`-2`, `-3`, …), so no backup ever overwrites an
earlier one; a backup of the same name that already holds the object (under this name or the
unescaped-dash name older versions wrote) is reused, so a retried action adds no second slot.
Two more rules live in the write path itself:
a branch checked out in any worktree — the opened one, the main worktree, or any linked one —
cannot be deleted (compared ignoring case, at every hop of a `HEAD -> alias -> branch` chain;
its delete plan is a `Deny` naming the checkout and
the worktree's path unless the policy already denies it, a worktree whose `HEAD` cannot be read
refuses rather than counts as elsewhere, and the merged check never counts `HEAD` on the branch
itself, directly or through a symbolic alias, nor any symbolic branch, as another home for its
commits), and a ref is never created over an existing one.
Checkout likewise refuses a branch another worktree has checked out, naming that worktree as
git does. Every name a delete, a checkout or the merged check acts on must appear
byte for byte in the repository's own branch or tag listing, so `MAIN` never reaches `main`
through filesystem case folding. Under `core.ignoreCase`, creating or deleting a name that
another ref matches except for case is refused, since both names are one file on disk.

The Changes tab's write paths add their own guarantees:

- **Raw bytes throughout.** Status, diffs, commits, and restores all work on the file's raw
  bytes: gitattributes filters and `core.autocrlf` are not honored (matching the Files
  view), so what the diff shows is exactly what a commit records and a restore writes. One
  exception keeps a converted file (eol, LFS) from reading changed: status first asks the
  index, and a file whose stat still matches an entry holding the tree's id reads clean.
- **Commit** builds blobs from disk, rewrites only the touched directories, and advances the
  branch through the same single ref-edit funnel — only if `HEAD` still is the commit the
  change list was built against. Hunk selections are re-validated against a fresh diff and
  refused when the file moved. The committed paths are then updated in `.git/index` so
  terminal `git status` agrees; entries staged from the terminal for other paths survive.
- **Stash** entries are written in git's own format — `git stash list`, `apply`, `pop`, and
  `drop` all accept them — and rc-git's apply refuses to touch any file that is not exactly
  at the stash's base state, so it can never overwrite uncommitted work. Apply, pop and drop
  name the entry by position and by the stash commit the list showed there, and refuse when
  that position now holds another entry; the pop and drop challenges name the entry and its
  message. When `refs/stash` is not the commit the newest readable reflog line records (a
  cut-short last line, say), `git stash list` and `git stash apply stash@{0}` name different
  entries, so apply, pop and drop refuse and hand the stash to git. Ticked untracked files go into the stash tree itself rather than git's
  third-parent scheme; `git stash pop` recreates them all the same. An entry made with
  `git stash -u` or `-a` carries that third parent, which rc-git does not restore, so apply
  and pop refuse it before writing anything, name its untracked files, and hand it to
  `git stash apply` or `git stash pop`; the entry stays in the list.
- **Paths are checked before anything is touched.** Every path a commit, stash, discard,
  checkout, pull or stash apply would read, store or write passes git's own checkout rules
  first: no empty, `.` or `..` segment (so no leading, trailing or doubled `/`), no `.git`
  segment in any spelling HFS+ or NTFS resolve to it (`.GIT`, `GIT~1`, ignorable code
  points), no symlinked `.gitmodules`, and no parent directory that is a symlink. A bad
  path is refused, never normalized, and it refuses the whole operation before the first
  byte is written — so a fetched tree carrying `.git/hooks/…` cannot plant a hook, and
  `.git/refs/heads/main` cannot be discarded as a file. A checkout or pull that writes a
  symlink and also a path below a directory spelled like it in another case (`D` and
  `d/payload`) is refused from the plan, since a case-insensitive volume would resolve that
  directory to the new link. Under `core.ignoreCase`, a commit or stash refuses a selected
  path that matches a file on disk or an entry in `HEAD` only by case (`F` for `f`), naming
  both spellings, instead of storing the bytes under a second name. A commit or stash of a
  path inside a submodule (`sub/x` where `HEAD` records `sub` as a gitlink) is refused, as
  `git add` refuses it, instead of replacing the submodule pointer with a directory.
- **Discard** and stash restores write through one helper that re-checks every parent
  directory at write time and removes the old file first, so no write follows a symlink,
  whether at the last segment or above it, and deleting an already-gone file is not an
  error — a retried action lands in the state it retries from. Discard deletes a path
  missing from `HEAD` only when its directory holds an entry of exactly that name, so on a
  case-insensitive volume `ALPHA.TXT` never deletes the tracked `alpha.txt`.
  Both also reset `.git/index` for exactly the paths they restore — `HEAD`'s entry, or none
  for an untracked path — as `git restore --staged --worktree` and `git stash push` do, so a
  staged copy of the discarded or stashed change is never left for a terminal `git commit`.

- **A git operation left in progress** (`MERGE_HEAD`, `CHERRY_PICK_HEAD`, `REVERT_HEAD`, a
  rebase or `git am` directory) refuses commit and stash push, which would otherwise record
  work on top of it and lose the merge ancestry; a bisect is allowed there. Checkout and
  fast-forward pull refuse every one, a bisect included, and also refuse an index holding an
  unresolved conflict at any path, before a byte is written.

- **A branch switch** applies the tree diff between the two commits to exactly the changed
  paths, through the same planner the fast-forward pull uses: any dirty or staged overlap
  refuses the whole switch, named by path, before a byte is written, and paths the switch does
  not touch keep their changes. A case-only rename (`Readme.md` to `README.md`) is not an
  overlap: the added path exists on disk only as the clean file the switch deletes, and
  deletions run before writes. `HEAD` moves last, expecting the branch or detached commit the
  switch was planned against. Its `checkout:` reflog line is appended explicitly, because a
  gix transaction writes none for a symbolic update — without it `git checkout -` and `HEAD@{n}`
  would name the wrong commit
  (`docs/solutions/integration-issues/gix-no-reflog-for-symbolic-ref-update.md`).

The remote operations add their own guarantees:

- **Fetch** runs gix's own fetch machinery: the pack lands in `.git/objects/pack` and the
  remote-tracking refs move inside that one call — the single ref-write path that does not
  go through `apply_edit`. Tracking-ref updates lose nothing. `FETCH_HEAD` is never
  written.
- **Fast-forward pull** applies the tree diff to exactly the changed paths. Any dirty or
  staged overlap — an untracked file in the way included — refuses the whole pull before a
  byte is written. The branch ref moves last, expecting the old tip, so an interruption
  leaves visibly modified files rather than what looks like lost work, and a branch moved
  from the terminal mid-pull fails the ref edit instead of clobbering it.
- **Push** is never force from rc-git: the remote tip must be an ancestor of the local tip,
  checked client-side before anything is sent and enforced again by the server. It goes to
  the remote and URL `git push` would use (`pushRemote`, `pushDefault`, `pushurl`) and moves
  only that remote's tracking ref. It writes only the same-named remote branch and refuses,
  before connecting, a push to the upstream's remote whose `branch.<name>.merge` names
  another one. The pack is self-contained (never thin). A rejection surfaces the server's reason verbatim.
- **Credentials** are per-host, supplied by the caller (the app reads the login keychain),
  passed to the guard per call, and never stored or logged by rc-git-guard.
- **Every transfer ends.** Each remote call carries a `Cancel` the app can signal
  from another thread; the step in flight notices it within 200 ms. The ssh transport
  wakes out of its connect and every read to look at it, and abandons a host-name lookup
  it no longer needs rather than waiting for the resolver. curl has no interrupt, so every HTTPS network step
  (connect, handshake, pack receive, upload, report read) runs on a detached thread the call
  stops waiting for; that thread lingers until its next object or 64 KiB upload chunk, or
  until curl's timeout below ends it. The pack steps check the flag between objects. A
  connection that stops moving bytes dies on its own after 120 seconds —
  `http.lowSpeedLimit`/`lowSpeedTime` set in code for curl (an isolated open reads no
  global config), an idle budget per read and write for ssh — with one 30-second budget
  that dialing, the handshake, authentication and, for ssh, opening the session channel
  share. A cancelled operation stops between steps: nothing is left
  half-applied. A transfer already past its last check can still land after the cancel
  returns — a fetch moving its tracking refs, a push the server accepts — exactly as if it
  had finished just before; the next fetch shows it. A fetch claims its repository until its
  thread really ends, so a fetch started right after a cancelled one waits for it (up to the
  connect budget) instead of racing its ref updates, and otherwise refuses with a retry
  message. The last cancel check before gix moves the tracking refs is just before the pack
  receive; past it, only the pack writer's per-object check can still stop the fetch.

## GitHub API

Pull request features talk to GitHub's REST and GraphQL APIs with a token that can merge and
delete branches, so a leaked or misdirected token is the worst outcome here. The threat: a
repository someone else produced controls its own remote URL and config, a host can answer
with a redirect, an oversized body or a page to open, and every title, branch name and log line
is written by whoever opened the pull request. The client is
`crates/rc-git-guard/src/actions/remote/github/`; it is the only code in rc-git that calls
libcurl directly.

- **The token never follows the repository.** Every URL comes from a `GitHubHost` the user
  typed in Settings (a lowercase DNS name and a port other than 443): `https://api.github.com`
  for github.com, `https://<host>/api/v3` and `https://<host>/api/graphql` for an Enterprise
  host. A remote URL only selects an owner and a name, and only when its host is the account's
  exactly after parsing (`RepositoryTarget::on_host`): no suffix, substring or path match, so
  `github.com.evil.example` and `evil.example/github.com` get nothing. An `https://` remote
  carrying userinfo, an ssh remote with a user other than `git` or a port other than 22, and
  `git://`, `file://` and plain `http://` remotes are refused before any request. Owner and name
  must be `[A-Za-z0-9._-]+` and not `.` or `..`, the owner at most 39 characters and the name at
  most 100 (GitHub's own limits), and a remote URL over 1024 bytes is refused unparsed.
- **No account fallthrough.** Accounts and the per-repository choice live in app settings
  (`rc-git.githubAccounts`, `rc-git.githubRepositoryAccounts`, keyed by repository path), never
  in the repository's config. One account on the host is used; several with no choice ask; a
  second account's token is never tried (`account_for`, rc-git-kit).
- **Tokens.** Only in the login keychain: a generic-password item per host and account, service
  `rc-git`, account `github-api/<host>/<login>`, holding the access token, a GitHub App's
  refresh token and their expiries. Generic passwords never match the HTTPS push and ssh items,
  which are internet passwords queried by host and protocol alone. A sign-in deletes whatever
  item holds its account and adds a fresh one (`save_new_tokens`), so an item another program
  planted under that service and account, with an access list of its choosing, never receives
  the token; one that reappears between the delete and the add fails the add. A refresh
  rewrites the data of the existing item only (`update_tokens`) and never adds one, so it
  cannot bring back an account a sign-out deleted. Never in settings, logs, error messages,
  argv or the environment: `ApiToken` and `RefreshToken` print `<redacted>` under `Debug` and
  have no `Display`; the transport's request, body and response types print no secret either;
  and every message built from server or curl text is scrubbed of the secrets the request
  carried. A token must be visible ASCII, so it cannot split a header. `rc-git auth login
  --method token` reads it from standard input and refuses a terminal. The keychain key decides
  where a token goes: a settings record naming another host for a login reads no token.
- **Transport.** `https://` only: the request URL is checked in the one send function, and
  plain `http://` reaches a loopback host only under `cfg(test)` or the dev-only
  `dev-loopback-http` feature. `scripts/check.sh` proves that a `cargo build -p rc-git-app` or
  `cargo build -p rc-git-cli`, and so the bundle `scripts/build-app.sh` assembles, lacks the
  feature. It proves nothing about a binary `cargo test` leaves behind: Cargo unifies features
  with dev-dependencies when it builds tests, so after any `cargo test` the
  `target/<profile>/rc-git` it built carries the feature (as it carries the dev-only
  `http-client-insecure-credentials`), speaks plain http to loopback and honors the CLI's test
  hooks. Such a binary is a test artifact; never use it with a real account. The `curl` crate
  has no safe setter for curl's protocol list and `unsafe` is forbidden outside rc-git-mac, so
  that URL check, with redirects off, is the restriction: curl connects to that URL alone.
  Redirects are never followed and a 3xx is an error, so a redirect target receives nothing.
  Peer and host verification stay on. `clippy.toml` bans the curl constructors outside the
  client, and everywhere the setters that would turn verification off (DoH's included), add a
  CA, lower the TLS version or ciphers, load a crypto engine or entropy source, print the
  headers (`verbose`), hand credentials to curl or a proxy, send them across a redirect, or
  connect anywhere but the URL's host as the system resolves it. It also bans gix-transport's
  own HTTP client (the `Curl` type, `http::connect`, `Transport::new`, `Http::get` and `post`),
  which fetch and push reach only through the banned connect entry points. The dependency graph
  is checked too: only gix-transport and rc-git-guard may depend on `curl`, and only `curl` on
  `curl-sys`, however a manifest spells it. `.netrc` is ignored. Nothing is read from the
  repository; libcurl still honors the process environment's proxy variables, as fetch and push
  do, and a loopback host bypasses them.
- **Bounded.** Every call takes a `Cancel`: the connect budget, the idle budget and a total of
  both, a wait that returns within 200 ms of a cancel while curl's progress callback aborts the
  transfer at its next tick, and caps on the body (4 MiB for the API, 64 KiB for sign-in) and
  the headers (64 KiB). Over the cap is an error, never a truncated parse.
- **Least privilege, said before sign-in.** Settings names what each method may do: a GitHub
  App needs Pull requests, Contents and Actions read and write, Checks, Commit statuses and
  Metadata read; an OAuth App asks for `repo`; a fine-grained token should carry the GitHub
  App's permissions. After sign-in the sheet and `rc-git auth` show the login, `X-OAuth-Scopes`
  and `GitHub-Authentication-Token-Expiration` when the host sends them.
- **Device flow.** rc-git opens only `https://<host>/login/device`; an answer naming another
  `verification_uri` stops the flow before any poll. Polls wait `interval` and slow down by what
  `slow_down` says, stop at `expires_in` without another request, end on `access_denied` and
  `expired_token`, and every wait looks at the cancel flag. One flow runs per process, and the
  sign-in sheet's state machine (`SignInFlow`, rc-git-kit) holds it only while a poll follows,
  so a failed or cancelled attempt frees it for the next. No client secret exists anywhere:
  GitHub refreshes a device-flow token with the client id alone. The device code is never shown
  or logged, and a test makes the fake host echo it (and a refresh token) back in each place an
  error can carry text.
- **Refresh.** A GitHub App refresh token is single-use. `TokenKeeper` (rc-git-kit) reads the
  keychain on every call, so a sign-out in the app or the CLI stops the other at its next call;
  it serializes refreshes per account in one process, and after a refused refresh reads the
  keychain once more: only when the item still holds the refused refresh token is the account
  signed out, since the CLI may have renewed it. A renewal whose item was deleted meanwhile is
  dropped, not saved. A refresh that cannot reach the host signs nobody out.
- **Untrusted text.** `single_line` shows control characters, every `Bidi_Control` code point,
  the line and paragraph separators and the zero-width characters as visible `\u{..}` escapes
  and drops ANSI escape sequences; every server message in an error passes through it, and so
  does every free-text field read back from the settings file. An OAuth `error` code is shown
  only when it is 1 to 64 of `[a-z_]` and holds no secret of the request, else as
  `unrecognized error code`. GraphQL queries are static strings and every value travels in
  `variables`.
- **CLI test hooks.** `RC_GIT_SETTINGS` and `RC_GIT_KEYCHAIN_SERVICE` point `rc-git auth` at a
  scratch settings file and keychain service, and are read only when `DEV_ALLOWANCE`
  (`github/host.rs`, true only with the dev allowance compiled in) says so. A shipped CLI reads
  neither, so its environment cannot point it at other accounts.

Sign-in, the token check and the refresh change no repository, so they have no `Action`; the
pull request mutations of later slices will each get a policy row in the table above.

## Updater

**Rebuild and Relaunch** in the app menu rebuilds rc-git from its own checkout, and it sits
outside the guarantees above. Its spawns go through rc-git-mac's bounded process runner, the
one `Command::new` exempt from the clippy ban. It does not fetch: it runs
`scripts/build-app.sh` on the working tree as it stands, uncommitted changes included, through
a login `/bin/zsh -lc`, so the user's shell profile runs too. That script calls `git` on
rc-git's own checkout — never on a browsed repository — and `cargo build`, which may fetch
crates when the lockfile moved. A rebuild therefore executes whatever the checkout holds:
trust in the rebuild is trust in that checkout.

## Supply chain

The GitHub client adds `curl` and `serde_json` as direct dependencies of `rc-git-guard`, at the
versions `Cargo.lock` already held (gix-transport links `curl`; GPUI's tree has `serde_json`),
so they added no crate to the lockfile; `curl` keeps gix-transport's features (none: the
system libcurl and its TLS).

Rust dependencies are pinned to exact versions in `Cargo.toml` and checked by `cargo deny` for
advisories, licenses, and sources on every run of `scripts/check.sh` and in CI. GitHub Actions are
pinned to commit hashes and scanned by `zizmor`; Dependabot updates both ecosystems weekly with a
seven-day cooldown.
