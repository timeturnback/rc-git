//! The binary's contract: output shape and exit codes.
#![expect(
    clippy::unwrap_used,
    reason = "test helpers panic when the binary cannot run"
)]

use std::process::{Command, Output};

use rc_git_testkit::{COMMIT_1_TITLE, COMMIT_2_TITLE, Fixture, MAIN_RS};

#[expect(clippy::disallowed_methods, reason = "tests run the built binary")]
fn rc_git(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rc-git"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn head_prints_ref_and_commit() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    let out = rc_git(&["head", path]);
    assert!(out.status.success());
    assert_eq!(
        stdout(&out),
        format!("refs/heads/main\t{}\n", fx.rev_parse("HEAD"))
    );
}

#[test]
fn refs_lists_kind_short_name_oid_full_name() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    let out = rc_git(&["refs", path]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(text.contains(&format!(
        "branch\tmain\t{}\trefs/heads/main\n",
        fx.rev_parse("main")
    )));
    assert!(text.contains("remote\torigin/main\t"));
    assert!(text.contains(&format!(
        "tag\tv1.0\t{}\trefs/tags/v1.0\n",
        fx.rev_parse("v1.0^{commit}")
    )));
}

#[test]
fn ls_defaults_to_head_root_and_accepts_rev_and_path() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();

    let root = stdout(&rc_git(&["ls", path]));
    let first: Vec<&str> = root
        .lines()
        .map(|l| l.rsplit('\t').next().unwrap())
        .collect();
    assert_eq!(
        first,
        [
            "docs",
            "src",
            "alpha.txt",
            "bin.dat",
            "link",
            "README.md",
            "Zeta.txt"
        ]
    );
    assert!(root.starts_with("tree\t"));

    let src = stdout(&rc_git(&["ls", path, "main", "src"]));
    assert_eq!(
        src,
        format!(
            "blob\t{}\tlib.rs\nblob\t{}\tmain.rs\n",
            fx.rev_parse("main:src/lib.rs"),
            fx.rev_parse("main:src/main.rs")
        )
    );

    let at_v09 = stdout(&rc_git(&["ls", path, "v0.9"]));
    assert!(
        !at_v09.contains("docs"),
        "docs arrived in the second commit"
    );

    let missing = rc_git(&["ls", path, "main", "nope"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("'nope' does not exist at main"));

    let file = rc_git(&["ls", path, "main", "README.md"]);
    assert_eq!(file.status.code(), Some(1));
    assert!(stderr(&file).contains("is a file"));
}

#[test]
fn cat_streams_text_and_signals_binary_and_oversize() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();

    let text = rc_git(&["cat", path, "main", "src/main.rs"]);
    assert!(text.status.success());
    assert_eq!(text.stdout, MAIN_RS);

    let binary = rc_git(&["cat", path, "main", "bin.dat"]);
    assert_eq!(binary.status.code(), Some(3));
    assert_eq!(binary.stdout.len(), 0);
    assert!(stderr(&binary).contains("is binary"));

    let large = rc_git(&["cat", path, "main", "README.md", "2"]);
    assert_eq!(large.status.code(), Some(4));
    assert!(stderr(&large).contains("over the 2 byte limit"));

    let dir = rc_git(&["cat", path, "main", "src"]);
    assert_eq!(dir.status.code(), Some(1));
    assert!(stderr(&dir).contains("is a directory"));

    let bad_limit = rc_git(&["cat", path, "main", "README.md", "lots"]);
    assert_eq!(bad_limit.status.code(), Some(1));
    assert!(stderr(&bad_limit).contains("not a number"));
}

