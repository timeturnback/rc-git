---
title: "ssh fetch logs in as the wrong GitHub account: agent key order and a dropped IdentitiesOnly"
date: 2026-09-29
category: integration-issues
module: rc-git-guard
problem_type: integration_issue
component: infrastructure
symptoms:
  - "fetch 'origin': An IO error occurred when talking to the server: failed to fill whole buffer"
  - "rc-git fetch fails on an ssh-config alias remote while git ls-remote on the same repository succeeds"
  - "ssh -T git@github.com greets a different account than the alias's IdentityFile key belongs to"
  - "GitHub's 'Repository not found' goes to the channel's stderr, which rc-git never reads"
root_cause: logic_error
resolution_type: code_fix
severity: high
related_components:
  - rc-git-core
tags: [ssh, russh, russh-config, identitiesonly, ssh-agent, github, openssh-parity, fetch]
---

# ssh fetch logs in as the wrong GitHub account: agent key order and a dropped IdentitiesOnly

## Problem

rc-git's in-process ssh transport offered keys in a different order from OpenSSH. On a multi-account alias remote (`git@github.com-work:example-org/example-repo.git`, whose `~/.ssh/config` block sets `HostName github.com`, `IdentityFile <work key>`, `IdentitiesOnly yes`) it offered every ssh-agent key first, in agent order. GitHub accepted the first one, which belonged to a different account, and that account cannot see the repository. Every fetch and Sync on the remote failed with an error that looked like a network fault.

## Symptoms

- App Sync dialog: `rc-git could not sync — fetch 'origin': An IO error occurred when talking to the server`.
- CLI, after about 3 s: `rc-git fetch <repo>` → `fetch 'origin': An IO error occurred when talking to the server: An IO error occurred when talking to the server: failed to fill whole buffer`. This is not a lookup error or a timeout: the connection was made, authenticated, and then the stream ended.
- `git ls-remote origin` on the same repository works, and `ssh -T git@github.com-work` greets the right account (`Hi <work-account>!`).
- `ssh -T git@github.com` greets a different account (`Hi <personal-account>!`), the owner of the first key in `ssh-add -l`. The agent held five keys, and the one the alias names was third.

## What Didn't Work

- **Treating it as host resolution.** The earlier alias fix already resolves `HostName` through russh-config (see the related doc). The about-3-second failure and the EOF, with no lookup error, show that the dial reached GitHub and got through authentication.
- **Treating it as a transport or framing bug.** `failed to fill whole buffer` is std's `UnexpectedEof` message. It means gix's handshake asked for a pkt-line and the channel closed first. The real reason was on the channel's stderr, and rc-git never reads stderr. The transport wraps `channel.into_stream()` (`crates/rc-git-guard/src/actions/remote/ssh.rs:66`), and `SshRead` is a reader over the channel's stdout only (`crates/rc-git-guard/src/actions/remote/ssh/session.rs:141-181`). GitHub's `ERROR: Repository not found.` was dropped, and gix reported the EOF instead.
- **Using `git ls-remote` as the control.** It proves the config and the key are good. OpenSSH orders keys correctly, though, so it cannot expose an ordering bug. What cracked it was checking which identity each path authenticates as: match the `ssh-add -l` fingerprints against `ssh-keygen -lf <key>.pub`, then compare `ssh -T git@github.com` (first agent key wins) with `ssh -T git@github.com-work` (the configured key wins).
- **A narrower fix would not have been enough.** Honoring `IdentitiesOnly` alone fixes this config but not the same alias without `IdentitiesOnly`. OpenSSH still offers the configured key before the agent's other keys there, while the old ladder would still lead with agent order.

## Solution

The fix makes the ladder follow OpenSSH's `pubkey_prepare` (`sshconnect2.c`): first agent keys that match a configured identity file, in agent order; then the agent's other keys unless `IdentitiesOnly`; then the identity files the agent does not hold, in config order. rc-git's agent read keeps only plain public keys, so the certificate steps OpenSSH puts first do not arise (`auth.rs:220-226`). All three parts are in `crates/rc-git-guard/src/actions/remote/ssh/`.

