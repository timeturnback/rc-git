//! The binary's contract: output shape and exit codes.
#![expect(
    clippy::unwrap_used,
    reason = "test helpers panic when the binary cannot run"
)]

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::process::{Command, Output, Stdio};

use rc_git_testkit::{COMMIT_1_TITLE, COMMIT_2_TITLE, Fixture, MAIN_RS};

/// The built binary with a scrubbed environment: only `PATH` survives, so a caller's
/// `RC_GIT_TOKEN`, `RC_GIT_KEY_PASSPHRASE`, proxy variables or `HOME` never reach it.
#[expect(clippy::disallowed_methods, reason = "tests run the built binary")]
fn rc_git_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rc-git"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    command
}

fn rc_git(args: &[&str]) -> Output {
    rc_git_with_env(args, &[])
}

/// [`rc_git`] with extra environment variables, to exercise the env fallbacks on purpose.
fn rc_git_with_env(args: &[&str], env: &[(&str, &str)]) -> Output {
    rc_git_command()
        .args(args)
        .envs(env.iter().copied())
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
    assert_eq!(bad_limit.status.code(), Some(2));
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
    assert_eq!(bad.status.code(), Some(2));
    assert!(stderr(&bad).contains("-n needs a value"));
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

    let bounded = rc_git(&["filelog", path, "README.md", "--max-scan", "1"]);
    assert!(bounded.status.success(), "{}", stderr(&bounded));
    assert_eq!(
        stdout(&bounded),
        "",
        "README.md last changed in the second commit walked"
    );
    assert!(
        stderr(&bounded).contains("warning: truncated after scanning 1 commits"),
        "{}",
        stderr(&bounded)
    );
    let wide = rc_git(&["filelog", path, "README.md", "--max-scan", "2"]);
    assert_eq!(stdout(&wide), readme);
    assert_eq!(
        stderr(&wide),
        "",
        "a budget covering all history is no truncation"
    );

    let zero = rc_git(&["filelog", path, "README.md", "--max-scan", "0"]);
    assert_eq!(zero.status.code(), Some(2));
    assert!(stderr(&zero).contains("--max-scan must be at least 1"));

    let none = stdout(&rc_git(&["filelog", path, "nope.txt"]));
    assert_eq!(none.len(), 0);

    let missing = rc_git(&["filelog", path]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(stderr(&missing).contains("filelog needs <path>"));
}

#[test]
fn every_verb_reads_one_spelling_of_a_path() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();

    let lib = stdout(&rc_git(&["filelog", path, "src/lib.rs"]));
    assert_eq!(lib.lines().count(), 1);
    for spelling in ["./src/lib.rs", "/src/lib.rs", "src//./lib.rs"] {
        let out = rc_git(&["filelog", path, spelling]);
        assert!(out.status.success(), "{spelling}: {}", stderr(&out));
        assert_eq!(stdout(&out), lib, "filelog {spelling}");
    }
    assert_eq!(
        stdout(&rc_git(&["cat", path, "HEAD", "./README.md"])),
        stdout(&rc_git(&["cat", path, "HEAD", "README.md"]))
    );
    assert_eq!(
        stdout(&rc_git(&["ls", path, "HEAD", "./src/"])),
        stdout(&rc_git(&["ls", path, "HEAD", "src"]))
    );

    let root = stdout(&rc_git(&["status", path]));
    assert!(!root.contains(".git"), "{root}");
    for spelling in ["/", "./", "."] {
        assert_eq!(
            stdout(&rc_git(&["status", path, spelling])),
            root,
            "status {spelling}"
        );
    }
    assert_eq!(
        stdout(&rc_git(&["status", path, "./src"])),
        stdout(&rc_git(&["status", path, "src"]))
    );

    for args in [
        ["filelog", path, "src/../README.md"].as_slice(),
        ["status", path, ".."].as_slice(),
        ["discard", path, "--yes", "../outside.txt"].as_slice(),
    ] {
        let out = rc_git(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(stderr(&out).contains("'..' segment"), "{}", stderr(&out));
    }
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

    let newest = rc_git(&["search", path, "--message", "docs", "--max-scan", "1"]);
    assert_eq!(
        stdout(&newest),
        docs,
        "the match is the first commit walked"
    );

    let bounded = rc_git(&["search", path, "--until", "1704067200", "--max-scan", "1"]);
    assert!(bounded.status.success(), "{}", stderr(&bounded));
    assert_eq!(
        stdout(&bounded),
        "",
        "the only match is the second commit walked"
    );
    assert!(
        stderr(&bounded).contains("warning: truncated after scanning 1 commits"),
        "{}",
        stderr(&bounded)
    );

    let filled = rc_git(&["search", path, "-n", "1", "--max-scan", "1"]);
    assert_eq!(stdout(&filled).lines().count(), 1);
    assert_eq!(stderr(&filled), "", "reaching -n is not truncation");

    let zero = rc_git(&["search", path, "--max-scan", "0"]);
    assert_eq!(zero.status.code(), Some(2));
    assert!(stderr(&zero).contains("--max-scan must be at least 1"));

    let bad = rc_git(&["search", path, "--since", "yesterday"]);
    assert_eq!(bad.status.code(), Some(2));
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
    assert_eq!(missing.status.code(), Some(2));
    assert!(stderr(&missing).contains("show needs <rev>"));
}

#[test]
fn show_prints_author_and_committer_offsets_apart() {
    let fx = Fixture::standard();
    fx.git_with_env(
        &["commit", "-q", "--allow-empty", "-m", "zoned"],
        &[
            ("GIT_AUTHOR_DATE", "2024-01-04T00:00:00-0130"),
            ("GIT_COMMITTER_DATE", "2024-01-04T00:00:00+0545"),
        ],
    );
    let path = fx.path().to_str().unwrap();

    let shown = stdout(&rc_git(&["show", path, "main"]));
    assert!(shown.contains("author\tTest Author <author@example.com>\t1704331800\t-5400\n"));
    assert!(
        shown.contains("committer\tTest Committer <committer@example.com>\t1704305700\t20700\n")
    );
}

#[test]
fn resolve_prints_the_oid() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    let out = rc_git(&["resolve", path, "v1.0"]);
    assert_eq!(stdout(&out), format!("{}\n", fx.rev_parse("v1.0")));
    let bad = rc_git(&["resolve", path, "nope"]);
    assert_eq!(bad.status.code(), Some(1));
    let err = stderr(&bad);
    assert_eq!(
        err,
        "error: 'nope' is not a revision in this repository: \
         The ref partially named \"nope\" could not be found\n"
    );
    assert_eq!(err.matches("could not be found").count(), 1);
    assert!(!err.contains("registry/src"), "{err}");
}

