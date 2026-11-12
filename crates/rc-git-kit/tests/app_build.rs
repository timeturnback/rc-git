//! Ported from `AppBuildTests.swift`.

use std::collections::HashMap;

use rc_git_kit::app_build::{AppBuild, AppVariant, BuildDate, UpdateMode};

fn stamped(overrides: &[(&str, &str)]) -> HashMap<String, String> {
    let mut info: HashMap<String, String> = [
        ("CFBundleName", "rc-git"),
        ("CFBundleShortVersionString", "0.1.0"),
        ("CFBundleVersion", "42"),
        ("RCGitVariant", "release"),
        ("RCGitSourceRoot", "/Users/dev/rc-git"),
        (
            "RCGitSourceCommit",
            "a3f91c2d4e5f60718293a4b5c6d7e8f901234567",
        ),
        ("RCGitSourceDirty", "false"),
        ("RCGitBuildDate", "2026-09-19T11:02:03Z"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect();
    for (key, value) in overrides {
        info.insert((*key).to_owned(), (*value).to_owned());
    }
    info
}

#[test]
fn reads_a_release_stamp() {
    let build = AppBuild::from_info(&stamped(&[]));
    assert_eq!(build.variant, AppVariant::Release);
    assert_eq!(build.name, "rc-git");
    assert_eq!(build.source_root.as_deref(), Some("/Users/dev/rc-git"));
    assert_eq!(build.short_commit(), Some("a3f91c2"));
    assert!(!build.dirty);
    assert_eq!(build.update_mode(), Some(UpdateMode::Update));
    assert_eq!(
        build.relaunch_path().as_deref(),
        Some("/Applications/RCGit.app")
    );
    assert_eq!(build.version_line(), "0.1.0 (42) · a3f91c2");
    let date = build.build_date.unwrap();
    assert_eq!(date.raw, "2026-09-19T11:02:03Z");
    assert_eq!(
        (
            date.year,
            date.month,
            date.day,
            date.hour,
            date.minute,
            date.second
        ),
        (2026, 9, 19, 11, 2, 3)
    );
}

#[test]
fn a_build_date_parses_only_the_stamped_utc_form() {
    for bad in [
        "",
        "2026-09-19",
        "2026-09-19T11:02:03",
        "2026-09-19T11:02:03+02:00",
        "2026-09-19 11:02:03Z",
        "2026-13-19T11:02:03Z",
        "2026-00-19T11:02:03Z",
        "2026-09-32T11:02:03Z",
        "2026-09-19T24:02:03Z",
        "2026-09-19T11:60:03Z",
        "2026-09-19T11:02:61Z",
        "2O26-09-19T11:02:03Z",
        "+026-09-19T11:02:03Z",
    ] {
        assert_eq!(BuildDate::parse(bad), None, "{bad}");
    }
    assert!(BuildDate::parse("2026-12-31T23:59:60Z").is_some());
    let build = AppBuild::from_info(&stamped(&[("RCGitBuildDate", "yesterday")]));
    assert_eq!(build.build_date, None);
}

#[test]
fn a_dirty_build_marks_its_commit() {
    let build = AppBuild::from_info(&stamped(&[("RCGitSourceDirty", "true")]));
    assert!(build.dirty);
    assert_eq!(build.short_version(), "0.1.0+");
    assert_eq!(build.version_line(), "0.1.0 (42) · a3f91c2+");
}

#[test]
fn a_commit_count_version_drops_the_repeated_build_number() {
    let build = AppBuild::from_info(&stamped(&[("CFBundleShortVersionString", "0.1.42")]));
    assert_eq!(build.short_version(), "0.1.42");
    assert_eq!(build.version_line(), "0.1.42 · a3f91c2");
}

#[test]
fn a_different_build_number_stays() {
    let build = AppBuild::from_info(&stamped(&[("CFBundleShortVersionString", "0.1.420")]));
    assert_eq!(build.version_line(), "0.1.420 (42) · a3f91c2");
}

#[test]
fn the_dev_bundle_rebuilds_itself_in_place() {
    let build = AppBuild::from_info(&stamped(&[
        ("RCGitVariant", "dev"),
        ("CFBundleName", "rc-git (dev)"),
    ]));
    assert_eq!(build.variant, AppVariant::Dev);
    assert_eq!(build.name, "rc-git (dev)");
    assert_eq!(build.update_mode(), Some(UpdateMode::Rebuild));
    assert_eq!(
        build.relaunch_path().as_deref(),
        Some("/Users/dev/rc-git/RCGit-dev.app")
    );
}

#[test]
fn an_unstamped_bundle_degrades_to_dev() {
    let build = AppBuild::from_info(&HashMap::new());
    assert_eq!(build.variant, AppVariant::Dev);
    assert_eq!(build.source_root, None);
    assert_eq!(build.commit, None);
    assert_eq!(build.short_commit(), None);
    assert_eq!(build.build_date, None);
    assert_eq!(build.update_mode(), None);
    assert_eq!(build.relaunch_path(), None);
    assert_eq!(build.version_line(), "0 (0)");
}

#[test]
fn empty_values_and_an_unknown_variant_read_as_unstamped() {
    let build = AppBuild::from_info(&stamped(&[
        ("RCGitVariant", "nightly"),
        ("RCGitSourceRoot", ""),
        ("CFBundleName", ""),
    ]));
    assert_eq!(build.variant, AppVariant::Dev);
    assert_eq!(build.source_root, None);
    assert_eq!(build.name, "rc-git");
}

#[test]
fn an_unknown_commit_is_no_commit() {
    let build = AppBuild::from_info(&stamped(&[("RCGitSourceCommit", "unknown")]));
    assert_eq!(build.commit, None);
    assert_eq!(build.version_line(), "0.1.0 (42)");
    assert_eq!(build.update_mode(), Some(UpdateMode::Update));
}

#[test]
fn a_short_commit_is_kept_whole() {
    let build = AppBuild::from_info(&stamped(&[("RCGitSourceCommit", "a3f9")]));
    assert_eq!(build.short_commit(), Some("a3f9"));
}

#[test]
fn a_build_date_counts_seconds_from_the_epoch() {
    let date = BuildDate::parse("1970-01-02T00:00:01Z");
    assert_eq!(date.map(|date| date.instant()), Some(86_401));
    let date = BuildDate::parse("2026-09-19T11:02:03Z");
    assert_eq!(date.map(|date| date.instant()), Some(1_789_815_723));
}

#[test]
fn the_gpui_bundle_rebuilds_and_relaunches_itself() {
    let build = AppBuild::from_info(&stamped(&[
        ("RCGitVariant", "gpui"),
        ("RCGitSourceRoot", "/src/rc-git"),
    ]));
    assert_eq!(build.variant, AppVariant::Gpui);
    assert_eq!(build.update_mode(), Some(UpdateMode::Rebuild));
    assert_eq!(
        build.relaunch_path().as_deref(),
        Some("/src/rc-git/RCGit-gpui.app")
    );
    assert_eq!(
        build.variant.build_script(),
        ("scripts/build-app.sh", " --gpui --dev")
    );
    assert_eq!(
        AppVariant::Dev.build_script(),
        ("scripts/build-app.sh", " --dev")
    );
    assert_eq!(
        AppVariant::Release.build_script(),
        ("scripts/install-app.sh", "")
    );
}
