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
    assert!(binary.stdout.is_empty());
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
