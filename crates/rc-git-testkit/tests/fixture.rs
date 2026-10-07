//! `Fixture::clone_of`: a copy must read exactly like a fresh build.

use std::os::unix::fs::PermissionsExt;

use rc_git_testkit::Fixture;

#[test]
fn clone_of_keeps_symlinks_modes_and_a_clean_status() {
    let template = Fixture::standard();
    template.set_mode("src/main.rs", 0o755);
    template.git(&["add", "src/main.rs"]);

    let copy = Fixture::clone_of(&template);

    let link = std::fs::symlink_metadata(copy.path().join("link")).unwrap();
    assert!(link.file_type().is_symlink(), "link is still a symlink");
    assert_eq!(
        std::fs::read_link(copy.path().join("link")).unwrap(),
        std::path::Path::new("README.md")
    );
    let mode = std::fs::metadata(copy.path().join("src/main.rs"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755, "permission bits survive the copy");

    assert_eq!(
        copy.git_output(&["status", "--porcelain"]),
        "M  src/main.rs",
        "only the staged mode change shows, as in the template"
    );
    assert_eq!(copy.git_output(&["diff-files", "--name-only"]), "");
    assert_eq!(copy.rev_parse("v1.0"), template.rev_parse("v1.0"));
}

#[test]
fn clone_of_is_independent_of_its_template() {
    let template = Fixture::standard();
    let copy = Fixture::clone_of(&template);
    copy.write("new.txt", b"new\n");
    copy.commit("Only in the copy", "2024-02-01T00:00:00+0000");

    assert_ne!(copy.rev_parse("main"), template.rev_parse("main"));
    assert!(!template.path().join("new.txt").exists());
}
