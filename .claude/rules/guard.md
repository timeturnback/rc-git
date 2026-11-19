---
description: how any future mutating action must be gated, and what rc-git-guard already decides
paths: crates/rc-git-guard/**
---

# rc-git-guard

The policy for every mutating action, and — in the `actions` module (`actions.rs` plus
`actions/`) — the only code in rc-git that performs one. Branch and tag management go through
here; so does everything that comes later.

- Every entry point calls `authorize` (actions.rs), which runs `Policy::authorize`: the policy
  evaluates the `Action` itself and returns a `Confirmation` for an `Allow`, through
  `Challenge::verify` for a `Confirm`, or the refusal. Every write is a method on
  `actions/write.rs`'s `Writer`, built only by `Writer::new(repo, &confirmation)`, so the type
  system forces a verified challenge, and the typed phrase only where the policy names one. A
  confirm with no phrase verifies for any input: its button lives in the front end (the GUI's
  confirm step, the CLI's `--yes`). `Challenge` and `Confirmation` have fields private to
  `policy.rs`; nothing else in the crate can build either one.
- The front end renders decisions; it never classifies an action itself and never offers a
  one-click path around a challenge. The front end passes the user's typed input through; the
  verification lives here.
- `Deny` is final. Force-pushing, deleting, or rewriting published history on a protected
  branch is not confirmable inside rc-git; the message tells the user to use git directly.
- A challenge with `backup_ref` writes a ref under `refs/rc-git/backup/<kind>/<name>` before
  the action runs. `<name>` escapes `%` as `%25`, `/` as `%2F` and `-` as `%2D`, so no backup
  nests under another or reads as another name's slot; a repeat takes one past the highest
  existing slot (`-2`, `-3`, …, read once, not probed), so nothing is overwritten, and a slot
  of the same name already holding the object is reused. `Writer::write_backup_ref`
  is that one implementation, gated on `Writer::backup_required`; new destructive actions
  reuse it. It refuses a slot whose name differs from a ref of its kind only in ASCII case
  (`refuse_case_twin`): on a case-insensitive volume the new loose ref would hide the old backup.
- Ref edits go through `Writer::apply_edit` (synthetic `rc-git` committer for the reflog).
  Object, index, working-tree and reflog writes live beside it on the `Writer`, each behind its
  own `#[expect(clippy::disallowed_methods, reason = ...)]`; nothing outside the `actions`
  module writes, and `scripts/check.sh` fails on a write opt-out outside `actions/write.rs`.
  A new write is a new `Writer` method.
- Two hard rules live in the write path itself: never delete the checked-out branch, never
  create over an existing ref (`PreviousValue::MustNotExist`).
- Anything that moves the working tree to another commit goes through `actions/treeapply.rs`:
  `plan` the tree diff, `verify_clean` every touched path (a dirty one refuses the whole
  operation by name, before a byte is written), `apply`, then move the ref *last*. The
  fast-forward pull and the branch switch are both that sequence; a new one reuses it rather
  than walking a tree itself.
- Pull stays fast-forward only: a divergence returns `ActionError::Diverged` and the app hands
  the user a quoted `git pull --rebase` for the checkout. Settle signing before adding anything
  that rewrites commits (rebase or merge pull, amend, squash): guard commits carry no signature
  and the user's signing config sits outside the isolated open
  (`docs/solutions/architecture-patterns/fast-forward-only-pull-hands-divergence-to-git.md`).
- A gix ref transaction writes no reflog for a symbolic update — it has no object id to log —
  and a direct branch edit logs only the branch, so checkout, commit and pull append their
  line to `logs/HEAD` themselves through `Writer::open_head_reflog` (before the first write)
  and `Writer::append_head_reflog`, only when the log already exists. Without it
  `git checkout -` and `HEAD@{1}` would silently point at the wrong commit.
- Protected patterns default to `main`, `master`, `release/*`, and are fixed: every caller
  uses `Policy::default()` and no setting changes them yet.
- The app reaches the policy through rc-git-kit's guard bridge; never reimplement it in
  rc-git-kit or rc-git-app.
- Every remote entry point takes a `Cancel` and must stay interruptible: no new blocking
  call without a cancel poll and an idle budget (`CANCEL_POLL`, `IDLE_TIMEOUT` in
  `actions/remote.rs`). A blocking read with neither is how a sync gets stuck forever.
  Build a tokio timer inside the `async` block, never as an argument to `block_on`: a
  `Sleep` needs the runtime context that `block_on` has not entered yet.
- The policy settles rows ahead of their write paths on purpose ("settled policy waiting for
  its feature", `docs/safety.md`): force push, remote branch delete, reset --hard, history
  rewrite and prune each have a `docs/roadmap.md` Planned entry and are tested only in
  `lib.rs`'s tests. Delete such a variant only with its roadmap entry, together with its
  `risk` and `Policy::evaluate` arms, its tests and its `docs/safety.md` row.
- Every rule change updates the table in `docs/safety.md`, the tests in `lib.rs`, and the
  tests in `tests/actions.rs` together.
- Remote I/O lives in `actions/remote/` only; the network entry points (`find_remote`,
  `Remote::connect`, `handshake`, …) are clippy-banned everywhere else. HTTPS and SSH —
  `vet_remote_url` routes the scheme, allowing plain http for loopback hosts so tests
  run end to end. The ssh transport (`actions/remote/ssh/`) is in-process russh:
  `known_hosts` is never written (approvals arrive as `RemoteAuth::accepted_hostkey`),
  channel streams must drop inside the transport's runtime, and the passphrase rides
  `Credentials::token`. The key ladder in `ssh/auth.rs` keeps OpenSSH's offer order and
  honors `IdentitiesOnly`: on GitHub the first accepted key picks the account
  (`docs/solutions/integration-issues/ssh-auth-offers-wrong-agent-key-ignoring-identitiesonly.md`).
  On the HTTPS side always install the credential closure; never let gix's helper
  cascade run (it spawns). Fetch and push take their HTTP options only from `http_options`
  in `actions/remote.rs`: repo-local `http.*`, `remote.<name>.proxy` and `credential.*`
  keys are ignored, and a connection left without options would load them from config.
  `docs/solutions/architecture-patterns/hand-rolled-git-push-over-gix-https.md` has the
  push recipe and its traps (`prevent_pack_unload`, `MissingCommitter`, dev-only
  `http-client-insecure-credentials`).
- GitHub API calls go through `actions/remote/github/http.rs`'s `send` only: it fixes https-only,
  no redirects, body and header caps and timeouts. Never build a curl handle elsewhere, nor
  gix-transport's own HTTP client (clippy bans both; `check.sh` checks who depends on curl).
  A curl setter that weakens TLS, adds a CA, prints headers, hands curl credentials or picks
  another address joins `clippy.toml` for `Easy` and `Easy2` both.
- Build every GitHub URL from the account's `GitHubHost`; a remote URL only yields owner and name
  through `RepositoryTarget::on_host`. Never let repository config or a remote URL pick a host.
- Hold tokens as `ApiToken`/`RefreshToken` (redacted `Debug`, no `Display`), and hand-write a
  redacting `Debug` for any type that carries one (`Request`, `Body`, `Response`); put a secret in
  a header or form body, never a URL. Pass server text through `server_text` with the request's
  secrets, and an OAuth `error` code through `FormAnswer::error_code`, before either reaches an
  error.
- Send a GitHub App's refresh only through kit's `TokenKeeper`: refresh tokens are single-use, and
  it serializes them, reads the keychain on every call (a sign-out elsewhere must stop the token)
  and re-reads it before signing out. A sign-in saves through `ApiTokenStore::save_new_tokens`
  (a fresh item), a refresh through `update_tokens` (the existing item, never a new one).
- Read `dev-loopback-http` only in `github/host.rs`'s `PLAIN_HTTP` (`DEV_ALLOWANCE` exposes it),
  and enable it only from `[dev-dependencies]`; `scripts/check.sh` fails a normal app or CLI build
  that carries it. A test hook (the CLI's `RC_GIT_SETTINGS`) is honored only when `DEV_ALLOWANCE`.
- GraphQL queries are `&'static str`; every value goes in `variables`. Test each GitHub call
  against testkit's `GitHubServer`, which answers only the exact arguments a call must send.
- Pass every server string through `single_line` (`multi_line` for a description) and every
  server URL through `links::server_link` before it leaves the guard: no raw server text out.
- Resolve an ssh alias only through `ssh::dialed_host` (the transport's resolver), and compare its
  `HostName` with an account host exactly; never write a second ssh-config reader.
- Send a GitHub mutation only through `Session::mutate`, which takes the `Confirmation` and
  builds the request from its action (`github/mutation.rs`); reads stay on `Session::graphql`.
- Before a mutation, re-read the pull request and compare it with the stamp and the answered
  decision (`github/merge.rs`); any difference refuses before a byte is sent.
- Never resend a mutation at any layer: send it once, and settle a lost answer with one read.
- A mutation's outcome is three-valued (merged, refused, unknown); never report a lost answer as
  "not merged" unless a read proves it.
- Read a mutation answer's `data` before its errors and headers; call it refused only with empty
  data and errors that each prove it never ran (`refuses` in `github/merge.rs`).
- Report a merge GitHub names on another base than the stamp's as `MergedIntoOtherBase`, never
  `Merged`; name a base only when GitHub's answer or the settling read did.
- Build a merge challenge as labelled lines, facts first and server free text last, each value
  through `merge.rs`'s `fact`, so no server text starts a line or reads as a fact.
- Build an admin merge only through `PullRequestAdminMerge::new` (`admin_merge_blocker`), never by
  reaching the plain merge's `read`; its phrase is the number, and its skipped list is part of it.
- Never word what GitHub's bypass covers beyond what the read reports: say the list may not be
  complete, and keep the two unverified API behaviours in `docs/safety.md` unasserted.
- Build a later pull request action on `github/guarded.rs` (`plan_action`, `proof_for`): the
  merge's re-read, stamp check and answered-decision check; give it an `ActionBlocker` rule.
- Send a REST mutation only as a `MutationRequest::Rest` of `Mutation::of` (plain segments and
  integer ids through `rest_url`); the outbox sends each request once, never a free-standing POST.
- Re-run only runs `failed_runs` names (positive ids, at most `MAX_RERUN_RUNS`); never call the
  endpoint that approves fork or first-time contributor runs, and never offer one.
- Act on a check's Actions id only once GitHub's REST read of it matches the stamp's head
  (`match_runs` in `rerun.rs`, `matched_job` in `logs.rs`); skip an unmatched run with `skip_next`.
- Follow a redirect only in `read_job_log`: `Session::job_log_location` alone calls
  `send_reading_redirect`, and the hop goes as a token-less `DownloadRequest` to a `LogDownloadUrl`.
- Never put the download address in a message: name `LogDownloadUrl::shown`, scrub its `secrets`.
- Send the job log's second storage request only after a `416` refusing the range, from
  `download_log`; never a third (`check.sh` counts the `download` calls).
- Delete a pull request's head branch only through `delete_head_branch`: `delete_remote`'s rule,
  the backup ref written and read back, then one `updateRefs` whose `beforeOid` is the backed-up
  commit; never a delete that names no expected commit (`deleteRef`, REST `DELETE`).
- After the backup ref is written, a cancel or an unsent request is `ActionRefusal::BackupKept`
  (it names the ref), never `Cancelled` or `NotSent`.
- Refuse a branch name `typeable` rejects (a challenge would show it changed); never accept an
  escape-form phrase.
- Say in an auto-merge challenge that later commits merge without asking; assert nothing about
  when GitHub cancels auto-merge beyond what `docs/safety.md`, "Auto-merge", cites.
- Keep turning auto-merge off decided on the number, account and auto-merge request alone
  (`AutoMergeDisable`, `changed_for`): a title, head or base in it lets an author block it.
- Never state a merge method as what GitHub will use: a merge queue overrides it, and rc-git does
  not read whether there is one.
- Fetch a pull request head only through `fetch_pull_request_head` (`actions/remote/pull_head.rs`):
  its refspec comes from `pull_head::refspec(number)` alone, never from server text, it fetches
  no tags, and `run_fetch` refuses before `receive` when the advertised head is not the stamp's.
  Give a new fetch variant a `fetch::Wanted` arm rather than a second transport path; the PR
  remote resolves through `pull_request_upstream`, as the reads do.
- Resolve a branch's remote and tracking ref only through `configured_remote_name` and
  `tracking_ref_name` in `actions/remote.rs`, never gix's `branch_remote*` helpers directly
  (clippy-banned in `clippy.toml`; those two wrappers carry the only `#[expect]`):
  those return `None` for a branch with no `[branch]` config and skip the `origin` fallback,
  so status reads 0/0 while push still works. Test a new remote action on an `untrack`ed
  clone (`docs/solutions/integration-issues/tracking-ref-skips-origin-fallback-without-branch-merge.md`).
