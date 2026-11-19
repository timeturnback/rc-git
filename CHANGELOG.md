# Changelog

What changed in rc-git, newest first. Help ▸ Changelog shows this file in the app.

## 2026-11-19

### Added

- A pull request's tab can turn on GitHub's auto-merge when branch protection still blocks it:
  pick a method and click Enable Auto-Merge. The confirmation says in its own line that GitHub
  will then merge the pull request by itself once its requirements are met, including commits
  pushed to its head branch after you confirm, without asking you again; when the head branch
  lives in someone else's repository, it names that owner, who can push more commits. While
  auto-merge is on, the tab says so, with the method and who turned it on, and Disable Auto-Merge
  turns it off. From Terminal: `rc-git pr auto-merge <n> --enable --method squash --head <oid>
  --base <branch> --yes`, or `--disable` in place of `--enable --method …`.
- Re-run Failed Jobs on a pull request's tab asks GitHub to re-run the failed jobs of the head's
  GitHub Actions runs (up to ten runs at a time). The confirmation lists each run with its failed
  checks and what it leaves out, and the note afterwards says how each run's request ended.
  From Terminal: `rc-git pr rerun <n> --head <oid> --base <branch> --yes`. rc-git never approves
  workflow runs from forks or first-time contributors.
- View Log on a finished GitHub Actions check of a pull request's tab shows that job's log in
  place of the description, as plain text: colours dropped, invisible and direction-changing
  characters shown as escapes, and nothing in it clickable. RCGit first checks with GitHub that
  the job ran on the head commit the tab shows, and shows nothing for a job it cannot match.
  Only the end of a long log is downloaded (about its last 2 MB), and the tab says when part of
  it is left out. Stop cancels a download; Close Log puts the description back. From Terminal:
  `rc-git pr logs <n> --check <name>` or `--job <id>`.

### Changed

- Re-run Failed Jobs now asks GitHub about each run first and re-runs only the runs that ran on
  the head commit you confirmed. A run that does not match is not re-run, and the note (and
  `rc-git pr rerun`, as `not-matched`) says which.

- The Merge confirmation lists what it is about to merge one fact per line, always in the same
  order: the repository and its host, the pull request's number, the GitHub account that
  merges, the base branch, the head commit and its branch, the method, the checks, and the
  title last. A title or branch name can no longer start a line of its own or look like one of
  those facts: line breaks, quote marks, invisible characters and unusual spaces in them show
  as escapes such as `\u{a0}`. When GitHub says your account could merge past branch protection,
  the confirmation also says that GitHub will not stop this merge if a requirement starts
  blocking before it runs, and a change to that since you opened the confirmation sends
  nothing.
- `rc-git pr merge` now needs `--base <branch>` beside `--head`, both as `rc-git pr view` prints
  them; a pull request moved to another base since you looked sends nothing. Without `--yes` it
  prints the confirmation one `would-merge` line per fact, and with it the same lines go to
  stderr before the request is sent. `merged` lines end with the base GitHub merged into.
- `rc-git pr merge --admin` prints the plan's fingerprint (`plan<TAB>…`, 32 hex digits) when
  run without `--confirm`, and the real run needs it: `--confirm <n> --plan <fingerprint>`. If anything in
  the plan changed since (a check, the title, what would be skipped), nothing is sent and it
  exits 11.
- A pull request's Changes view, and `rc-git pr diff`, compare its head with the commit GitHub
  reports its base branch at, not your local copy of that branch. When that commit is not in
  your repository yet, the view asks you to fetch instead of showing changes against another
  commit; `pr diff` prints `base<TAB>merge base<TAB>pull-request-base<TAB>commit`. Only a GitHub
  Enterprise Server that does not report the base commit is compared with your local copy,
  and the view and `pr diff` (`local-copy`) say so.
- `rc-git` quotes a path that holds a bidirectional, invisible or other hidden character, or a
  space other than the ordinary one, printing that character's bytes in octal, so a file name
  cannot hide its real name in a terminal. `rc-git pr view` prints a tab inside a description
  line as `\u{9}`. Titles, branch names and check names show unusual spaces and invisible
  characters as escapes too.

### Fixed

- A merge that GitHub says landed on another base branch than the one you confirmed (the pull
  request was moved to another base on GitHub after RCGit last read it) is no longer reported
  as merged into the base you confirmed: the tab shows a warning naming both branches, the log
  marks it, and `rc-git pr merge` prints `merged-into-other-base` and exits 13. When GitHub's
  answer does not name the base, the note no longer names one.