**1. Offer order** (`auth.rs:124-154`). This is a pure function over comparable keys, so it is unit-tested with integers. An agent key that matches a file stands in for that file, so the file is not offered a second time:

```rust
fn offer_order<K: PartialEq>(
    agent: &[K],
    files: &[Option<K>],
    identities_only: bool,
) -> Vec<Offer> {
    let mut covered = vec![false; files.len()];
    let mut matched = Vec::new();
    let mut unmatched = Vec::new();
    for (index, key) in agent.iter().enumerate() {
        let mut matches_a_file = false;
        for (file, file_key) in files.iter().enumerate() {
            if file_key.as_ref() == Some(key) {
                covered[file] = true;
                matches_a_file = true;
            }
        }
        if matches_a_file {
            matched.push(Offer::Agent(index));
        } else if !identities_only {
            unmatched.push(Offer::Agent(index));
        }
    }
    let mut order = matched;
    order.extend(unmatched);
    for (file, covered) in covered.into_iter().enumerate() {
        if !covered {
            order.push(Offer::File(file));
        }
    }
    order
}
```

`authenticate` feeds it `PublicKey::key_data()` rather than `PublicKey` (`auth.rs:73-83`). ssh-key derives `PartialEq` on `PublicKey` over the comment too (ssh-key 0.7.0-rc.11, `src/public.rs:94-106`, a dependency path). The agent's copy and the `.pub` file's copy usually have different comments, so whole-key equality never matches. `file_public_key` (`auth.rs:233-243`) reads `<path>.pub`. When that file is missing, it takes the public half that an OpenSSH-format private key stores in the clear, so an encrypted key matches its agent copy without a passphrase. `authenticate` walks the order through a `Ladder` (handle, target, RSA hash, op; `auth.rs:157-201`). Each offer ends as `Outcome::Accepted`, `Rejected` or `Locked` (`auth.rs:35-40`). A locked file becomes `PassphraseRequired` only after every other candidate has failed (`auth.rs:84-118`).

**2. `IdentitiesOnly` is read, although russh-config drops it.** russh-config 0.58.0's `HostConfig` has only `user`, `hostname`, `port`, `identity_file`, `proxy_command`, `proxy_jump`, `add_keys_to_agent`, `user_known_hosts_file` and `strict_host_key_checking` (`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/russh-config-0.58.0/src/lib.rs:30-49`). Its parser sends any other key to a `debug!` and throws it away (same file, `lib.rs:291-293`). rc-git builds a second view of the normalized config that keeps only `Host` lines and rewrites each `IdentitiesOnly <v>` as `User <v>` (`target.rs:207-223`):

```rust
fn identities_only_as_user(normalized: &str) -> String {
    let mut view = String::new();
    for line in normalized.lines() {
        let line = line.trim_start();
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        if key.eq_ignore_ascii_case("host") {
            view.push_str(line);
        } else if key.eq_ignore_ascii_case("identitiesonly") {
            view.push_str("User ");
            view.push_str(value);
        } else {
            continue;
        }
        view.push('\n');
    }
    view
}
```

`load_config` parses that view through the same `parse_for_host` as the real config (`target.rs:182-186`, `target.rs:195-201`) and reads `.user` as the `IdentitiesOnly` value. Only `Host` lines survive in the view, so a real `User` directive cannot leak into the answer.

**3. Default keys only when no `IdentityFile` applies** (`target.rs:82-92`). rc-git used to append `id_ed25519`, `id_ecdsa` and `id_rsa` after any configured files. OpenSSH adds them only when `num_identity_files == 0` (`fill_default_options` in `readconf.c`). A default key tried after the configured one can log in as another account for the same reason the agent key did.

