//! Port of `AppBuild.swift`: what `scripts/build-app.sh` stamped into the bundle's
//! `Info.plist`, read from the plist's string keys.

use std::collections::HashMap;

/// Which bundle is running. The two carry different bundle identifiers, so their tabs,
/// recents, window frames, and trusted host keys never mix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppVariant {
    /// `RCGit.app`, installed in `/Applications` by `scripts/install-app.sh`.
    Release,
    /// `RCGit-dev.app`, built into the checkout by `scripts/build-app.sh --dev`.
    Dev,
}

/// What an update does, which is decided by the bundle running it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpdateMode {
    /// The release bundle: fetch, fast-forward, rebuild, install into `/Applications`,
    /// relaunch.
    Update,
    /// The dev bundle: rebuild from the working tree as it stands and relaunch.
    Rebuild,
}

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

/// What `scripts/build-app.sh` stamped into the bundle: which variant it is, the checkout it
/// was built from, and the commit it was built at. An unstamped bundle — a bare `cargo run`,
/// or a bundle assembled by hand — reports [`AppVariant::Dev`] with no source root, which is
/// what keeps the self-update path off.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AppBuild {
    /// Release or dev.
    pub variant: AppVariant,
    /// `CFBundleName`: `rc-git`, or `rc-git (dev)` for the dev bundle.
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
    /// Where `scripts/install-app.sh` puts the release bundle, and what a self-update
    /// replaces and relaunches.
    pub const INSTALLED_PATH: &'static str = "/Applications/RCGit.app";
    /// What `scripts/build-app.sh --dev` names the dev bundle inside its own checkout.
    pub const DEV_BUNDLE_NAME: &'static str = "RCGit-dev.app";

    /// Reads the stamp out of an `Info.plist`'s string keys. Missing or empty keys degrade:
    /// an unstamped bundle is a dev build that cannot update itself.
    #[must_use]
    pub fn from_info(info: &HashMap<String, String>) -> Self {
        let string = |key: &str| info.get(key).filter(|value| !value.is_empty()).cloned();
        let variant = match string("RCGitVariant").as_deref() {
            Some("release") => AppVariant::Release,
            Some(_) | None => AppVariant::Dev,
        };
        Self {
            variant,
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

    /// What this bundle's update does, or `None` when it carries no checkout to build from.
    #[must_use]
    pub fn update_mode(&self) -> Option<UpdateMode> {
        self.source_root.as_ref()?;
        Some(match self.variant {
            AppVariant::Release => UpdateMode::Update,
            AppVariant::Dev => UpdateMode::Rebuild,
        })
    }

    /// The bundle an update launches when the build finishes: the installed release app, or
    /// the dev bundle in the checkout that built it.
    #[must_use]
    pub fn relaunch_path(&self) -> Option<String> {
        match self.variant {
            AppVariant::Release => Some(Self::INSTALLED_PATH.to_owned()),
            AppVariant::Dev => self
                .source_root
                .as_ref()
                .map(|root| format!("{root}/{}", Self::DEV_BUNDLE_NAME)),
        }
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

/// The `RCGitVariant` stamp `scripts/build-app.sh --gpui` writes on the interim GPUI bundle.
pub const GPUI_STAMP: &str = "gpui";

/// Whether this bundle's self-update rebuilds and relaunches this same bundle. The interim
/// GPUI bundle (`RCGit-gpui.app`) is stamped [`GPUI_STAMP`], which [`AppBuild::from_info`]
/// reads as [`AppVariant::Dev`], and a dev update runs `scripts/build-app.sh --dev` and
/// launches `RCGit-dev.app`: another bundle, under another bundle identifier, so the tabs
/// would not carry over. Until the build script makes the dev bundle the GPUI build, such a
/// bundle offers no update.
#[must_use]
pub fn update_targets_this_bundle<S: std::hash::BuildHasher>(
    info: &HashMap<String, String, S>,
) -> bool {
    info.get("RCGitVariant").map(String::as_str) != Some(GPUI_STAMP)
}