#[test]
fn log_pages_newest_first() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();

    let all = stdout(&rc_git(&["log", path]));
    let titles: Vec<&str> = all
        .lines()
        .map(|l| l.rsplit('\t').next().unwrap())
        .collect();
    assert_eq!(titles, [COMMIT_2_TITLE, COMMIT_1_TITLE]);
    assert!(all.starts_with(&fx.rev_parse("main")));
    assert!(all.contains("\t1704067200\tTest Author\t"));

    let one = stdout(&rc_git(&["log", path, "main", "-n", "1"]));
    assert_eq!(one.lines().count(), 1);
    let skipped = stdout(&rc_git(&["log", path, "--skip", "1", "feature"]));
    assert!(skipped.ends_with(&format!("{COMMIT_1_TITLE}\n")));

    let bad = rc_git(&["log", path, "-n"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(stderr(&bad).contains("-n needs a number"));
}

#[test]
fn filelog_lists_commits_that_changed_a_path() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();

    let lib = stdout(&rc_git(&["filelog", path, "src/lib.rs"]));
    assert_eq!(lib.lines().count(), 1);
    assert!(lib.ends_with(&format!("{COMMIT_2_TITLE}\n")));

    let readme = stdout(&rc_git(&["filelog", path, "README.md", "main"]));
    assert_eq!(readme.lines().count(), 1);
    assert!(readme.ends_with(&format!("{COMMIT_1_TITLE}\n")));

    let chunked = stdout(&rc_git(&["filelog", path, "README.md", "--max-scan", "1"]));
    assert_eq!(chunked, readme, "chunked scanning changes nothing");

    let none = stdout(&rc_git(&["filelog", path, "nope.txt"]));
    assert_eq!(none.len(), 0);

    let missing = rc_git(&["filelog", path]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("filelog needs <path>"));
}