#[test]
fn usage_and_open_errors_have_distinct_exit_codes() {
    let no_args = rc_git(&[]);
    assert_eq!(no_args.status.code(), Some(2));
    assert!(stderr(&no_args).contains("usage: rc-git"));

    let help = rc_git(&["--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(stdout(&help).starts_with("usage: rc-git"));
    assert_eq!(stderr(&help), "");

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

fn rc_git_os(args: &[OsString]) -> Output {
    rc_git_command().args(args).output().unwrap()
}

#[test]
fn non_utf8_arguments_are_usage_errors_and_repo_paths_stay_bytes() {
    let fx = Fixture::standard();
    let not_utf8 = OsStr::from_bytes(b"\xff.txt").to_owned();

    let path_arg = rc_git_os(&[
        "cat".into(),
        fx.path().into(),
        "HEAD".into(),
        not_utf8.clone(),
    ]);
    assert_eq!(path_arg.status.code(), Some(2));
    assert!(stderr(&path_arg).contains("is not valid UTF-8"));
    assert!(!stderr(&path_arg).contains("panicked"));

    let verb = rc_git_os(&[not_utf8.clone(), fx.path().into()]);
    assert_eq!(verb.status.code(), Some(2));
    assert!(!stderr(&verb).contains("panicked"));

    let mut repo = fx.path().as_os_str().to_owned();
    repo.push(&not_utf8);
    let repo_arg = rc_git_os(&["head".into(), repo]);
    assert_eq!(
        repo_arg.status.code(),
        Some(1),
        "an open error, not a usage error"
    );
    assert!(stderr(&repo_arg).contains("cannot open git repository at"));
}

#[test]
#[expect(clippy::disallowed_methods, reason = "tests run the built binary")]
fn a_reader_that_closes_early_is_not_an_error() {
    let fx = Fixture::standard();
    let mut child = Command::new(env!("CARGO_BIN_EXE_rc-git"))
        .args(["log", fx.path().to_str().unwrap()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "status {:?}", out.status);
    assert_eq!(stderr(&out), "");
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
    assert_eq!(stdout(&out), "@@ -0,0 +1,1 @@\n+guide\n");

    let out = rc_git(&["diff", path, "v0.9", "main", "bin.dat"]);
    assert_eq!(out.status.code(), Some(3), "binary exit code");

    let out = rc_git(&["diff", path, "v0.9", "main", "missing.txt"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("does not exist"));

    let out = rc_git(&["diff", path, "v0.9", "main", "src"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("'src' is a directory"),
        "{}",
        stderr(&out)
    );

    let out = rc_git(&["diff", path]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("diff needs <rev>, or <old> <new> [path]"));
}

#[test]
fn diff_of_a_submodule_bump_prints_subproject_lines() {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "main"]);
    fx.write("a", b"x\n");
    fx.commit("no submodule yet", "2024-01-01T00:00:00+0000");
    let old = "1111111111111111111111111111111111111111";
    let new = "2222222222222222222222222222222222222222";
    let date = "2024-01-02T00:00:00+0000";
    let dates = [("GIT_AUTHOR_DATE", date), ("GIT_COMMITTER_DATE", date)];
    for (sha, title) in [(old, "one"), (new, "two")] {
        let gitlink = format!("160000,{sha},sub");
        fx.git(&["update-index", "--add", "--cacheinfo", &gitlink]);
        fx.git_with_env(&["commit", "-q", "-m", title], &dates);
    }
    let path = fx.path().to_str().unwrap();

    let out = rc_git(&["diff", path, "HEAD~1", "HEAD", "sub"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("-Subproject commit {old}\n+Subproject commit {new}\n")
    );

    let out = rc_git(&["diff", path, "HEAD~2", "HEAD", "sub"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), format!("+Subproject commit {new}\n"));
}

#[test]
fn diff_of_a_file_replaced_by_a_submodule_diffs_the_file_and_prints_the_gitlink() {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "main"]);
    fx.write("lib", b"a file\n");
    fx.commit("file", "2024-01-01T00:00:00+0000");
    fx.git(&["rm", "-q", "--cached", "lib"]);
    let commit = "1111111111111111111111111111111111111111";
    let gitlink = format!("160000,{commit},lib");
    fx.git(&["update-index", "--add", "--cacheinfo", &gitlink]);
    let date = "2024-01-02T00:00:00+0000";
    fx.git_with_env(
        &["commit", "-q", "-m", "submodule"],
        &[("GIT_AUTHOR_DATE", date), ("GIT_COMMITTER_DATE", date)],
    );
    let path = fx.path().to_str().unwrap();

    let out = rc_git(&["diff", path, "HEAD~1", "HEAD", "lib"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("@@ -1,1 +0,0 @@\n-a file\n+Subproject commit {commit}\n")
    );

    let out = rc_git(&["diff", path, "HEAD", "HEAD~1", "lib"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("-Subproject commit {commit}\n@@ -0,0 +1,1 @@\n+a file\n")
    );
}

#[test]
fn diff_marks_a_line_without_a_final_newline_as_git_does() {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "main"]);
    fx.write("f.txt", b"x\na");
    fx.commit("one", "2024-01-01T00:00:00+0000");
    fx.write("f.txt", b"x\na\n");
    fx.commit("two", "2024-01-02T00:00:00+0000");
    let path = fx.path().to_str().unwrap();

    let out = rc_git(&["diff", path, "HEAD~1", "HEAD", "f.txt"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let hunk = "@@ -1,2 +1,2 @@\n x\n-a\n\\ No newline at end of file\n+a\n";
    assert_eq!(stdout(&out), hunk);
    let git_diff = fx.git_output(&["diff", "HEAD~1", "HEAD"]);
    assert!(git_diff.ends_with(hunk.trim_end()), "{git_diff}");
}

#[test]
fn diff_keeps_the_carriage_return_on_a_crlf_line_as_git_does() {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "main"]);
    fx.write("f.txt", b"a\nb\n");
    fx.commit("one", "2024-01-01T00:00:00+0000");
    fx.write("f.txt", b"a\r\nb\n");
    fx.commit("two", "2024-01-02T00:00:00+0000");
    let path = fx.path().to_str().unwrap();

    let out = rc_git(&["diff", path, "HEAD~1", "HEAD", "f.txt"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let hunk = "@@ -1,2 +1,2 @@\n-a\n+a\r\n b\n";
    assert_eq!(stdout(&out), hunk);
    let git_diff = fx.git_output(&["diff", "HEAD~1", "HEAD"]);
    assert!(git_diff.ends_with(hunk.trim_end()), "{git_diff:?}");
}

#[test]
fn diff_of_one_commit_uses_first_parent_or_nothing_for_a_root() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();

    let out = rc_git(&["diff", path, "v0.9"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "A\tREADME.md\nA\tbin.dat\nA\tsrc/main.rs\n");

    let out = rc_git(&["diff", path, "main"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        stdout(&rc_git(&["diff", path, "main~1", "main"]))
    );
    assert!(stdout(&out).contains("A\tdocs/guide.md\n"));
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
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("blame needs <rev> <path>"));
}

#[test]
fn blame_since_marks_lines_older_than_the_floor() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    let unbounded = stdout(&rc_git(&["blame", path, "main", "README.md"]));
    let out = rc_git(&["blame", path, "main", "README.md", "--since", "0"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        unbounded,
        "a floor below every commit changes nothing"
    );

    let out = rc_git(&[
        "blame",
        path,
        "main",
        "README.md",
        "--since",
        &i64::MAX.to_string(),
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let marked = format!("^{}\t1\t3\t", fx.rev_parse("main"));
    assert!(stdout(&out).starts_with(&marked), "{}", stdout(&out));
    assert_eq!(stdout(&out).lines().count(), 1);

    let out = rc_git(&["blame", path, "main", "README.md", "--since", "yesterday"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("--since value 'yesterday'"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn blame_declines_a_file_over_the_default_limit() {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "main"]);
    let size = 8 * 1024 * 1024 + 1;
    fx.write("big.txt", &vec![b'x'; size]);
    fx.commit("big", "2024-01-01T00:00:00+0000");
    let path = fx.path().to_str().unwrap();

    let out = rc_git(&["blame", path, "main", "big.txt"]);
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(out.stdout.len(), 0);
    assert!(
        stderr(&out).contains(&format!("is {size} bytes, over the 8388608 byte limit")),
        "{}",
        stderr(&out)
    );
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
fn status_prints_the_kind_of_missing_entries() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.remove("docs");
    fx.remove("Zeta.txt");

    let out = rc_git(&["status", path]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(text.contains("D\ttree\tdocs\n"), "{text}");
    assert!(text.contains("D\tblob\tZeta.txt\n"), "{text}");
}

#[test]
fn an_unreadable_index_keeps_ignore_rules_and_warns() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.write(".gitignore", b".env\n");
    fx.commit("ignore secrets", "2024-01-05T00:00:00Z");
    fx.write(".env", b"S=1\n");
    let index = std::fs::read(fx.git_dir().join("index")).unwrap();
    fx.write(".git/index", &index[..40]);

    for verb in ["changes", "status"] {
        let out = rc_git(&[verb, path]);
        assert!(out.status.success(), "{verb}: {}", stderr(&out));
        assert!(
            stderr(&out).starts_with("warning: read index '"),
            "{verb}: {}",
            stderr(&out)
        );
    }
    assert_eq!(stdout(&rc_git(&["changes", path])), "");
    assert!(stdout(&rc_git(&["status", path])).contains("!\tblob\t.env\n"));
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
    assert!(stderr(&out).contains("warning: truncated after scanning 1 entries"));

    let out = rc_git(&["changes", path, "--bogus"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("--bogus"));
}

#[test]
fn worktree_verbs_list_untracked_files_on_an_unborn_branch() {
    let fx = Fixture::empty_dir();
    fx.git(&["init", "-q", "-b", "main"]);
    fx.write("a.txt", b"hi\n");
    let path = fx.path().to_str().unwrap();

    for (args, expected) in [
        (&["changes", path][..], "?\ta.txt\n"),
        (&["files", path][..], "a.txt\n"),
        (&["status", path][..], "?\tblob\ta.txt\n"),
    ] {
        let out = rc_git(args);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        assert_eq!(stdout(&out), expected, "{args:?}");
    }
    let out = rc_git(&["diff", path, "--worktree", "a.txt"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("+hi\n"), "{}", stdout(&out));
}

#[test]
fn an_unreadable_directory_is_a_warning_and_the_rest_is_listed() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.write("locked/secret.txt", b"s\n");
    fx.write("new.txt", b"n\n");
    fx.set_mode("locked", 0o000);
    let changes = rc_git(&["changes", path]);
    let files = rc_git(&["files", path]);
    fx.set_mode("locked", 0o755);

    assert!(changes.status.success(), "{}", stderr(&changes));
    assert_eq!(stdout(&changes), "?\tnew.txt\n");
    assert!(
        stderr(&changes).starts_with("warning: skipped 'locked': read directory: "),
        "{}",
        stderr(&changes)
    );
    assert!(files.status.success(), "{}", stderr(&files));
    assert!(stdout(&files).contains("new.txt\n"));
    assert!(
        stderr(&files).starts_with("warning: skipped 'locked': read directory: "),
        "{}",
        stderr(&files)
    );
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
    assert!(stderr(&out).contains("warning: truncated after scanning 1 entries"));

    let out = rc_git(&["files", path, "--bogus"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("--bogus"));
}

#[test]
fn a_newline_in_a_name_is_quoted_not_split_into_two_records() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.write("evil\nREADME.md", b"x\n");

    let out = rc_git(&["files", path]);
    assert!(out.status.success());
    assert_eq!(
        stdout(&out),
        "README.md\nZeta.txt\nalpha.txt\nbin.dat\ndocs/guide.md\n\"evil\\nREADME.md\"\nlink\n\
         src/lib.rs\nsrc/main.rs\n"
    );

    let out = rc_git(&["changes", path]);
    assert!(out.status.success());
    assert_eq!(stdout(&out), "?\t\"evil\\nREADME.md\"\n");

    let out = rc_git(&["status", path]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("?\tblob\t\"evil\\nREADME.md\"\n"));

    let out = rc_git(&["files", path, "-z"]);
    assert!(out.status.success());
    let records: Vec<&[u8]> = out.stdout.split(|byte| *byte == 0).collect();
    assert_eq!(records.get(5).copied(), Some(b"evil\nREADME.md".as_slice()));
    assert_eq!(
        records.last().copied(),
        Some(b"".as_slice()),
        "every record ends in NUL"
    );
    let out = rc_git(&["changes", path, "-z"]);
    assert_eq!(out.stdout, b"?\tevil\nREADME.md\0");
    let out = rc_git(&["status", path, "-z"]);
    assert!(
        out.stdout
            .split(|byte| *byte == 0)
            .any(|record| record == b"?\tblob\tevil\nREADME.md")
    );

    fx.write("tab\there \"q\".txt", b"x\n");
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", "odd names"]);
    let out = rc_git(&["ls", path]);
    assert!(stdout(&out).contains("\t\"evil\\nREADME.md\"\n"));
    assert!(stdout(&out).contains("\t\"tab\\there \\\"q\\\".txt\"\n"));
    let out = rc_git(&["diff", path, "main~1", "main"]);
    assert_eq!(
        stdout(&out),
        "A\t\"evil\\nREADME.md\"\nA\t\"tab\\there \\\"q\\\".txt\"\n"
    );
}

#[test]
fn a_wrapped_or_tabbed_title_stays_one_record() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.write("wrapped.txt", b"x\n");
    fx.commit("first line\nsecond\tline\n\nBody", "2024-01-05T00:00:00Z");
    let head = fx.rev_parse("main");

    let out = stdout(&rc_git(&["log", path, "-n", "1"]));
    assert_eq!(
        out,
        format!("{head}\t1704412800\tTest Author\tfirst line second line\n")
    );
    let out = stdout(&rc_git(&["blame", path, "main", "wrapped.txt"]));
    assert_eq!(
        out,
        format!("{head}\t1\t1\tTest Author\tfirst line second line\n")
    );
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

    let out = rc_git(&["diff", path, "--worktree", "src"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("'src' is a directory"),
        "{}",
        stderr(&out)
    );

    let out = rc_git(&["diff", path, "--worktree"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("diff --worktree needs <path>"));
}

#[test]
fn diff_worktree_of_a_file_deleted_from_disk_prints_its_removal() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.remove("alpha.txt");

    let out = rc_git(&["diff", path, "--worktree", "alpha.txt"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.starts_with("@@ "), "{text}");
    assert!(text.contains("-alpha\n"), "{text}");

    let out = rc_git(&["diff", path, "--worktree", "never-tracked.txt"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("'never-tracked.txt'"),
        "{}",
        stderr(&out)
    );
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
    fx.write("notes.txt", b"untracked\n");
    let out = rc_git(&["discard", path, "--yes", "alpha.txt", "notes.txt"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "restored\talpha.txt\ndeleted\tnotes.txt\n");
    assert_eq!(fx.git_output(&["status", "--porcelain"]), "");

    fx.write("alpha.txt", b"precious\n");
    let out = rc_git(&["discard", path, "--yes", "alpha.txt", "docs"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("'docs'"), "{}", stderr(&out));
    assert_eq!(fx.git_output(&["status", "--porcelain"]), " M alpha.txt");
}

#[test]
fn commit_without_identity_names_the_repository_config_and_author_flag() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    let head = fx.rev_parse("HEAD");
    fx.write("alpha.txt", b"no identity\n");

    let out = rc_git(&["commit", path, "-m", "nobody", "alpha.txt"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let message = stderr(&out);
    assert!(
        message.starts_with("error: commit: no author identity"),
        "{message}"
    );
    assert!(
        message.contains(".git/config (not ~/.gitconfig)"),
        "{message}"
    );
    assert!(
        message
            .trim_end()
            .ends_with("or pass an identity (--author \"Name <email>\")"),
        "{message}"
    );
    assert_eq!(
        fx.rev_parse("HEAD"),
        head,
        "a refused commit writes nothing"
    );
}

#[test]
fn commit_expect_head_refuses_when_head_moved() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    let seen = fx.rev_parse("HEAD");
    fx.commit_with_body(
        "Made elsewhere",
        "Another process.",
        "2024-03-01T00:00:00+0000",
    );
    let moved = fx.rev_parse("HEAD");
    fx.write("alpha.txt", b"agent edit\n");

    let out = rc_git(&[
        "commit",
        path,
        "-m",
        "stale",
        "--expect-head",
        &seen,
        "alpha.txt",
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("changed since"), "{}", stderr(&out));
    assert_eq!(fx.rev_parse("HEAD"), moved, "a stale commit writes nothing");

    let out = rc_git(&[
        "commit",
        path,
        "-m",
        "x",
        "--expect-head",
        "abc123",
        "alpha.txt",
    ]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("'abc123'"), "{}", stderr(&out));

    let out = rc_git(&[
        "commit",
        path,
        "-m",
        "fresh",
        "--author",
        "CLI User <cli@example.com>",
        "--expect-head",
        &moved,
        "alpha.txt",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(fx.rev_parse("HEAD~1"), moved);
}

#[test]
fn discard_needs_an_explicit_yes_and_rejects_unknown_flags() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.write("alpha.txt", b"keep this edit\n");
    let edit = || std::fs::read(fx.path().join("alpha.txt")).unwrap();

    for args in [
        ["discard", path, "--dry-run", "alpha.txt"].as_slice(),
        ["discard", path, "alpha.txt"].as_slice(),
        ["discard", path, "--confirm", "yes", "alpha.txt"].as_slice(),
        ["discard", path, "--yes"].as_slice(),
    ] {
        let out = rc_git(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
        assert_eq!(edit(), b"keep this edit\n", "{args:?}");
    }
    assert!(stderr(&rc_git(&["discard", path, "--dry-run", "alpha.txt"])).contains("'--dry-run'"));

    let out = rc_git(&["discard", path, "--confirm", "discard", "alpha.txt"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(fx.git_output(&["status", "--porcelain"]), "");

    fx.write("-x.txt", b"untracked\n");
    let out = rc_git(&["discard", path, "--yes", "--", "-x.txt"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        !fx.path().join("-x.txt").exists(),
        "`--` passes a dash-named path"
    );
}

#[test]
fn unknown_flags_and_surplus_arguments_are_usage_errors() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    for (args, token) in [
        (["refs", path, "--json"].as_slice(), "--json"),
        (["head", path, "extra"].as_slice(), "extra"),
        (["stash", path, "extra"].as_slice(), "extra"),
        (["default-branch", path, "extra"].as_slice(), "extra"),
        (["log", path, "--oneline"].as_slice(), "--oneline"),
        (
            ["log", path, "main", "main~1", "-n", "1"].as_slice(),
            "main~1",
        ),
        (["search", path, "--grep", "x"].as_slice(), "--grep"),
        (
            ["filelog", path, "README.md", "main", "extra"].as_slice(),
            "extra",
        ),
        (["ls", path, "HEAD", "src", "extra"].as_slice(), "extra"),
        (
            ["cat", path, "HEAD", "README.md", "10", "extra"].as_slice(),
            "extra",
        ),
        (["status", path, "src", "extra"].as_slice(), "extra"),
        (
            ["blame", path, "main", "README.md", "extra"].as_slice(),
            "extra",
        ),
        (
            ["commit", path, "-m", "x", "--amend", "alpha.txt"].as_slice(),
            "--amend",
        ),
        (["stash-drop", path, "0", "1"].as_slice(), "1"),
        (["checkout", path, "main", "feature"].as_slice(), "feature"),
        (["fetch", path, "--bogus"].as_slice(), "--bogus"),
        (["push", path, "origin"].as_slice(), "origin"),
    ] {
        let out = rc_git(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
        assert!(
            stderr(&out).contains(&format!("'{token}'")),
            "{args:?}: {}",
            stderr(&out)
        );
        assert_eq!(stdout(&out), "", "{args:?}");
    }

    let cat = rc_git(&["cat", path]);
    assert_eq!(cat.status.code(), Some(2));
    assert!(
        stderr(&cat).contains("cat needs <rev> <path>"),
        "{}",
        stderr(&cat)
    );
}

#[test]
fn help_anywhere_prints_usage_to_stdout() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.write("--help", b"untracked\n");
    for args in [
        ["-h"].as_slice(),
        ["--time", "--help"].as_slice(),
        ["log", "--help"].as_slice(),
        ["discard", path, "--help"].as_slice(),
    ] {
        let out = rc_git(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", stderr(&out));
        assert!(stdout(&out).starts_with("usage: rc-git"), "{args:?}");
        assert_eq!(stderr(&out), "", "{args:?}");
    }
    assert!(
        fx.path().join("--help").exists(),
        "help never runs the verb"
    );
}

#[test]
fn discard_refuses_a_path_through_a_symlinked_directory() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    let outside = Fixture::empty_dir();
    outside.write("outside.txt", b"keep me\n");
    fx.symlink(outside.path().to_str().unwrap(), "up");

    let out = rc_git(&["discard", path, "--yes", "up/outside.txt"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("up/outside.txt"), "{}", stderr(&out));
    assert_eq!(
        std::fs::read(outside.path().join("outside.txt")).unwrap(),
        b"keep me\n"
    );

    let out = rc_git(&["discard", path, "--yes", ".git/config"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(fx.git_dir().join("config").exists());
}

#[test]
fn the_stash_verbs_round_trip() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.write("alpha.txt", b"stash me\n");

    let out = rc_git(&[
        "stash-push",
        path,
        "--yes",
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
    assert_eq!(
        stderr(&out),
        "error: stash drop 'refs/stash': no typed confirmation was given; \
         type 'drop' to confirm (rerun with --confirm drop)\n"
    );
    let out = rc_git(&["stash-drop", path, "0", "--confirm", "drpo"]);
    assert_eq!(out.status.code(), Some(1), "a wrong phrase is refused");
    assert!(
        stderr(&out).contains("did not match; type 'drop' to confirm (rerun with --confirm drop)"),
        "{}",
        stderr(&out)
    );
    let out = rc_git(&["stash-drop", path, "0", "--confirm", "drop"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "refs/rc-git/backup/stash/0\n");
    assert_eq!(fx.git_output(&["stash", "list"]), "");
}

#[test]
fn commit_rejects_a_malformed_author_and_missing_message() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.write("alpha.txt", b"keep this edit\n");
    let head = fx.rev_parse("HEAD");

    for (args, message) in [
        (
            [
                "commit",
                path,
                "-m",
                "x",
                "--author",
                "No Email",
                "alpha.txt",
            ]
            .as_slice(),
            "--author needs \"Name <email>\", got 'No Email'",
        ),
        (
            ["commit", path, "-m", "x", "--author", "A <a@b", "alpha.txt"].as_slice(),
            "--author needs \"Name <email>\", got 'A <a@b'",
        ),
        (
            [
                "commit",
                path,
                "-m",
                "x",
                "--author",
                "A <b> <c>",
                "alpha.txt",
            ]
            .as_slice(),
            "--author needs \"Name <email>\", got 'A <b> <c>'",
        ),
        (
            ["commit", path, "-m", "x", "--author", " <a@b>", "alpha.txt"].as_slice(),
            "--author needs \"Name <email>\", got ' <a@b>'",
        ),
        (
            ["commit", path, "alpha.txt"].as_slice(),
            "commit needs -m <msg>",
        ),
        (
            ["commit", path, "-m", "x"].as_slice(),
            "commit needs at least one <path>",
        ),
        (
            ["stash-push", path, "--yes"].as_slice(),
            "stash-push needs at least one <path>",
        ),
        (
            ["stash-apply", path, "first"].as_slice(),
            "'first' is not a stash index",
        ),
        (
            ["stash-pop", path, "--yes", "-1"].as_slice(),
            "does not take '-1'",
        ),
        (
            ["stash-drop", path, "one", "--confirm", "drop"].as_slice(),
            "'one' is not a stash index",
        ),
    ] {
        let out = rc_git(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).contains(message), "{args:?}: {}", stderr(&out));
        assert_eq!(stdout(&out), "", "{args:?}");
    }
    assert_eq!(
        fx.rev_parse("HEAD"),
        head,
        "a rejected commit writes nothing"
    );
    assert_eq!(
        std::fs::read(fx.path().join("alpha.txt")).unwrap(),
        b"keep this edit\n"
    );
}

#[test]
fn stash_pop_restores_and_prints_the_backup_ref() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.write("alpha.txt", b"pop me\n");

    let out = rc_git(&[
        "stash-push",
        path,
        "--yes",
        "--author",
        "CLI User <cli@example.com>",
        "alpha.txt",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_ne!(
        std::fs::read(fx.path().join("alpha.txt")).unwrap(),
        b"pop me\n"
    );

    let out = rc_git(&["stash-pop", path, "0", "--yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "refs/rc-git/backup/stash/0\n");
    assert_eq!(
        std::fs::read(fx.path().join("alpha.txt")).unwrap(),
        b"pop me\n"
    );
    assert_eq!(fx.git_output(&["stash", "list"]), "");
}

#[test]
fn stash_push_and_pop_need_an_explicit_yes() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.write("alpha.txt", b"keep this edit\n");
    fx.write("untracked.txt", b"untracked\n");
    let read = |name: &str| std::fs::read(fx.path().join(name)).unwrap();

    for args in [
        ["stash-push", path, "alpha.txt", "untracked.txt"].as_slice(),
        ["stash-push", path, "--confirm", "anything", "alpha.txt"].as_slice(),
    ] {
        let out = rc_git(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).contains("--yes"), "{args:?}: {}", stderr(&out));
        assert_eq!(read("alpha.txt"), b"keep this edit\n", "{args:?}");
        assert_eq!(read("untracked.txt"), b"untracked\n", "{args:?}");
        assert_eq!(fx.git_output(&["stash", "list"]), "", "{args:?}");
    }

    let out = rc_git(&[
        "stash-push",
        path,
        "--yes",
        "--author",
        "CLI User <cli@example.com>",
        "alpha.txt",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let stashed = fx.rev_parse("refs/stash");
    let before = std::fs::read(fx.path().join("alpha.txt")).unwrap();

    for args in [
        ["stash-pop", path].as_slice(),
        ["stash-pop", path, "0"].as_slice(),
        ["stash-apply", path, "--yes"].as_slice(),
    ] {
        let out = rc_git(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
        assert_eq!(read("alpha.txt"), before, "{args:?}");
        assert_eq!(fx.rev_parse("refs/stash"), stashed, "{args:?}");
    }

    let out = rc_git(&["stash-pop", path, "--yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(read("alpha.txt"), b"keep this edit\n");
    assert_eq!(fx.git_output(&["stash", "list"]), "");
}

// --- network verbs against the test HTTP server ---

/// Origin, a served bare copy (pushes enabled), and a clone with `origin` upstream.
fn served_clone() -> (Fixture, Fixture, rc_git_testkit::HttpGitServer, Fixture) {
    let origin = Fixture::standard();
    let (bare, bare_path) = origin.bare_copy();
    let server = rc_git_testkit::HttpGitServer::serve(&bare_path);
    let clone = Fixture::empty_dir();
    clone.git(&["clone", "--quiet", &server.url(), "."]);
    (origin, bare, server, clone)
}

/// Commits one change on `origin` and pushes it into the served bare copy held by `bare`.
fn advance_origin(origin: &Fixture, bare: &Fixture) {
    origin.write("news.txt", b"news\n");
    origin.commit("News on origin", "2024-03-01T00:00:00+0000");
    origin.push_main_to(&bare.path().join("remote.git"));
}

#[test]
fn sync_fetches_pulls_and_pushes() {
    let (origin, bare, _server, clone) = served_clone();
    // Remote gains one commit; local gains none, then commits one after the pull.
    advance_origin(&origin, &bare);
    let path = clone.path().to_str().unwrap();

    let out = rc_git(&["sync", path]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("fetch\trefs/remotes/origin/main"), "{text}");
    assert!(text.contains("pull\tmain\t"), "{text}");
    assert!(text.contains("push\tmain\tup to date"), "{text}");

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
fn sync_pushes_a_branch_the_remote_does_not_have_yet() {
    let (_origin, bare, _server, clone) = served_clone();
    clone.git(&["checkout", "-q", "-b", "newwork"]);
    clone.write("local.txt", b"local\n");
    clone.commit("Local work", "2024-03-02T00:00:00+0000");
    let path = clone.path().to_str().unwrap();

    let out = rc_git(&["sync", path]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("pull\tnewwork\tnothing upstream yet"),
        "{text}"
    );
    assert!(
        text.contains("push\tnewwork\torigin/newwork\t1 commit(s)"),
        "{text}"
    );
    assert_eq!(
        bare.git_output(&["--git-dir", "remote.git", "rev-parse", "newwork"]),
        clone.rev_parse("HEAD")
    );
}

#[test]
fn push_uploads_a_local_commit() {
    let (_origin, bare, _server, clone) = served_clone();
    clone.write("local.txt", b"local\n");
    clone.commit("Local work", "2024-03-02T00:00:00+0000");
    let path = clone.path().to_str().unwrap();

    let out = rc_git(&["push", path]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "push\tmain\torigin/main\t1 commit(s)\n");
    assert_eq!(
        bare.git_output(&["--git-dir", "remote.git", "rev-parse", "main"]),
        clone.rev_parse("HEAD")
    );

    let out = rc_git(&["push", path]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "push\tmain\tup to date\n");
}

#[test]
fn pull_fast_forwards_and_reports_the_file_count() {
    let (origin, bare, _server, clone) = served_clone();
    let old = clone.rev_parse("HEAD");
    advance_origin(&origin, &bare);
    let path = clone.path().to_str().unwrap();

    let out = rc_git(&["pull", path]);
    assert!(out.status.success(), "{}", stderr(&out));
    let new = origin.rev_parse("HEAD");
    assert_eq!(
        stdout(&out),
        format!("fetch\trefs/remotes/origin/main\t{new}\npull\tmain\t{old}\t{new}\t1 file(s)\n")
    );
    assert_eq!(clone.rev_parse("HEAD"), new);
    assert_eq!(
        std::fs::read(clone.path().join("news.txt")).unwrap(),
        b"news\n"
    );

    let out = rc_git(&["pull", path]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "fetch\torigin\tup to date\npull\tmain\tup to date\n"
    );
}

#[test]
fn remote_flags_reject_half_credentials_and_unknown_flags() {
    let (_origin, _bare, _server, clone) = served_clone();
    let path = clone.path().to_str().unwrap();

    for (args, message) in [
        (
            ["fetch", path, "--username"].as_slice(),
            "--username needs a value",
        ),
        (
            ["pull", path, "--token"].as_slice(),
            "--token needs a value",
        ),
        (
            ["push", path, "--ssh-dir"].as_slice(),
            "--ssh-dir needs a value",
        ),
        (
            ["fetch", path, "--bogus"].as_slice(),
            "fetch does not take '--bogus'",
        ),
        (
            ["fetch", path, "--username", "u"].as_slice(),
            "--username was given without --token (or RC_GIT_TOKEN)",
        ),
        (
            ["push", path, "--token", "t"].as_slice(),
            "a token was given without --username",
        ),
    ] {
        let out = rc_git(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).contains(message), "{args:?}: {}", stderr(&out));
        assert_eq!(stdout(&out), "", "{args:?}");
    }
}

#[test]
fn network_verbs_report_auth_with_exit_code_5() {
    let origin = Fixture::standard();
    let (_bare, bare_path) = origin.bare_copy();
    let server = rc_git_testkit::HttpGitServer::serve_with_auth(&bare_path, "user", "sekret");
    let clone = Fixture::basic_auth_clone(&server, "user", "sekret");
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
fn key_passphrase_env_never_answers_an_http_challenge() {
    let origin = Fixture::standard();
    let (_bare, bare_path) = origin.bare_copy();
    let server = rc_git_testkit::HttpGitServer::serve_with_auth(&bare_path, "", "pp");
    let clone = Fixture::basic_auth_clone(&server, "", "pp");
    let path = clone.path().to_str().unwrap();

    let out = rc_git_with_env(&["fetch", path], &[("RC_GIT_KEY_PASSPHRASE", "pp")]);
    assert_eq!(out.status.code(), Some(5), "{}", stderr(&out));
}

#[test]
fn rc_git_token_env_supplies_the_token() {
    let origin = Fixture::standard();
    let (_bare, bare_path) = origin.bare_copy();
    let server = rc_git_testkit::HttpGitServer::serve_with_auth(&bare_path, "user", "sekret");
    let clone = Fixture::basic_auth_clone(&server, "user", "sekret");
    let path = clone.path().to_str().unwrap();
    let env = [("RC_GIT_TOKEN", "sekret")];

    let out = rc_git_with_env(&["fetch", path, "--username", "user"], &env);
    assert!(out.status.success(), "{}", stderr(&out));

    let out = rc_git_with_env(&["fetch", path], &env);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("a token was given without --username"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn pull_and_push_report_divergence_with_exit_7() {
    let (origin, bare, _server, clone) = served_clone();
    advance_origin(&origin, &bare);
    clone.write("local.txt", b"local\n");
    clone.commit("Local work", "2024-03-02T00:00:00+0000");
    let path = clone.path().to_str().unwrap();

    let out = rc_git(&["pull", path]);
    assert_eq!(out.status.code(), Some(7), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("cannot fast-forward"),
        "{}",
        stderr(&out)
    );
    assert!(
        stdout(&out).contains("fetch\trefs/remotes/origin/main\t"),
        "{}",
        stdout(&out)
    );

    let out = rc_git(&["push", path]);
    assert_eq!(out.status.code(), Some(7), "{}", stderr(&out));
    assert!(stderr(&out).contains("push rejected"), "{}", stderr(&out));
}

#[test]
fn sync_on_a_diverged_clone_still_reports_the_fetch_it_ran() {
    let (origin, bare, _server, clone) = served_clone();
    advance_origin(&origin, &bare);
    clone.write("local.txt", b"local\n");
    clone.commit("Local work", "2024-03-02T00:00:00+0000");
    let path = clone.path().to_str().unwrap();
    let served_main = || bare.git_output(&["--git-dir", "remote.git", "rev-parse", "main"]);
    let before = served_main();

    let out = rc_git(&["sync", path]);
    assert_eq!(out.status.code(), Some(7), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("cannot fast-forward"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        stdout(&out),
        format!(
            "fetch\trefs/remotes/origin/main\t{}\n",
            clone.rev_parse("origin/main")
        )
    );
    assert_eq!(served_main(), before, "sync uploaded past a divergence");
    assert_ne!(served_main(), clone.rev_parse("HEAD"));
}

#[test]
fn fetch_on_a_detached_head_fetches_origin() {
    let (origin, bare, _server, clone) = served_clone();
    clone.git(&["checkout", "-q", "--detach"]);
    advance_origin(&origin, &bare);
    let path = clone.path().to_str().unwrap();

    let out = rc_git(&["fetch", path]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        clone.rev_parse("refs/remotes/origin/main"),
        bare.git_output(&["--git-dir", "remote.git", "rev-parse", "main"])
    );
}

#[test]
fn ssh_unknown_host_exits_6_and_accept_hostkey_lets_the_fetch_through() {
    let origin = Fixture::standard();
    let (_bare, bare_path) = origin.bare_copy();
    let server = rc_git_testkit::SshGitServer::serve(&bare_path);
    let (clone, ssh_home) = Fixture::ssh_clone(&bare_path, &server);
    let path = clone.path().to_str().unwrap();
    let ssh_dir = ssh_home.path().to_str().unwrap();

    let out = rc_git(&["fetch", path, "--ssh-dir", ssh_dir]);
    assert_eq!(out.status.code(), Some(6), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&server.host_fingerprint()),
        "{}",
        stderr(&out)
    );

    let accepted = [
        "fetch",
        path,
        "--ssh-dir",
        ssh_dir,
        "--accept-hostkey",
        &server.host_fingerprint(),
    ];
    let out = rc_git(&accepted);
    assert!(out.status.success(), "{}", stderr(&out));

    // An HTTPS token in the environment is not an ssh credential and must not fail the fetch.
    let out = rc_git_with_env(&accepted, &[("RC_GIT_TOKEN", "for-https-only")]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn default_branch_names_the_mainline_or_fails() {
    let fx = Fixture::standard();
    let out = rc_git(&["default-branch", fx.path().to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "main\n");

    let bare = Fixture::empty_dir();
    bare.git(&["init", "-q", "-b", "work"]);
    let out = rc_git(&["default-branch", bare.path().to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(
        stderr(&out).contains("no default branch"),
        "{}",
        stderr(&out)
    );

    bare.git(&["commit", "-q", "--allow-empty", "-m", "init"]);
    bare.git(&["update-ref", "refs/remotes/origin/none", "HEAD"]);
    bare.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/none",
    ]);
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
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("feature.txt"), "{}", stderr(&out));
    assert_eq!(
        fx.git_output(&["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature"
    );

    let out = rc_git(&["checkout", path]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("exactly one branch name"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn checkout_during_a_merge_names_the_requested_branch() {
    let fx = Fixture::standard();
    fx.conflicted_merge("README.md");
    let path = fx.path().to_str().unwrap();

    let out = rc_git(&["checkout", path, "feature"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("checkout 'feature': a merge is in progress"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        fx.git_output(&["rev-parse", "--abbrev-ref", "HEAD"]),
        "main"
    );
}

/// Sends `signal` (a `kill` name such as `INT`) to the process `pid`.
#[expect(clippy::disallowed_methods, reason = "tests signal the built binary")]
fn send_signal(pid: u32, signal: &str) {
    let status = Command::new("kill")
        .args([&format!("-{signal}"), &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "kill -{signal} {pid} failed");
}

/// Waits up to `limit` for `child` to exit, killing it if it does not: a killed child exits by
/// signal, with no code, so the caller's exit-code assertion fails.
fn wait_with_deadline(child: &mut std::process::Child, limit: std::time::Duration) -> Output {
    let deadline = std::time::Instant::now() + limit;
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let mut output = Output {
        status: child.wait().unwrap(),
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    if let Some(mut stderr) = child.stderr.take() {
        std::io::Read::read_to_end(&mut stderr, &mut output.stderr).unwrap();
    }
    output
}

#[test]
fn interrupting_a_stalled_fetch_exits_130() {
    let (_origin, _bare, _server, clone) = served_clone();
    // A remote that accepts the connection and then never says a word: curl blocks in a read
    // the flag cannot reach, and the guard stops waiting for it on the first signal.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/remote.git", silent.local_addr().unwrap());
    clone.git(&["remote", "set-url", "origin", &url]);

    let mut child = rc_git_command()
        .args(["fetch", clone.path().to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Accepting proves the fetch is past the handler setup, so the signal cannot kill it raw.
    let (_held_open, _) = silent.accept().unwrap();
    send_signal(child.id(), "INT");

    let out = wait_with_deadline(&mut child, std::time::Duration::from_secs(5));
    assert_eq!(out.status.code(), Some(130), "{}", stderr(&out));
}

#[test]
fn branch_create_and_tag_create_make_refs_and_never_move_one() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();

    let out = rc_git(&["branch-create", path, "topic"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "created\trefs/heads/topic\n");
    assert_eq!(fx.rev_parse("refs/heads/topic"), fx.rev_parse("HEAD"));

    let out = rc_git(&["tag-create", path, "v0.9-again", "v0.9"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "created\trefs/tags/v0.9-again\n");
    assert_eq!(
        fx.rev_parse("refs/tags/v0.9-again"),
        fx.rev_parse("v0.9^{commit}")
    );

    let feature = fx.rev_parse("feature");
    let out = rc_git(&["branch-create", path, "feature", "main"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert_eq!(
        fx.rev_parse("feature"),
        feature,
        "an existing ref stays put"
    );

    let out = rc_git(&["tag-create", path, "v1.0", "v0.9"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));

    let out = rc_git(&["branch-create", path]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let out = rc_git(&["tag-create", path, "a", "HEAD", "surplus"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}

#[test]
fn branch_delete_of_a_merged_branch_needs_no_phrase_and_writes_no_backup() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.git(&["branch", "merged", "main~1"]);

    let out = rc_git(&["branch-delete", path, "merged"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "deleted\trefs/heads/merged\n");
    assert_eq!(fx.git_output(&["branch", "--list", "merged"]), "");
    assert_eq!(
        fx.git_output(&["for-each-ref", "refs/rc-git/backup/"]),
        "",
        "a merged branch loses nothing, so nothing is backed up"
    );
}

#[test]
fn branch_delete_of_an_unmerged_branch_needs_its_name_and_backs_it_up() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    let tip = fx.rev_parse("feature");

    let out = rc_git(&["branch-delete", path, "feature"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("needs --confirm feature"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        fx.rev_parse("feature"),
        tip,
        "nothing deleted without the phrase"
    );

    let out = rc_git(&["branch-delete", path, "feature", "--confirm", "nope"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("rerun with --confirm feature"),
        "{}",
        stderr(&out)
    );
    assert_eq!(fx.rev_parse("feature"), tip);

    let out = rc_git(&["branch-delete", path, "feature", "--confirm", "feature"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "deleted\trefs/heads/feature\nbackup\trefs/rc-git/backup/branch/feature\n"
    );
    assert_eq!(fx.git_output(&["branch", "--list", "feature"]), "");
    assert_eq!(fx.rev_parse("refs/rc-git/backup/branch/feature"), tip);
}

#[test]
fn branch_delete_refuses_a_protected_or_checked_out_branch() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.git(&["branch", "release/1", "feature"]);
    let tip = fx.rev_parse("release/1");

    let out = rc_git(&["branch-delete", path, "release/1", "--confirm", "release/1"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("protected branch 'release/1'"),
        "{}",
        stderr(&out)
    );
    assert_eq!(fx.rev_parse("release/1"), tip);

    fx.git(&["checkout", "-q", "-b", "current"]);
    let out = rc_git(&["branch-delete", path, "current"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("checked-out branch"),
        "{}",
        stderr(&out)
    );
    assert_eq!(fx.rev_parse("current"), fx.rev_parse("HEAD"));
    assert_eq!(fx.git_output(&["for-each-ref", "refs/rc-git/backup/"]), "");
}

#[test]
fn tag_delete_needs_the_tag_name_and_backs_the_tag_up() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    let tag = fx.rev_parse("refs/tags/v1.0");

    let out = rc_git(&["tag-delete", path, "v1.0"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("needs --confirm v1.0"),
        "{}",
        stderr(&out)
    );
    assert_eq!(fx.rev_parse("refs/tags/v1.0"), tag);

    let out = rc_git(&["tag-delete", path, "v1.0", "--confirm", "v1.0"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "deleted\trefs/tags/v1.0\nbackup\trefs/rc-git/backup/tag/v1.0\n"
    );
    assert_eq!(fx.git_output(&["tag", "--list", "v1.0"]), "");
    assert_eq!(fx.rev_parse("refs/rc-git/backup/tag/v1.0"), tag);

    let out = rc_git(&["tag-delete", path, "missing", "--confirm", "missing"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
}

#[test]
fn plan_delete_previews_what_a_deletion_asks_for() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.git(&["branch", "merged", "main~1"]);
    fx.git(&["branch", "release/1", "main"]);

    let plan = |kind: &str, name: &str| {
        let out = rc_git(&["plan-delete", path, kind, name]);
        assert!(out.status.success(), "{}", stderr(&out));
        stdout(&out)
    };
    assert_eq!(
        plan("branch", "feature"),
        "confirm\tdestructive\tfeature\tyes\tDelete branch 'feature' and its unmerged commits.\n"
    );
    assert_eq!(
        plan("branch", "merged"),
        "confirm\trecoverable\t-\tno\t\
         Delete branch 'merged'. Its commits stay reachable elsewhere.\n"
    );
    assert!(
        plan("branch", "release/1").starts_with("deny\t-\t-\tno\t"),
        "a protected branch is denied"
    );
    assert_eq!(
        plan("tag", "v1.0"),
        "confirm\tdestructive\tv1.0\tyes\tDelete tag 'v1.0'.\n"
    );
    assert_eq!(fx.git_output(&["for-each-ref", "refs/rc-git/backup/"]), "");
    assert!(
        fx.git_output(&["branch", "--list", "merged"])
            .contains("merged")
    );

    let out = rc_git(&["plan-delete", path, "remote", "x"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}

#[test]
fn upstream_reports_the_tracking_branch_or_its_absence() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();

    let out = rc_git(&["upstream", path]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "main\tnone\n");

    fx.git(&["remote", "add", "origin", "https://example.com/repo.git"]);
    fx.git(&["config", "branch.main.remote", "origin"]);
    fx.git(&["config", "branch.main.merge", "refs/heads/main"]);
    fx.git(&["update-ref", "refs/remotes/origin/main", "main~1"]);
    let out = rc_git(&["upstream", path]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "main\torigin\thttps://example.com/repo.git\t1\t0\n"
    );

    let out = rc_git(&["upstream", path, "surplus"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}

/// Twenty numbered lines, so an edit near each end makes two hunks three lines of context
/// cannot join.
fn numbered_lines(edits: &[(usize, &str)]) -> Vec<u8> {
    let mut text = String::new();
    for line in 1..=20 {
        let default = format!("line {line}");
        let replacement = edits.iter().find(|(at, _)| *at == line);
        text.push_str(replacement.map_or(default.as_str(), |(_, edit)| edit));
        text.push('\n');
    }
    text.into_bytes()
}

/// The `<path>:a,b,c,d` selector for each hunk header `diff --worktree` prints for `file`.
fn hunk_selectors(path: &str, file: &str) -> Vec<String> {
    let diff = stdout(&rc_git(&["diff", path, "--worktree", file]));
    let mut selectors = Vec::new();
    for line in diff.lines() {
        let Some(ranges) = line
            .strip_prefix("@@ -")
            .and_then(|rest| rest.strip_suffix(" @@"))
        else {
            continue;
        };
        let (old, new) = ranges.split_once(" +").unwrap();
        selectors.push(format!("{file}:{old},{new}"));
    }
    selectors
}

#[test]
fn commit_and_discard_take_single_hunks() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.write("numbers.txt", &numbered_lines(&[]));
    fx.commit("numbers", "2024-01-04T00:00:00+0000");
    fx.write(
        "numbers.txt",
        &numbered_lines(&[(2, "two"), (19, "nineteen")]),
    );
    let hunks = hunk_selectors(path, "numbers.txt");
    assert_eq!(hunks.len(), 2, "{hunks:?}");

    let out = rc_git(&[
        "commit",
        path,
        "-m",
        "first hunk",
        "--author",
        "CLI User <cli@example.com>",
        "--hunk",
        &hunks[0],
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let committed = String::from_utf8(numbered_lines(&[(2, "two")])).unwrap();
    assert_eq!(
        fx.git_output(&["show", "HEAD:numbers.txt"]),
        committed.trim_end(),
        "only the first hunk is committed"
    );

    let rest = hunk_selectors(path, "numbers.txt");
    assert_eq!(rest.len(), 1, "{rest:?}");
    let out = rc_git(&["discard", path, "--hunk", &rest[0]]);
    assert_eq!(out.status.code(), Some(2), "discard still needs --yes");

    let stale = "numbers.txt:1,1,1,1";
    let out = rc_git(&["discard", path, "--yes", "--hunk", stale]);
    assert_eq!(out.status.code(), Some(8), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("'numbers.txt' changed"),
        "{}",
        stderr(&out)
    );

    let out = rc_git(&["discard", path, "--yes", "--hunk", &rest[0]]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "hunks\t1\tnumbers.txt\n");
    assert_eq!(
        std::fs::read(fx.path().join("numbers.txt")).unwrap(),
        numbered_lines(&[(2, "two")])
    );
    assert_eq!(fx.git_output(&["status", "--porcelain"]), "");
}

#[test]
fn malformed_or_mixed_hunk_selectors_are_usage_errors() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    let head = fx.rev_parse("HEAD");
    for selector in [
        "alpha.txt",
        "alpha.txt:1,2,3",
        "alpha.txt:1,2,3,x",
        ":1,1,1,1",
    ] {
        let out = rc_git(&["commit", path, "-m", "m", "--hunk", selector]);
        assert_eq!(out.status.code(), Some(2), "{selector}: {}", stderr(&out));
    }
    let out = rc_git(&[
        "commit",
        path,
        "-m",
        "m",
        "--hunk",
        "alpha.txt:1,1,1,1",
        "alpha.txt",
    ]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let out = rc_git(&["commit", path, "-m", "m"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let out = rc_git(&[
        "discard",
        path,
        "--yes",
        "--hunk",
        "alpha.txt:1,1,1,1",
        "README.md",
    ]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert_eq!(fx.rev_parse("HEAD"), head);
}

#[test]
fn highlight_and_links_print_spans_and_honor_the_app_limit() {
    let fx = Fixture::standard();
    let path = fx.path().to_str().unwrap();
    fx.write("web/app.js", b"import x from \"./util\";\nx();\n");
    fx.write("big.rs", &vec![b'a'; 2 * 1024 * 1024 + 1]);
    fx.commit("web", "2024-01-04T00:00:00+0000");

    let out = rc_git(&["--time", "highlight", path, "HEAD", "src/main.rs"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).starts_with("0\t2\tkeyword\n"),
        "{}",
        stdout(&out)
    );
    assert!(stderr(&out).contains(", highlight "), "{}", stderr(&out));

    let out = rc_git(&["links", path, "HEAD", "web/app.js"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let links = stdout(&out);
    assert!(
        links.contains("\t15\t21\n"),
        "the ./util specifier: {links}"
    );

    let out = rc_git(&["links", path, "HEAD", "src/main.rs"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "", "rust has no import links");

    let out = rc_git(&["highlight", path, "HEAD", "big.rs"]);
    assert_eq!(out.status.code(), Some(4), "{}", stderr(&out));
    let out = rc_git(&["highlight", path, "HEAD", "bin.dat"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("no language"), "{}", stderr(&out));
    let out = rc_git(&["highlight", path, "HEAD", "src/missing.rs"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
}