- A merge GitHub reported as done is no longer called refused when its answer also said the
  rate limit was spent; and an error RCGit cannot place no longer reads as "nothing was merged":
  RCGit reads the pull request again and tells you where it stands.
- After the window switched to another repository or GitHub account, a merge still running for
  the old one is logged under its own repository and never shows in the new one's tab of the
  same number, and a read still running for the old one's tab never lands in the new one's.
- A second Ctrl+C once `rc-git pr merge` has sent its request no longer ends the command as if
  it stopped between steps: it reads and prints the outcome first.

## 2026-11-18

### Added

- When branch protection blocks a pull request and GitHub says your account may merge past it,
  its tab shows a separate Merge Without Waiting for Requirements (Admin) control under the
  greyed-out Merge. It skips rules your team set for the base branch on purpose, so RCGit reads
  the pull request again first, lists what it knows will be skipped (required checks that failed
  or have not finished, a review that is still required or asked for changes, a branch that is
  out of date), says other rules GitHub does not report may be skipped too, and merges only
  once you type the pull request's number. If anything moved since you looked, including a
  check, nothing is sent. A pull request that can merge normally, or one GitHub does not let
  your account merge past protection, has no such control. Every attempt and outcome is written
  to the app's log. In Terminal, `rc-git pr merge <n> --method squash --head <oid> --admin
  --confirm <n>` does the same; without `--confirm` it prints what would be skipped and sends
  nothing, and `--yes` is not enough.

- A pull request's tab can merge it on GitHub: pick Squash and Merge, Create a Merge Commit or
  Rebase and Merge (only the ones the repository allows), then Merge. RCGit reads the pull
  request again first and asks you to confirm what it is about to merge: the repository and
  number, the title, both branches, the head commit, the method and how the checks stand. If
  anything moved since you looked, nothing is sent and the tab reads it again. Merge stays
  greyed out, with the reason, for a draft, a conflict, a closed pull request, branch
  protection that blocks it, or a signed-out account. Cancel works until the request is sent;
  after that the tab says it cannot be stopped. When GitHub's answer never arrives, RCGit
  never sends the merge again: it reads the pull request once more and tells you whether it
  merged, or that it cannot tell and you should look on GitHub. Your local repository is
  unchanged until you fetch. In Terminal, `rc-git pr merge <n> --method squash --head <oid>
  --yes` does the same, with `--head` set to the head `rc-git pr view` printed.

- A pull request's tab has a Changes view: the files the pull request changes since it left
  its base branch, as GitHub counts them, and the diff of the file you pick, read from your own
  repository. When its commits are not in your repository yet, Fetch Changes downloads them
  from the remote, with the credentials you saved for Fetch and never your GitHub token, into
  `refs/rc-git/pr/<number>/head`, a ref that never shows as a branch and is never checked out;
  Stop stays on screen while it runs. If the pull request moved after you opened it, nothing is
  downloaded and RCGit reads it again. When your copy of the base branch is missing, it says to
  fetch first rather than guess. In Terminal, `rc-git pr diff <n>` lists the same files (add
  `--fetch` to download them), and `rc-git merge-base` prints where two branches meet.

- The sidebar has a Pull Requests section for a repository on GitHub: the open pull requests
  of its remote, newest activity first, each with its number, title, author, branches and a
  check icon (passing, failing, running). Click one to open it in a tab on the page: its state,
  review, conflicts, every check with required ones first and a Details button naming the site
  it opens, and the description. Links in a description open in your browser only when you
  click them, and only web links; images show as their name. The list checks again every minute
  and an open pull request every 15 seconds while a check runs, only while the window is in
  front. When a check fails to load the section says "Couldn't check" and why rather than
  showing old results as current. With no account for github.com it offers to open Settings,
  and with several accounts on one host it asks which one this repository uses. A remote named
  through an alias in your ssh config (`git@github-work:…`) is found through the host the alias
  stands for. In Terminal, `rc-git pr list`, `pr view <n>` and `pr checks <n>` show the same.

- Settings has a GitHub Accounts list: Add Account… signs in to github.com or a GitHub
  Enterprise host through a GitHub App or an OAuth App (you get a code to enter on the host's
  own login/device page, and Cancel stays on screen while RCGit waits), or with a pasted
  personal access token. Before you sign in it says what each method may do, and after it shows
  the account, its scopes and when the token expires. Sign Out forgets the account and its
  token. Tokens stay in your login keychain and go only to the host you signed into. In
  Terminal, `rc-git auth login`, `status` and `logout` do the same with the same accounts.

- Files show an icon for their type, as VS Code draws them: TypeScript, Swift, Python,
  Markdown, JSON, Dockerfiles, Rust and a few hundred more, most in a color of their own, in the
  file tree, Go to File and Find in Files, drawn at the size VS Code draws them. A file deleted
  from disk keeps its icon dimmed, and a file of an unknown type keeps the plain file icon.

### Changed

- Window tabs are rounded pills with a repository icon: the current tab is filled, the others
  stay quiet until you point at them, and a tab's close button shows on hover. Tabs keep a
  fixed width from the left, with the + button right after the last one, and shrink only when
  the window runs out of room.
- The file, History and Changes panes sit on a rounded page inset from the window's edge, with
  the toolbar, tabs and sidebar on one shared background and no lines between them.
- The sidebar is open whenever a repository opens, even if you hid it earlier with View ▸
  Toggle Sidebar (⌃⌘S). A window too narrow for it opens the sidebar once it is wide enough.
- In the sidebar's ref list, a long branch or tag name ends in "…" instead of running under the
  scrollbar, and rows are taller and inset like the filter field above them, so the selected
  row no longer touches the sidebar's edges. Branches show a green icon, remote branches a blue
  one and tags a yellow one, here, in the branch picker (⌘B) and on the banner.
- In History, a commit you click opens across the whole window under the commit list, taking
  most of its height; drag the divider to resize it, or hide it with the close button on its
  right. Its Commit tab shows the message, author, hash and parents with the changed files on
  the right, and clicking a file opens it in the Changes tab: the files on the left, as a list
  or a tree, beside the diff. File Tree shows the commit's files in the Files pane. Files in
  both tabs show their type icon.

### Fixed

- When a GitHub sign-in stops partway (the host declines or expires the code, or the token
  check fails), you can start again from the same sign-in sheet; it no longer says another
  sign-in is still waiting for its code.
- Signing in to GitHub always puts the token in a new keychain item of RCGit's own, so an item
  another program left under RCGit's name never receives it.

## 2026-11-17

### Added

- Help ▸ Changelog opens this list of changes. It shows the changelog of the build you are
  running, so a rebuild brings its new entries with it.

### Changed

- New colors in light and dark: quiet near-neutral surfaces, faint dividers, and an indigo
  accent on focus rings, links and the main button. Lists drop the alternating row shading,
  and hovered and selected rows take a soft neutral fill.
- Rows are taller and icons a little larger, captions are easier to read, and the quick panels
  and the branch picker have rounder corners.
- The toolbar shares the sidebar's color, and Files, History and Changes sit in one switch with
  the current mode filled in. The branch name at the top of the sidebar no longer sits in a
  gray box; it highlights when you point at it.
- History commit titles are larger, with the hash, author and date in a quieter tone, and a
  commit row highlights under the pointer. Sidebar section headings, folder and branch icons
  take quieter tones too. In Changes the tick boxes are gray instead of blue, and each file's
  status letter is a small colored mark.
- Go to File, Find in Repository, Open Recent and the branch picker (⌘B) share one look: a
  large borderless search field over a thin divider, and roomier result rows.
- Added and removed lines in a diff carry a stronger tint along the line numbers, so changes
  stand out at the edge. Source code uses new syntax colors: strings green, functions blue,
  types cyan, comments in a quiet gray.
- The markdown preview has more room around headings and blocks, and its headings are no
  longer bold. Commit titles, window titles and empty-pane messages take a lighter weight or a
  quieter tone, and Blame drops its striped rows.

## 2026-11-12

### Added

- View ▸ Wrap Lines wraps long lines in every diff and in the file viewer. Turn it off and
  long lines scroll sideways instead, with a scrollbar on each axis. The top line stays in view
  when you switch, and the choice is remembered. The path bar has the same toggle.
- View ▸ Expand Diff shows the History or Changes diff across the whole window. Use it again,
  or the path bar toggle, to bring the panes back.
- The History commit list has a header with author and date columns you can drag wider or
  narrower. The date column opens fitted to the widest date, and a width you drag is kept.
- Every long list draws a scrollbar, and the scrollbars stay visible.
- Each repository window shows its tabs in a strip under the toolbar: equal-width tabs, a close
  button on each, and a + that opens another repository.
- The diverged sheet has Open in Terminal. It copies the rebase command and opens Terminal in
  the checkout.
- A stash row shows its commit id when you hover over it.

### Changed

- rc-git is rebuilt on a new drawing engine. Files, History, Changes, blame, the markdown
  preview, Go to File, Find in Repository, Open Recent, Settings, sync, About and Rebuild and
  Relaunch all came across, and tabs, recents and the window frame carry over from the old
  app.

### Fixed

- A repository opened from a window joins that window as a tab instead of opening a window of
  its own.
- A window you close no longer comes back as a tab at the next launch.
- Shortcuts for commands that act on every window, such as Show Hidden Files, take effect.
- Dragging a History column wide stops at the right place when the graph column is hidden.

## 2026-11-03

### Added

- Control-R opens the recent repository picker.
- The Find in Repository panel has a close button.

## 2026-10-29

### Added

- ⇧⌘F on the Files tab finds text in every file of the repository.

## 2026-10-25

### Added

- ⌘R opens a picker of recent repositories. Type to filter it.

## 2026-10-15

### Added

- Settings lists the ssh hosts you approved, with Remove.
- A banner names the saved tabs that did not reopen at launch.
- A large markdown file shows a placeholder while its preview builds.
- History shows the committer time while you filter by date.

### Fixed

- A markdown preview no longer flashes the previous file's content when you switch files
  quickly, and emphasis, links and block quotes render the way CommonMark does.
- A linked worktree's Changes refresh when its exclude file changes, and a repository opened
  through its `.git` folder is named after its working folder.
- rc-git refuses to delete a tag that moved since you looked at it, and refuses stash actions
  when the stash list and its log disagree.
- Push, pull and sync report a rejected push, a credential redirect to another host and a
  local upstream branch correctly.
- Blame marks lines at a shallow clone's boundary, and diffs show a path that changes between
  a file and a submodule.

## 2026-10-14

### Fixed

- Go to File shortens long rows in the middle, shows listing failures in the panel, and no
  longer reopens the open file on a double-click.
- An empty tree says why it is empty instead of asking you to open a repository.
- Sheets with a text field put the cursor in it.
- Branch, tag, stash, discard and switch actions check every path before they write, refuse a
  name that already exists, and say how far a failed step got.

## 2026-10-13

### Fixed

- History: the ref filter takes arrows and Return, unreadable refs are listed instead of
  dropped, date filters cover whole local days, search stays within the file you are looking
  at, and graph lanes stay visible on the selected row.
- Changes: the list stays on screen while it rescans, long rows keep their status letter, a
  click on any row of a hunk discards that hunk, and the commit box no longer turns quotes and
  dashes into typographic ones.
- The Edit menu has Undo, Redo, Cut and Paste.
- History of This File no longer shares a shortcut with Hide Others.
- Tabs on a disk that is not mounted at launch stay in the tab list, and the same repository
  never opens in two tabs.
- Closing a tab cancels its running sync.
- Sheet buttons stay disabled until their fields are filled.
- Markdown previews follow CommonMark more closely: lists, quotes, fenced code, HTML blocks,
  bare URLs and heading anchors. A link to another app asks before it opens.
- Syntax colors for Swift comments and keywords, booleans, and markdown table cells.
- ssh remotes read your ssh config the way OpenSSH does (HostName, Include, IdentityAgent,
  HostKeyAlias, quoted values) and check every known_hosts file. A changed key for a host you
  approved is refused.
- Rebuild and Relaunch can be cancelled, reports a build killed by a signal, and keeps your
  tabs when the relaunch fails.
- Credentials are stored in rc-git's own keychain items and read once per sync.

## 2026-10-12

### Fixed

- Push goes to the push URL and remote git would use, keeps hook output, and counts a push as
  done even when the tracking ref could not be updated.
- Sync can publish a branch the remote does not have yet, and fetch works on a detached HEAD.
- Cancel ends a stuck HTTPS or ssh fetch or push.
- An ssh connection that times out names the step that timed out.

## 2026-10-08

### Fixed

- Blame follows renames, and stops at a size limit instead of reading a huge file.
- Diff line numbers for an empty side match git's.
- rc-git refuses to delete a branch that moved, or one checked out in another worktree.
- Stash apply and pop act on the entry you picked, not on whatever sits at its position.
- Commit, discard and switch refuse paths inside a submodule, special files, and changes made
  during a merge.

## 2026-10-07

### Fixed

- The working tree status matches git's: the exec bit, files inside nested repositories,
  checked-out submodules, a folder that replaced a file, and names that differ only in case or
  Unicode form.
- Untracked files show on a branch with no commits yet, and your global ignore file applies.
- File history and blame work in shallow clones, and a wrapped commit title reads as one line.
- Annotated tags show their commit in History and blame.
- rc-git refuses paths through a symbolic link, `.git` paths, and names that differ from an
  existing one only in case.
- Repositories under `/tmp` or reached through a symbolic link refresh when their files change.

## 2026-10-06

### Added

- ⌥⌘R rebuilds and relaunches without opening the update window first.
- ⇧⌘T reopens the last tab you closed.

### Changed

- In History, the commit list sits over the open commit's details and the picked file's diff
  fills the right pane. Changed files show as a folder tree by default, with a switch to a
  flat list.

### Fixed

- A tab you close with ⌘W does not come back at the next launch.

## 2026-09-30

### Added

- Back and forward step through the files, commits and tabs you visited.
- TypeScript files are highlighted, and you can follow an import to the file it names.

### Fixed

- The file pane empties when you step back to a file that no longer exists.

## 2026-09-29

### Added

- The ref list opens from a popover, and the sidebar has one native look.
- When a sync finds the branch has diverged, rc-git hands over the `git pull --rebase` command
  to run yourself.

### Fixed

- Ahead and behind counts compare against origin when a branch has no upstream.

## 2026-09-28

### Added

- Files and Changes show a banner naming the branch, commit or ref you browse. Click it, or
  press ⌘B, to pick another.
- View ▸ Show Hidden Files (⇧⌘.) shows the clutter rc-git hides by default, such as
  `.DS_Store`. Repositories with a checkout open on the working tree.

### Fixed

- Code blocks in the markdown preview no longer collapse to one character a line.

## 2026-09-27

### Added

- The markdown preview shows tables, task lists, footnotes and images, and its links work: a
  link to a file in the repository opens that file.
- Repository ▸ Back to Default switches back to the default branch.

## 2026-09-26

### Fixed

- The change list no longer rescans once for every file event.

## 2026-09-25

### Added

- Closed tabs stay in the list a launch can restore.

### Fixed

- A stuck sync can be cancelled, and it times out on its own.
- The history drawer keeps its layout when you drag its divider.

## 2026-09-21

### Changed

- Discarding working changes no longer asks you to type a phrase.

### Fixed

- The rebuild check no longer spins forever.
- The history drawer's diff pane no longer shrinks to nothing.

## 2026-09-20

### Added

- The change list groups files by folder.
- Each mode's navigation sits in one left panel, and the tree pane fits its rows.
- The window title shows the build's version.
- The app rebuilds and relaunches itself from its own menu.

### Fixed

- Tabs are saved before a rebuild relaunches the app.

## 2026-09-19

### Added

- The file viewer is faster and highlights only what is on screen, and markdown files have a
  preview.
- The Changes tab has Fetch, Pull and Push buttons.

### Fixed

- ssh config Host patterns match regardless of case.

## 2026-09-18

### Added

- ssh remotes: rc-git asks before trusting a new host key, asks for a key's passphrase, and
  keeps credentials per kind.

### Fixed

- ssh config HostName, User and Port apply to alias remotes.

## 2026-09-17

### Added

- Fetch, pull, push and sync over ssh and HTTPS, from the Repository menu and the toolbar's
  Sync button. rc-git asks for credentials when a remote needs them.

## 2026-09-11

### Added

- History shows the commit's details in a drawer on the right, and the commit list takes the
  full height.

## 2026-09-10

### Added

- Each repository opens in a native window tab, and the tabs come back after a relaunch.

## 2026-09-08

### Added

- The Changes tab: see what changed in the working tree, commit it, stash it or discard it.

## 2026-09-07

### Added

- rc-git: browse a repository's branches, tags, files and history, read files, and diff and
  blame them.
- History has a lane graph, a commit search by message, author and date, history of a single
  file, and commit details with body, committer, refs and parent links.

### Fixed

- The History commit list no longer collapses to zero height.