#[test]
fn search_filters_by_message_author_and_dates() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();

    let docs = stdout(&rc_git(&["search", path, "--message", "DOCS"]));
    assert_eq!(docs.lines().count(), 1);
    assert!(docs.ends_with(&format!("{COMMIT_2_TITLE}\n")));

    let author = stdout(&rc_git(&["search", path, "--author", "test author"]));
    assert_eq!(author.lines().count(), 2, "both commits share the author");

    let until = stdout(&rc_git(&["search", path, "--until", "1704067200"]));
    assert_eq!(until.lines().count(), 1);
    assert!(until.ends_with(&format!("{COMMIT_1_TITLE}\n")));

    let since = stdout(&rc_git(&["search", path, "--since", "1704153600"]));
    assert_eq!(since.lines().count(), 1);
    assert!(since.ends_with(&format!("{COMMIT_2_TITLE}\n")));

    let chunked = stdout(&rc_git(&[
        "search",
        path,
        "--message",
        "docs",
        "--max-scan",
        "1",
    ]));
    assert_eq!(chunked, docs, "chunked scanning changes nothing");

    let bad = rc_git(&["search", path, "--since", "yesterday"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(stderr(&bad).contains("not a number"));
}

#[test]
fn show_prints_header_and_message() {
    let fx = Fixture::standard();
    fx.commit_with_body("Detail commit", "The body.", "2024-01-04T00:00:00+0000");
    let path = fx.path().to_str().unwrap();

    let shown = stdout(&rc_git(&["show", path, "main"]));
    assert!(shown.starts_with(&format!("commit\t{}\n", fx.rev_parse("main"))));
    assert!(shown.contains(&format!("tree\t{}\n", fx.rev_parse("main^{tree}"))));
    assert!(shown.contains(&format!("parent\t{}\n", fx.rev_parse("main~1"))));
    assert!(shown.contains("author\tTest Author <author@example.com>\t"));
    assert!(shown.contains("committer\tTest Committer <committer@example.com>\t1704326400\t0"));
    assert!(shown.ends_with("Detail commit\n\nThe body.\n"));

    let plain = stdout(&rc_git(&["show", path, "main~1"]));
    assert!(plain.ends_with("Add docs and library\n"));

    let missing = rc_git(&["show", path]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("show needs <rev>"));
}

#[test]
fn resolve_prints_the_oid() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    let out = rc_git(&["resolve", path, "v1.0"]);
    assert_eq!(stdout(&out), format!("{}\n", fx.rev_parse("v1.0")));
    let bad = rc_git(&["resolve", path, "nope"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(stderr(&bad).contains("'nope' is not a revision"));
}

#[test]
fn usage_and_open_errors_have_distinct_exit_codes() {
    let no_args = rc_git(&[]);
    assert_eq!(no_args.status.code(), Some(2));
    assert!(stderr(&no_args).contains("usage: rc-git"));

    let help = rc_git(&["--help"]);
    assert_eq!(help.status.code(), Some(2));

    let unknown = rc_git(&["frobnicate", "."]);
    assert_eq!(
        unknown.status.code(),
        Some(2),
        "rejected before any repository is opened"
    );
    assert!(stderr(&unknown).contains("unknown command 'frobnicate'"));

    let fx = Fixture::empty_dir();
    let not_repo = rc_git(&["head", fx.path().to_str().unwrap()]);
    assert_eq!(not_repo.status.code(), Some(1));
    assert!(stderr(&not_repo).contains("cannot open git repository at"));
}

#[test]
fn time_flag_reports_to_stderr_only() {
    let fx = Fixture::standard();
    let out = rc_git(&["--time", "head", fx.path().to_str().unwrap()]);
    assert!(out.status.success());
    assert!(stdout(&out).starts_with("refs/heads/main\t"));
    assert!(stderr(&out).starts_with("time: open "));
    assert!(stderr(&out).contains(" ms, head "));
}

#[test]
fn diff_lists_changes_and_prints_hunks() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    let out = rc_git(&["diff", path, "v0.9", "main"]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(text.contains("A\tdocs/guide.md\n"));
    assert!(text.contains("A\tsrc/lib.rs\n"));
    assert!(!text.contains("\tREADME.md"), "unchanged files stay out");

    let out = rc_git(&["diff", path, "v0.9", "main", "docs/guide.md"]);
    assert!(out.status.success());
    assert_eq!(stdout(&out), "@@ -1,0 +1,1 @@\n+guide\n");

    let out = rc_git(&["diff", path, "v0.9", "main", "bin.dat"]);
    assert_eq!(out.status.code(), Some(3), "binary exit code");

    let out = rc_git(&["diff", path, "v0.9", "main", "missing.txt"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("does not exist"));

    let out = rc_git(&["diff", path, "v0.9"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("diff needs <old> <new>"));
}

#[test]
fn blame_prints_commit_per_range() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    let out = rc_git(&["blame", path, "main", "README.md"]);
    assert!(out.status.success());
    assert_eq!(
        stdout(&out),
        format!(
            "{}\t1\t3\tTest Author\t{}\n",
            fx.rev_parse("v0.9^{commit}"),
            COMMIT_1_TITLE
        )
    );

    let out = rc_git(&["blame", path, "main", "nope.txt"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("blame file"));

    let out = rc_git(&["blame", path, "main"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("blame needs <rev> <path>"));
}

#[test]
fn status_overlays_the_working_tree_against_head() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.write("untracked.txt", b"new\n");
    fx.write("README.md", b"changed\n");

    let out = rc_git(&["status", path]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(text.contains("?\tblob\tuntracked.txt\n"));
    assert!(text.contains("M\tblob\tREADME.md\n"));
    assert!(text.contains(" \tblob\talpha.txt\n"));
    assert!(text.contains(" \ttree\tsrc\n"));

    let out = rc_git(&["status", path, "src"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains(" \tblob\tmain.rs\n"));

    let out = rc_git(&["status", path, "no-such-dir"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("no-such-dir"));
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the test arranges the fixture's working tree; the binary never mutates"
)]
fn changes_lists_every_changed_path_flat() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.write("untracked.txt", b"new\n");
    fx.write("src/lib.rs", b"changed\n");
    std::fs::remove_file(fx.path().join("Zeta.txt")).unwrap();

    let out = rc_git(&["changes", path]);
    assert!(out.status.success());
    assert_eq!(
        stdout(&out),
        "D\tZeta.txt\nM\tsrc/lib.rs\n?\tuntracked.txt\n"
    );

    let out = rc_git(&["changes", path, "--max-scan", "1"]);
    assert!(out.status.success());
    assert!(stderr(&out).contains("truncated after scanning 2 entries"));

    let out = rc_git(&["changes", path, "--bogus"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("--bogus"));
}

#[test]
fn files_lists_every_file_on_disk_including_ignored_ones() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.write(".gitignore", b".env\ntarget/\n");
    fx.write(".env", b"SECRET=1\n");
    fx.write("target/debug/out.bin", b"x\n");

    let out = rc_git(&["files", path]);
    assert!(out.status.success());
    assert_eq!(
        stdout(&out),
        ".env\n.gitignore\nREADME.md\nZeta.txt\nalpha.txt\nbin.dat\ndocs/guide.md\nlink\n\
         src/lib.rs\nsrc/main.rs\n"
    );

    let out = rc_git(&["files", path, "--max-scan", "1"]);
    assert!(out.status.success());
    assert!(stderr(&out).contains("truncated after scanning 2 entries"));

    let out = rc_git(&["files", path, "--bogus"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("--bogus"));
}

#[test]
fn stash_lists_entries_newest_first() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();

    let out = rc_git(&["stash", path]);
    assert!(out.status.success());
    assert_eq!(stdout(&out), "");

    fx.write("alpha.txt", b"stash one\n");
    fx.git(&["stash", "push", "-q", "-m", "first"]);
    fx.write("alpha.txt", b"stash two\n");
    fx.git(&["stash", "push", "-q", "-m", "second"]);

    let out = rc_git(&["stash", path]);
    assert!(out.status.success());
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("0\t"), "{}", lines[0]);
    assert!(lines[0].ends_with("second"), "{}", lines[0]);
    assert!(lines[1].starts_with("1\t"), "{}", lines[1]);
    assert!(lines[1].ends_with("first"), "{}", lines[1]);
}

#[test]
fn diff_worktree_shows_hunks_against_head() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.write("alpha.txt", b"alpha\nbeta\n");

    let out = rc_git(&["diff", path, "--worktree", "alpha.txt"]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(text.starts_with("@@ "), "{text}");
    assert!(text.contains("+beta\n"), "{text}");

    let out = rc_git(&["diff", path, "--worktree"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("diff --worktree needs <path>"));
}

#[test]
fn commit_and_discard_drive_the_guard_from_the_command_line() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.write("alpha.txt", b"cli edit\n");

    let out = rc_git(&[
        "commit",
        path,
        "-m",
        "CLI commit",
        "--author",
        "CLI User <cli@example.com>",
        "alpha.txt",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let line = stdout(&out);
    assert!(line.ends_with("\tmain\n"), "{line}");
    assert_eq!(
        fx.git_output(&["log", "-1", "--format=%an|%s"]),
        "CLI User|CLI commit"
    );
    assert_eq!(fx.git_output(&["status", "--porcelain"]), "");

    fx.write("alpha.txt", b"dirty again\n");
    let out = rc_git(&["discard", path, "alpha.txt"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(fx.git_output(&["status", "--porcelain"]), "");
}

#[test]
fn the_stash_verbs_round_trip() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.write("alpha.txt", b"stash me\n");

    let out = rc_git(&[
        "stash-push",
        path,
        "-m",
        "cli stash",
        "--author",
        "CLI User <cli@example.com>",
        "alpha.txt",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).ends_with("\tOn main: cli stash\n"),
        "{}",
        stdout(&out)
    );
    assert_eq!(fx.git_output(&["status", "--porcelain"]), "");

    let out = rc_git(&["stash-apply", path]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        std::fs::read(fx.path().join("alpha.txt")).unwrap(),
        b"stash me\n"
    );
    fx.git(&["checkout", "-q", "--", "alpha.txt"]);

    let out = rc_git(&["stash-drop", path, "0"]);
    assert_eq!(out.status.code(), Some(1), "drop needs --confirm drop");
    let out = rc_git(&["stash-drop", path, "0", "--confirm", "drop"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "refs/rc-git/backup/stash/0\n");
    assert_eq!(fx.git_output(&["stash", "list"]), "");
}

// --- network verbs against the test HTTP server ---

/// Origin, a served bare copy (pushes enabled), and a clone with `origin` upstream.
fn served_clone() -> (Fixture, Fixture, rc_git_testkit::HttpGitServer, Fixture) {
    let origin = Fixture::standard();
    let bare = Fixture::empty_dir();
    let bare_path = bare.path().join("remote.git");
    origin.git(&[
        "clone",
        "--bare",
        "--quiet",
        ".",
        bare_path.to_str().unwrap(),
    ]);
    bare.git(&[
        "--git-dir",
        "remote.git",
        "config",
        "http.receivepack",
        "true",
    ]);
    let server = rc_git_testkit::HttpGitServer::serve(&bare_path);
    let clone = Fixture::empty_dir();
    clone.git(&["clone", "--quiet", &server.url(), "."]);
    (origin, bare, server, clone)
}

#[test]
fn sync_fetches_pulls_and_pushes() {
    let (origin, bare, _server, clone) = served_clone();
    // Remote gains one commit; local gains none, then commits one after the pull.
    origin.write("news.txt", b"news\n");
    origin.commit("News on origin", "2024-03-01T00:00:00+0000");
    origin.git(&[
        "push",
        "--quiet",
        bare.path().join("remote.git").to_str().unwrap(),
        "main",
    ]);
    let path = clone.path().to_str().unwrap();

    let out = rc_git(&["sync", path]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("fetch\trefs/remotes/origin/main"), "{text}");
    assert!(text.contains("pull\tmain\t"), "{text}");
    assert!(text.contains("push\tup to date"), "{text}");

    clone.write("local.txt", b"local\n");
    clone.commit("Local work", "2024-03-02T00:00:00+0000");
    let out = rc_git(&["sync", path]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("push\tmain\torigin/main\t1 commit(s)"),
        "{text}"
    );
    assert_eq!(
        bare.git_output(&["--git-dir", "remote.git", "rev-parse", "main"]),
        clone.rev_parse("HEAD")
    );
}

#[test]
fn network_verbs_report_auth_with_exit_code_5() {
    let origin = Fixture::standard();
    let bare = Fixture::empty_dir();
    let bare_path = bare.path().join("remote.git");
    origin.git(&[
        "clone",
        "--bare",
        "--quiet",
        ".",
        bare_path.to_str().unwrap(),
    ]);
    let server = rc_git_testkit::HttpGitServer::serve_with_auth(&bare_path, "user", "sekret");
    let clone = Fixture::empty_dir();
    let with_creds = server.url().replace("http://", "http://user:sekret@");
    clone.git(&[
        "-c",
        "credential.helper=",
        "clone",
        "--quiet",
        &with_creds,
        ".",
    ]);
    clone.git(&["remote", "set-url", "origin", &server.url()]);
    let path = clone.path().to_str().unwrap();

    let out = rc_git(&["fetch", path]);
    assert_eq!(out.status.code(), Some(5), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("requires authentication"),
        "{}",
        stderr(&out)
    );

    let out = rc_git(&["fetch", path, "--username", "user", "--token", "sekret"]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn pull_reports_divergence_as_an_error() {
    let (origin, bare, _server, clone) = served_clone();
    origin.write("news.txt", b"news\n");
    origin.commit("News on origin", "2024-03-01T00:00:00+0000");
    origin.git(&[
        "push",
        "--quiet",
        bare.path().join("remote.git").to_str().unwrap(),
        "main",
    ]);
    clone.write("local.txt", b"local\n");
    clone.commit("Local work", "2024-03-02T00:00:00+0000");
    let path = clone.path().to_str().unwrap();

    let out = rc_git(&["pull", path]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("cannot fast-forward"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn ssh_unknown_host_exits_6_and_accept_hostkey_lets_the_fetch_through() {
    let origin = Fixture::standard();
    let bare = Fixture::empty_dir();
    let bare_path = bare.path().join("remote.git");
    origin.git(&[
        "clone",
        "--bare",
        "--quiet",
        ".",
        bare_path.to_str().unwrap(),
    ]);
    let server = rc_git_testkit::SshGitServer::serve(&bare_path);
    let clone = Fixture::empty_dir();
    clone.git(&["clone", "--quiet", bare_path.to_str().unwrap(), "."]);
    clone.git(&["remote", "set-url", "origin", &server.url()]);
    let ssh_home = Fixture::empty_dir();
    server.write_client_key(ssh_home.path());
    let path = clone.path().to_str().unwrap();
    let ssh_dir = ssh_home.path().to_str().unwrap();

    let out = rc_git(&["fetch", path, "--ssh-dir", ssh_dir]);
    assert_eq!(out.status.code(), Some(6), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&server.host_fingerprint()),
        "{}",
        stderr(&out)
    );

    let out = rc_git(&[
        "fetch",
        path,
        "--ssh-dir",
        ssh_dir,
        "--accept-hostkey",
        &server.host_fingerprint(),
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn default_branch_names_the_mainline_or_none() {
    let fx = Fixture::standard();
    let out = rc_git(&["default-branch", fx.path().to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "main\n");

    let bare = Fixture::empty_dir();
    bare.git(&["init", "-q", "-b", "work"]);
    let out = rc_git(&["default-branch", bare.path().to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "none\n");
}

#[test]
fn checkout_switches_branches_and_reports_a_dirty_path() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();

    let out = rc_git(&["checkout", path, "feature"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).starts_with("checkout\tfeature\t"),
        "{}",
        stdout(&out)
    );
    assert_eq!(
        fx.git_output(&["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature"
    );

    let out = rc_git(&["checkout", path, "feature"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "checkout\tfeature\talready checked out\n");

    // feature.txt only exists on `feature`, so switching away must delete it — and it is dirty.
    fx.write("feature.txt", b"work in progress\n");
    let out = rc_git(&["checkout", path, "main"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("feature.txt"), "{}", stderr(&out));
    assert_eq!(
        fx.git_output(&["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature"
    );

    let out = rc_git(&["checkout", path]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("exactly one branch name"),
        "{}",
        stderr(&out)
    );
}
