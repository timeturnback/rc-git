# Agent notes

`CLAUDE.md` is the entry point for any coding agent working here; area rules live in
`.claude/rules/` and the long-form references in `docs/`. Run `scripts/check.sh` before handing
work back.

## Changelog

- `CHANGELOG.md` is what Help ▸ Changelog shows. A change a user would notice is a `feat` or
  `fix` commit and adds an entry. A change no user notices takes another type (`refactor`,
  `test`, `docs`, `build`, `chore`), since every `feat` and `fix` must carry an entry.
- Put the entry under today's `## YYYY-MM-DD` heading (newest first; add the heading if
  missing), in `### Added`, `### Changed` or `### Fixed`, in that order.
- Write for the person using the app: what they can do now or what stopped going wrong, with
  menu names and shortcuts. Never paste the commit subject.
- The entry lands in the same commit as the change. `scripts/changelog-check.sh` refuses a
  `feat`/`fix` commit that leaves the file alone (pre-push hook and CI), and the
  `rc-git-kit` test `the_shipped_changelog_keeps_its_shape` fails on a malformed file.
