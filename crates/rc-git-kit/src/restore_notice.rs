//! Port of `RestoreNotice.swift`: what the banner says after a launch restore left saved
//! tabs closed.

/// One line naming each repository that did not reopen, and a tooltip with every full path
/// and what happens to it next launch.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RestoreNotice {
    /// The banner line.
    pub summary: String,
    /// The tooltip: one `path: fate` line per failed tab.
    pub detail: String,
}

impl RestoreNotice {
    /// The notice for a restore where `failed` did not open and `kept` is the saved list it
    /// wrote (see [`crate::tabs::restored_paths`]). A failed path still in `kept` is on a
    /// volume that is not mounted and is retried next launch; any other failed path was
    /// dropped. `None` when every saved tab opened.
    #[must_use]
    pub fn make(failed: &[String], kept: &[String]) -> Option<Self> {
        if failed.is_empty() {
            return None;
        }
        let count = if failed.len() == 1 {
            "1 saved tab".to_owned()
        } else {
            format!("{} saved tabs", failed.len())
        };
        let mut names = Vec::with_capacity(failed.len());
        let mut lines = Vec::with_capacity(failed.len());
        for path in failed {
            let fate = fate(path, kept);
            names.push(format!("{} ({fate})", name(path)));
            lines.push(format!("{path}: {fate}"));
        }
        Some(Self {
            summary: format!("Could not reopen {count}: {}", names.join(", ")),
            detail: lines.join("\n"),
        })
    }
}

fn fate(path: &str, kept: &[String]) -> &'static str {
    if kept.iter().any(|candidate| candidate == path) {
        "not reachable, will retry next launch"
    } else {
        "removed from saved tabs"
    }
}

/// The last component, as `URL.lastPathComponent` gives it (trailing slashes ignored), or
/// the whole path when there is none.
fn name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit('/').next() {
        Some(last) if !last.is_empty() => last,
        Some(_) | None => path,
    }
}