Verified with `cargo run -p rc-git-cli -- fetch <repo>` on the failing repository, which returned `fetch origin up to date`. A second alias repository still fetches, and `scripts/check.sh --fast` passes.

## Why This Works

GitHub decides the account during ssh user authentication and the repository permission only at `exec`. It accepts the first offered key registered to any account, then checks whether that account can read `example-org/example-repo` when `git-upload-pack` runs. On a host like that, the key offer order is the account choice. OpenSSH's order is the contract that multi-account configs are written against: name the account's key with `IdentityFile`, and optionally rule out the rest with `IdentitiesOnly`. Matching agent keys to files by key data keeps the agent as the signer, so there is no passphrase prompt for a key the agent already holds, while the choice of key still comes from the config.

Rewriting `IdentitiesOnly` as `User` reuses russh-config's own block matching and merge rule rather than adding a second host matcher that could drift from it. `query` folds the matching blocks in file order (`lib.rs:120-131`), and `merge` fills a scalar field such as `user` only while it is still `None` (`lib.rs:52-72`; `identity_file` alone accumulates, `lib.rs:74-79`). That is OpenSSH's first-value-wins rule, so `IdentitiesOnly` resolves exactly as `User` would, including the `Host *` retry for globals above the first block.

## Prevention

- Treat key offer order as account selection. Any change to `auth.rs` keeps the `pubkey_prepare` order, which these tests in `auth.rs` pin: `an_agent_key_matching_an_identity_file_is_offered_before_the_rest` (the field failure), `identities_only_drops_agent_keys_that_match_no_identity_file`, `identities_only_with_the_key_outside_the_agent_offers_only_the_file`, `identity_files_the_agent_does_not_hold_follow_every_agent_key_in_config_order`, `no_agent_offers_every_identity_file_in_config_order`. The public-key source is pinned by `the_public_key_comes_from_the_pub_file_beside_the_identity`, `without_a_pub_file_the_public_key_comes_from_the_private_key` and `an_unreadable_identity_has_no_public_key`.
- Before relying on an ssh directive, check that russh-config's `HostConfig` carries it. Its catch-all arm drops unknown keys with only a `debug!`. A directive that is not a field needs a view like `identities_only_as_user` plus a test. `IdentitiesOnly` resolution is pinned by `identities_only_is_read_from_the_matching_block` and `identities_only_takes_the_first_value_in_file_order` in `target.rs`, and default-key suppression by `config_host_name_user_port_and_identity_are_honored`. Other options multi-account setups use, such as `IdentityAgent` and `CertificateFile`, are still not `HostConfig` fields.
- Compare ssh keys by `key_data()`, never with `PublicKey ==`, because the comment takes part in equality.
- Read `failed to fill whole buffer` (or any `UnexpectedEof`) straight after connect as "the server closed the channel after exec", not as a network fault. Check which account authenticated before touching the transport: `ssh -T git@<real host>` against `ssh -T git@<alias>`, and `ssh-add -l` against `ssh-keygen -lf <key>.pub`.
- Open gap, not done in the fix: when the channel closes early, rc-git still drops the server's stderr, so the next rejection after exec will also arrive as a bare EOF. Showing that stderr in the error would have printed `Repository not found` directly.

## Related Issues

- [russh-config diverges from OpenSSH: ssh-config aliases silently fail to resolve](../integration-issues/russh-config-ssh-alias-resolution-divergence.md) covers the same `target.rs` and the same class of problem: russh-config accepting legal OpenSSH input and silently doing less with it. The `Host *` retry (now `parse_for_host`) and `normalize_config` that the `IdentitiesOnly` view reuses come from that fix.
- [Sync spins forever: a cancel flag nobody could set and a transport with no idle budget](../runtime-errors/sync-hangs-with-no-cancel-and-no-idle-timeout.md) covers `Watch::drive` in `session.rs`, which drives the `SshRead`/`SshWrite` channel halves. This failure ended at EOF in those halves.
