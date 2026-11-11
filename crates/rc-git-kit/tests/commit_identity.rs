//! Ported from `CommitIdentityTests.swift` (parse, store shape, author label).

use rc_git_kit::commit_identity::{
    CommitAuthor, CommitIdentity, IdentityField, IdentityProblem, SavedIdentities, SavedIdentity,
    standardized_path,
};

fn identity(name: &str, email: &str) -> CommitIdentity {
    CommitIdentity {
        name: name.to_owned(),
        email: email.to_owned(),
    }
}

#[test]
fn a_valid_identity_is_trimmed_of_spaces_and_newlines() {
    let parsed = CommitIdentity::parse("  Ada Lovelace\n", "\tlong@example.com \n");
    assert_eq!(parsed, Ok(identity("Ada Lovelace", "long@example.com")));
}

#[test]
fn trimming_covers_unicode_spaces_but_not_zero_width_ones() {
    let parsed = CommitIdentity::parse("\u{a0}Long\u{3000}", "\u{2028}a@b.c\u{85}");
    assert_eq!(parsed, Ok(identity("Long", "a@b.c")));
    let kept = CommitIdentity::parse("\u{200b}Long", "a@b.c");
    assert_eq!(kept, Ok(identity("\u{200b}Long", "a@b.c")));
}

#[test]
fn angle_brackets_in_the_name_are_refused() {
    let refused = Err(IdentityProblem::ForbiddenCharacter(IdentityField::Name));
    assert_eq!(
        CommitIdentity::parse("Long <long@example.com>", "x@example.com"),
        refused
    );
    for name in ["Long <", "Long >"] {
        assert_eq!(
            CommitIdentity::parse(name, "x@example.com"),
            refused,
            "{name}"
        );
    }
}

#[test]
fn angle_brackets_around_the_email_are_refused() {
    assert_eq!(
        CommitIdentity::parse("Long", "<long@example.com>"),
        Err(IdentityProblem::ForbiddenCharacter(IdentityField::Email))
    );
}

#[test]
fn an_embedded_newline_or_control_character_is_refused() {
    assert_eq!(
        CommitIdentity::parse("Ada\nLovelace", "long@example.com"),
        Err(IdentityProblem::ForbiddenCharacter(IdentityField::Name))
    );
    assert_eq!(
        CommitIdentity::parse("Long", "long@\u{7}example.com"),
        Err(IdentityProblem::ForbiddenCharacter(IdentityField::Email))
    );
    assert_eq!(
        CommitIdentity::parse("Long\u{9f}", "a@b.c"),
        Err(IdentityProblem::ForbiddenCharacter(IdentityField::Name)),
        "C1 controls are controls too"
    );
}

#[test]
fn empty_fields_are_refused_after_trimming() {
    assert_eq!(
        CommitIdentity::parse("  ", "a@b.c"),
        Err(IdentityProblem::Empty(IdentityField::Name))
    );
    assert_eq!(
        CommitIdentity::parse("Long", "\n"),
        Err(IdentityProblem::Empty(IdentityField::Email))
    );
    assert_eq!(
        CommitIdentity::parse("", "<"),
        Err(IdentityProblem::Empty(IdentityField::Name)),
        "emptiness is reported before a forbidden character"
    );
}

#[test]
fn non_ascii_names_and_emoji_joiners_are_kept() {
    let name = "Nguyễn Văn A 👩‍💻";
    assert_eq!(
        CommitIdentity::parse(name, "long@example.com"),
        Ok(identity(name, "long@example.com"))
    );
}

#[test]
fn each_problem_has_its_sheet_message() {
    assert_eq!(
        IdentityProblem::Empty(IdentityField::Name).message(),
        "Enter a name."
    );
    assert_eq!(
        IdentityProblem::Empty(IdentityField::Email).to_string(),
        "Enter an email address."
    );
    assert!(
        IdentityProblem::ForbiddenCharacter(IdentityField::Name)
            .message()
            .starts_with("The name cannot contain")
    );
    assert!(
        IdentityProblem::ForbiddenCharacter(IdentityField::Email)
            .message()
            .starts_with("Enter the email without")
    );
}

#[test]
fn an_identity_is_saved_for_one_repository_only() {
    let work = identity("Work Me", "me@work.example");
    let mut saved = SavedIdentities::default();
    saved.save(&work, "/repos/work");
    assert_eq!(saved.identity("/repos/work"), Some(work.clone()));
    assert_eq!(
        saved.identity("/repos/work/"),
        Some(work.clone()),
        "spelling does not split keys"
    );
    assert_eq!(saved.identity("/repos/./x/../work"), Some(work));
    assert_eq!(saved.identity("/repos/other"), None);
    saved.clear("/repos/work/");
    assert_eq!(saved.identity("/repos/work"), None);
}

#[test]
fn a_stored_value_that_no_longer_parses_reads_as_absent() {
    let mut saved = SavedIdentities::default();
    saved.entries.insert(
        "/bad".to_owned(),
        SavedIdentity {
            name: Some("Long <a@b.c>".to_owned()),
            email: Some("a@b.c".to_owned()),
        },
    );
    saved.entries.insert(
        "/half".to_owned(),
        SavedIdentity {
            name: Some("Only Name".to_owned()),
            email: None,
        },
    );
    assert_eq!(saved.identity("/bad"), None);
    assert_eq!(saved.identity("/half"), None);
    let work = identity("Work Me", "me@work.example");
    saved.save(&work, "/fresh");
    assert_eq!(saved.identity("/fresh"), Some(work));
}

#[test]
fn standardizing_is_lexical_and_keeps_private() {
    assert_eq!(standardized_path("/repos/work/"), "/repos/work");
    assert_eq!(standardized_path("/a/./b//c/"), "/a/b/c");
    assert_eq!(standardized_path("/a/b/../c"), "/a/c");
    assert_eq!(standardized_path("/.."), "/");
    assert_eq!(standardized_path("/"), "/");
    assert_eq!(standardized_path(""), "");
    assert_eq!(
        standardized_path("a/../b/"),
        "a/../b",
        "a relative .. is left alone"
    );
    assert_eq!(standardized_path("/private/var/x"), "/private/var/x");
}

#[test]
fn the_repository_config_beats_the_saved_identity() {
    let configured = identity("Repo", "repo@example.com");
    let saved = identity("Saved", "saved@example.com");
    assert_eq!(
        CommitAuthor::resolve(Some(configured.clone()), Some(saved.clone())),
        CommitAuthor::Configured(configured)
    );
    assert_eq!(
        CommitAuthor::resolve(None, Some(saved.clone())),
        CommitAuthor::Saved(saved)
    );
    assert_eq!(CommitAuthor::resolve(None, None), CommitAuthor::Unset);
}

#[test]
fn the_label_names_the_author_and_where_it_came_from() {
    assert_eq!(
        CommitAuthor::Configured(identity("Repo", "repo@example.com")).label(),
        "Author: Repo <repo@example.com> (repository config)"
    );
    assert_eq!(
        CommitAuthor::Saved(identity("Saved", "saved@example.com")).label(),
        "Author: Saved <saved@example.com> (saved in rc-git)"
    );
    assert_eq!(
        CommitAuthor::Unset.label(),
        "Author: none set; Commit asks for one"
    );
    assert_eq!(
        CommitAuthor::Unreadable("cannot open /repo".to_owned()).label(),
        "Author: could not read this repository’s config"
    );
}
