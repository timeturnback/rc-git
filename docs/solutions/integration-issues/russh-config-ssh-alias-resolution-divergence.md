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

russh-config 0.58.0 diverges from OpenSSH (`ssh_config(5)`) in four ways; rc-git handles all four in `crates/rc-git-guard/src/actions/remote/ssh/target.rs`:

1. **The parsed values live in `host_config`, not the top-level fields.** `russh_config::parse` (russh-config 0.58.0, its `lib.rs:312-321` — a dependency source path, not this repo) returns `Config { host_name: <queried host verbatim>, user: None, port: None, host_config }`. Only `host_config.hostname/user/port/identity_file` carry what the file said; the crate's own `Config::host()` accessor (its `lib.rs:164-169`) falls back from `host_config.hostname` to `host_name`. Reading `Config::host_name` returns the alias every time. Fix: `resolve()` maps straight to `host_config` (`target.rs:53-55`) and takes `hostname`/`user`/`port`/`identity_file` from it.

2. **`Host` matching is case-sensitive; OpenSSH's is not.** Patterns are compiled with `globset::Glob` verbatim, while OpenSSH lowercases host names before matching — and gix-url has already lowercased the URL's host. A mixed-case alias block can never match the query. Fix: lowercase `Host` pattern lines and the query before parsing; values on every other line keep their case because `IdentityFile` paths are case-sensitive on disk (`normalize_config`, `target.rs:177-194`).

3. **Tab separators are silently skipped.** The parser splits each line with `splitn(2, ' ')`; a legal `Key<TAB>value` line yields one token and is dropped without error. Fix: tabs → spaces per line (`target.rs:180`).

4. **Legal global options before the first `Host` block abort the parse.** Options preceding any `Host` header make `parse_ssh_config` return `Err(Error::HostNotFound)` for the whole file. Fix: on that error, retry once with a synthetic `Host *\n` prefix, which gives the leading globals the wildcard scope OpenSSH effectively gives them (`load_config`, `target.rs:163-169`).

The normalization and retry, as shipped:

```rust
let contents = std::fs::read_to_string(&path).map_err(|source| parse_err(&source))?;
let contents = normalize_config(&contents);
let host = host.to_lowercase();
match russh_config::parse(&contents, &host) {
    Ok(config) => Ok(Some(config)),
    Err(russh_config::Error::HostNotFound) => {
        russh_config::parse(&format!("Host *\n{contents}"), &host)
            .map(Some)
            .map_err(|source| parse_err(&source))
    }
    Err(source) => Err(parse_err(&source)),
}
```

```rust
fn normalize_config(contents: &str) -> String {
    let mut normalized = String::with_capacity(contents.len());
    for line in contents.lines() {
        let line = line.replace('\t', " ");
        if line
            .trim_start()
            .split(' ')
            .next()
            .is_some_and(|key| key.eq_ignore_ascii_case("host"))
        {
            normalized.push_str(&line.to_lowercase());
        } else {
            normalized.push_str(&line);
        }
        normalized.push('\n');
    }
    normalized
}
```

## Why This Works

russh-config is a lightweight reimplementation of a well-specified format, and its API surface reads like it does more than it does: `Config::host_name` looks like the resolved host but is the echo of the query. Normalizing the input to the subset the parser handles (single-space separators, lowercase patterns, explicit `Host *` scope) and reading the fields the parser actually fills (`host_config`) restores OpenSSH-equivalent resolution for the directives rc-git consumes — `HostName`, `User`, `Port`, `IdentityFile` — with URL values still winning where the URL specifies them.

One consequence reaches beyond `target.rs`: `known_hosts` must be looked up by the **dialed** name. OpenSSH records entries under the `HostName` it connects to, not the alias, so `HostKeyHandler` keys `check_known_hosts_path` on `target.host_name` (`hostkey.rs:41-51`, `hostkey.rs:90-95`). Looking up by the alias would raise a spurious trust-on-first-use prompt for a host whose key is already known.

## Prevention

- Test config parsing with the inputs real users have, not the happy path: mixed-case aliases, tab indentation, global options above the first `Host` block. Unit tests pin all four divergences in `target.rs`'s tests module — `config_host_name_user_port_and_identity_are_honored`, `mixed_case_host_patterns_match_the_lowercased_url_host`, `tab_separated_key_value_lines_are_honored`, `global_options_before_the_first_host_block_parse` — and the end-to-end alias round trip is `ssh_config_alias_resolves_hostname_user_and_port` in `crates/rc-git-guard/tests/remote.rs` (alias URL → `HostName 127.0.0.1` + `Port` + `User` from a config file → fetch over the loopback ssh server).
- When wrapping a "lightweight" reimplementation of a specified format, diff its behavior against the spec (`ssh_config(5)`) for the constructs the spec allows, before trusting field reports to find the gaps one at a time. The four divergences here were each invisible until a config used them.
- Prefer verifying against the developer's real environment when possible: the confirming test was `cargo run -p rc-git-cli -- fetch .` on a repo whose `origin` uses the mixed-case alias.

## Related Issues

- [Push over HTTPS with gix that has no push: hand-rolled send-pack](../architecture-patterns/hand-rolled-git-push-over-gix-https.md) — the same remote subsystem; the ssh transport plugs into the transport seam that doc describes.
