---
title: "russh-config diverges from OpenSSH: ssh-config aliases silently fail to resolve"
date: 2026-09-19
category: integration-issues
module: rc-git-guard
problem_type: integration_issue
component: infrastructure
severity: high
symptoms:
  - "push 'github.com-alias': failed to lookup address information: nodename nor servname provided, or not known"
  - "ssh-config HostName, User, and Port silently ignored; the URL's alias host is dialed raw"
  - "A mixed-case Host block never matches even though plain ssh resolves it fine"
  - "Tab-separated 'Key<TAB>value' config lines behave as if absent"
  - "A config with global options before the first Host block fails to parse entirely"
  - "'HostName = real.example' resolves to the host '= real.example'; an alias kept in an Included file is dialed raw"
  - "A Match block's User or IdentityFile applies to the Host block above it"
root_cause: wrong_api
resolution_type: code_fix
tags:
  - russh-config
  - russh
  - ssh-config
  - host-alias
  - gix
  - ssh
---

# russh-config diverges from OpenSSH: ssh-config aliases silently fail to resolve

## Problem

rc-git's pure-Rust ssh transport resolved OpenSSH host aliases wrong: a multi-account alias remote (`Host github.com-work` → `HostName github.com` in `~/.ssh/config`) was dialed literally instead of through its `HostName`, so every fetch/push against an alias remote failed DNS.

## Symptoms

- App dialog / CLI error: `push 'github.com-personalgithub': failed to lookup address information: nodename nor servname provided, or not known` — the alias, lowercased, treated as a DNS name.
- Plain `ssh github.com-PersonalGitHub` and `git push` work fine on the same machine with the same config, so the config itself is known-good.
- Remotes with a real hostname (`ssh://git@github.com/...`) work; only aliased remotes fail.

## What Didn't Work

The first fix (commit 538ed2b) switched `target.rs` from `Config::host_name`/`Config::user`/`Config::port` to the `host_config` fields — necessary, but the field failure persisted with the identical error. Reason: the user's block was `Host github.com-PersonalGitHub` (mixed case), the URL host arrives lowercased from gix, and russh-config matches `Host` patterns with a case-sensitive glob — so the block never matched and there was no `host_config` to read.

## Solution

russh-config 0.58.0 diverges from OpenSSH (`ssh_config(5)`) in the ways below; rc-git handles each in `crates/rc-git-guard/src/actions/remote/ssh/target.rs`, where `ConfigReader` rewrites the file into the subset the parser reads before it parses:

1. **The parsed values live in `host_config`, not the top-level fields.** `russh_config::parse` (russh-config 0.58.0, its `lib.rs:312-321` — a dependency source path, not this repo) returns `Config { host_name: <queried host verbatim>, user: None, port: None, host_config }`. Only `host_config.hostname/user/port/identity_file` carry what the file said; the crate's own `Config::host()` accessor (its `lib.rs:164-169`) falls back from `host_config.hostname` to `host_name`. Reading `Config::host_name` returns the alias every time. Fix: `resolve()` maps straight to `host_config` and takes `hostname`/`user`/`port`/`identity_file` from it.

2. **`Host` matching sees a host gix already lowercased.** OpenSSH (checked on OpenSSH_10.3p1) matches `Host` patterns case-sensitively against the host as typed and lowercases it only afterwards, so `Host GitHub.com-Work` matches `GitHub.com-Work` but not `github.com-work`. gix-url has already lowercased the URL's host, so the spelling ssh would match is gone, and russh-config compiles patterns with `globset::Glob` verbatim. Fix: lowercase `Host` pattern lines and the query before parsing — a deliberate superset of OpenSSH's matching. Values on every other line keep their case because `IdentityFile` paths are case-sensitive on disk. Matching exactly would need the raw host spelling from `.git/config`; do that only if the superset ever picks a different block than ssh does (two blocks that differ only in case).

3. **Only a single space separates key and value.** The parser splits each line with `splitn(2, ' ')`: a legal `Key<TAB>value` line yields one token and is dropped, `HostName=real.example` is dropped, and `HostName = real.example` resolves to `= real.example`. Fix: `split_directive` splits the way OpenSSH does — whitespace with at most one `=` — and writes `key value`.

4. **Legal global options before the first `Host` block abort the parse.** Options preceding any `Host` header make `parse_ssh_config` return `Err(Error::HostNotFound)` for the whole file. Fix: on that error, retry once with a synthetic `Host *\n` prefix, which gives the leading globals the wildcard scope OpenSSH effectively gives them (`parse_for_host`).

5. **`Include` is dropped.** Unknown keys go to `debug!`, so an alias kept in an included file (`config.d/*`, the line OrbStack and Colima add) is dialed raw. Fix: `ConfigReader::include` splices the files in place — relative paths under the ssh directory, globs expanded in sorted order, missing files skipped, at most 16 levels deep as in OpenSSH — skips an `Include` inside a block that does not apply, and writes the including block's header again when an included file opened a block of its own. It only reads.

