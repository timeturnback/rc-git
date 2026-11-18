//! Port of `AppBuild.swift`: what `scripts/build-app.sh` stamped into the bundle's
//! `Info.plist`, read from the plist's string keys.

use std::collections::HashMap;

/// The bundle identifier `scripts/build-app.sh` stamps, which names the app's defaults domain.
/// The CLI reads and writes the GitHub accounts there, so the app and the CLI share them;
/// `scripts/check.sh` fails when this and the script's `BUNDLE_ID` differ.
pub const BUNDLE_IDENTIFIER: &str = "com.timeturnback.rcgit";

/// A UTC instant stamped as `RCGitBuildDate`, kept as its RFC 3339 text and the fields read
/// from it. Only the `YYYY-MM-DDTHH:MM:SSZ` form `scripts/build-app.sh` writes parses.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BuildDate {
    /// The stamp as written.
    pub raw: String,
    /// Calendar year.
    pub year: u16,
    /// Month, 1 to 12.
    pub month: u8,
    /// Day of the month, 1 to 31.
    pub day: u8,
    /// Hour, 0 to 23.
    pub hour: u8,
    /// Minute, 0 to 59.
    pub minute: u8,
    /// Second, 0 to 60 (a leap second is accepted).
    pub second: u8,
}

impl BuildDate {
    /// Reads `YYYY-MM-DDTHH:MM:SSZ`; anything else, or a field out of range, is `None`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 20 {
            return None;
        }
        for (index, separator) in [
            (4, b'-'),
            (7, b'-'),
            (10, b'T'),
            (13, b':'),
            (16, b':'),
            (19, b'Z'),
        ] {
            if bytes.get(index) != Some(&separator) {
                return None;
            }
        }
        let year = u16::try_from(digits(text, 0, 4)?).ok()?;
        let field = |start: usize| u8::try_from(digits(text, start, 2)?).ok();
        let (month, day) = (field(5)?, field(8)?);
        let (hour, minute, second) = (field(11)?, field(14)?, field(17)?);
        let in_range = (1..=12).contains(&month)
            && (1..=31).contains(&day)
            && hour < 24
            && minute < 60
            && second <= 60;
        in_range.then(|| Self {
            raw: text.to_owned(),
            year,
            month,
            day,
            hour,
            minute,
            second,
        })
    }
}

/// The ASCII digits at `text[start..start + len]` as a number; `None` for any non-digit.
fn digits(text: &str, start: usize, len: usize) -> Option<u32> {
    let slice = text.get(start..start + len)?;
    let mut value = 0u32;
    for byte in slice.bytes() {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value * 10 + u32::from(byte - b'0');
    }
    Some(value)
}

/// What `scripts/build-app.sh` stamped into the bundle: the checkout it was built from and
/// the commit it was built at. An unstamped bundle — a bare `cargo run`, or a bundle assembled
/// by hand — has no source root, which is what keeps the self-update path off.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AppBuild {
    /// `CFBundleName`, e.g. `rc-git`.
    pub name: String,
    /// `CFBundleShortVersionString`, e.g. `0.1.42`.
    pub version: String,
    /// `CFBundleVersion`: the commit count at build time.
    pub build: String,
    /// Absolute path of the checkout the bundle was built from, when it was stamped.
    pub source_root: Option<String>,
    /// Full hex of the commit the bundle was built at, when it was stamped.
    pub commit: Option<String>,
    /// Whether tracked files were modified in the checkout at build time.
    pub dirty: bool,
    /// When the bundle was built, when the stamp parses.
    pub build_date: Option<BuildDate>,
}

impl AppBuild {
    /// What `scripts/build-app.sh` names the bundle inside its own checkout, and what a
    /// rebuild relaunches.
    pub const BUNDLE_NAME: &'static str = "RCGit.app";
    /// The script that rebuilds the bundle, relative to the checkout. It takes no arguments.
    pub const BUILD_SCRIPT: &'static str = "scripts/build-app.sh";

    /// Reads the stamp out of an `Info.plist`'s string keys. Missing or empty keys degrade:
    /// a bundle with no source root cannot rebuild itself.
    #[must_use]
    pub fn from_info(info: &HashMap<String, String>) -> Self {
        let string = |key: &str| info.get(key).filter(|value| !value.is_empty()).cloned();
        Self {
            name: string("CFBundleName").unwrap_or_else(|| "rc-git".to_owned()),
            version: string("CFBundleShortVersionString").unwrap_or_else(|| "0".to_owned()),
            build: string("CFBundleVersion").unwrap_or_else(|| "0".to_owned()),
            source_root: string("RCGitSourceRoot"),
            commit: string("RCGitSourceCommit").filter(|commit| commit != "unknown"),
            dirty: string("RCGitSourceDirty").as_deref() == Some("true"),
            build_date: string("RCGitBuildDate").and_then(|text| BuildDate::parse(&text)),
        }
    }

    /// First seven hex digits of the build commit, the length git itself abbreviates to.
    #[must_use]
    pub fn short_commit(&self) -> Option<&str> {
        let commit = self.commit.as_deref()?;
        Some(commit.get(..7).unwrap_or(commit))
    }

    /// Whether this bundle can rebuild itself: it carries the checkout it was built from.
    #[must_use]
    pub fn can_update(&self) -> bool {
        self.source_root.is_some()
    }

    /// The bundle a rebuild launches when the build finishes: [`Self::BUNDLE_NAME`] in the
    /// checkout that built it. `None` for a bundle with no source root.
    #[must_use]
    pub fn relaunch_path(&self) -> Option<String> {
        let root = self.source_root.as_ref()?;
        Some(format!("{root}/{}", Self::BUNDLE_NAME))
    }

    /// What the window titlebar shows beside its title: `0.1.42`, with a `+` when the
    /// checkout was dirty at build time.
    #[must_use]
    pub fn short_version(&self) -> String {
        if self.dirty {
            format!("{}+", self.version)
        } else {
            self.version.clone()
        }
    }

    /// `0.1.0 (42) · a3f91c2`, with a `+` on the commit when the checkout was dirty. The
    /// build number is dropped when the version already ends in it, which is what
    /// `scripts/build-app.sh` stamps: both are the commit count.
    #[must_use]
    pub fn version_line(&self) -> String {
        let mut line = self.version.clone();
        if !self.version.ends_with(&format!(".{}", self.build)) {
            line = format!("{line} ({})", self.build);
        }
        if let Some(short) = self.short_commit() {
            let mark = if self.dirty { "+" } else { "" };
            line = format!("{line} · {short}{mark}");
        }
        line
    }
}

impl BuildDate {
    /// The stamp as seconds since the Unix epoch.
    #[must_use]
    pub fn instant(&self) -> i64 {
        let days = crate::dates::days_from_civil(crate::dates::CivilDate {
            year: i64::from(self.year),
            month: u32::from(self.month),
            day: u32::from(self.day),
        });
        days * 86_400
            + i64::from(self.hour) * 3_600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
    }
}
