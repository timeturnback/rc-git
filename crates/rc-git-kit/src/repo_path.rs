//! Port of `RepoPath.swift`: where a relative reference written in a repository file lands in
//! the repository.

/// Appends `relative` to the `base` components and flattens `.`, `..` and empty segments.
///
/// `None` when the path climbs out of the repository; an empty result is the repository root.
#[must_use]
pub fn flatten(base: &[String], relative: &str) -> Option<Vec<String>> {
    let mut components = base.to_vec();
    for component in relative.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            name => components.push(name.to_owned()),
        }
    }
    Some(components)
}