6. **`Match` is dropped and its directives leak into the block above.** `Host github.com-work` followed by `Match host *.internal` / `User admin` resolved the alias as `admin`. Fix: every `Match` line becomes `host !*`, a block that never matches; a `Match` that may select the host (`all`, `exec`, or a `host`/`originalhost` criterion that holds) is refused with a named error instead, since rc-git does not evaluate it.

7. **Scalar directives overwrite within a block.** OpenSSH keeps the first value it obtains; russh-config keeps the last one within a block (it keeps the first across blocks). Fix: `ConfigReader::directive` drops later duplicates of `User`, `HostName`, `Port`, `ProxyJump`, `ProxyCommand`, `UserKnownHostsFile`, `StrictHostKeyChecking`, `AddKeysToAgent`, `IdentitiesOnly`, `HostKeyAlias` and `IdentityAgent`; `IdentityFile` stays cumulative.

8. **`%` tokens stay unexpanded.** russh-config expands tokens only for its own `ProxyCommand` stream. Fix: `HostName` expands `%h` and `%%`; `IdentityFile` and `IdentityAgent` expand `~`, `%d`, `%u`, `%h`, `%r` and `%%`; any other token is refused by name.

9. **`IdentityAgent`, `ProxyJump` and `ProxyCommand` are not acted on.** russh's `Agent::connect_env` reads only `SSH_AUTH_SOCK`, and the proxy fields are parsed but never used, so a host behind a bastion timed out. Fix: `SshTarget::identity_agent` carries `none`, `SSH_AUTH_SOCK`, `$VAR` or a path, and `auth.rs` connects to that socket (none at all for `none`); a host whose config sets `ProxyJump` or `ProxyCommand` (other than `none`) is refused before dialing, because rc-git cannot tunnel and never spawns.

## Why This Works

russh-config is a lightweight reimplementation of a well-specified format, and its API surface reads like it does more than it does: `Config::host_name` looks like the resolved host but is the echo of the query. Rewriting the input into the subset the parser handles (single-space separators, lowercase patterns, explicit `Host *` scope, includes spliced, `Match` neutralized, first value kept) and reading the fields the parser actually fills (`host_config`) resolves the directives rc-git consumes — `HostName`, `User`, `Port`, `IdentityFile`, `IdentitiesOnly`, `HostKeyAlias`, `IdentityAgent`, `UserKnownHostsFile` — with URL values still winning where the URL specifies them.

It is not OpenSSH parity. Handled: `Key=Value` and tab separators, `Include` (with globs, depth 16), `Match` (neutralized, or refused when it may apply), first-value-wins, the tokens above, `IdentityAgent`. Refused by name: `ProxyJump`, `ProxyCommand`, a `Match` that may apply, an unknown `%` token. Still unsupported: `Host` matching is the case-folded superset described in 2; `Match` criteria other than `all`, `exec`, `host` and `originalhost` are not evaluated; `${VAR}` expansion; `CanonicalizeHostname`; and every directive rc-git does not read.

One consequence reaches beyond `target.rs`: `known_hosts` must be looked up by the **dialed** name. OpenSSH records entries under the `HostName` it connects to, not the alias, so `HostKeyHandler` keys `check_known_hosts_path` on `target.host_name` (`hostkey.rs:41-51`, `hostkey.rs:90-95`). Looking up by the alias would raise a spurious trust-on-first-use prompt for a host whose key is already known.

## Prevention

- Test config parsing with the inputs real users have, not the happy path: mixed-case aliases, tab indentation, `Key=Value`, global options above the first `Host` block, includes, `Match` blocks. Unit tests pin each divergence in `target.rs`'s tests module — `config_host_name_user_port_and_identity_are_honored`, `mixed_case_host_patterns_match_the_lowercased_url_host`, `tab_separated_key_value_lines_are_honored`, `equals_separated_key_value_lines_are_honored`, `global_options_before_the_first_host_block_parse`, the `include_*` tests, `match_block_directives_do_not_leak_into_the_host_block_above`, `match_block_that_may_apply_is_refused`, `duplicate_keys_in_a_block_resolve_to_the_first_value`, `host_name_expands_percent_h`, `identity_file_tokens_expand_and_unknown_ones_are_refused`, the `identity_agent_*` tests and `proxy_jump_and_proxy_command_are_refused` — and the end-to-end alias round trip is `ssh_config_alias_resolves_hostname_user_and_port` in `crates/rc-git-guard/tests/remote.rs` (alias URL → `HostName 127.0.0.1` + `Port` + `User` from a config file → fetch over the loopback ssh server).
- When wrapping a "lightweight" reimplementation of a specified format, diff its behavior against the spec (`ssh_config(5)`) for the constructs the spec allows, before trusting field reports to find the gaps one at a time. Each divergence here was invisible until a config used it.
- Prefer verifying against the developer's real environment when possible: the confirming test was `cargo run -p rc-git-cli -- fetch .` on a repo whose `origin` uses the mixed-case alias.

## Related Issues

- [Push over HTTPS with gix that has no push: hand-rolled send-pack](../architecture-patterns/hand-rolled-git-push-over-gix-https.md) — the same remote subsystem; the ssh transport plugs into the transport seam that doc describes.
