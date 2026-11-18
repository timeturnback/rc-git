//! File-type icons as VS Code's default theme draws them: the Seti UI mapping vendored in
//! `vendor/seti-ui/mapping.less`, read the way VS Code reads it. No Swift counterpart.
//!
//! A rule whose key starts with a dot matches that extension, the longest one first, so
//! `a.test.ts` takes the `.test.ts` rule over `.ts`; any other key matches the whole file name,
//! and a name match beats every extension. Matching ignores ASCII case. Folders, symlinks and
//! submodules never come here: only a file's name picks its icon.
//!
//! The mapping is parsed once, on first use. A rule line that does not parse is skipped at run
//! time and fails `every_rule_in_the_mapping_parses`, so an upstream syntax change surfaces in
//! the tests rather than as a missing icon.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::LazyLock;

/// The vendored Seti UI mapping, unchanged from upstream.
const MAPPING: &str = include_str!("../../../vendor/seti-ui/mapping.less");

/// The color family an icon draws in. The app picks the shade for the current appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileTint {
    /// Seti's blue.
    Blue,
    /// Seti's green.
    Green,
    /// Seti's yellow.
    Yellow,
    /// Seti's orange.
    Orange,
    /// Seti's red.
    Red,
    /// Seti's purple.
    Purple,
    /// Seti's pink.
    Pink,
    /// Seti's greys and white: the quiet tone unknown files also take.
    Muted,
}

/// The icon for one file type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileIcon {
    /// The icon's file stem in `vendor/seti-ui/icons/`, such as `rust` for `rust.svg`.
    pub name: &'static str,
    /// The color it draws in.
    pub tint: FileTint,
}

/// The icon for a file named `file_name`, `None` when the mapping has no rule for it.
#[must_use]
pub fn file_icon(file_name: &str) -> Option<FileIcon> {
    TABLE.lookup(file_name)
}

/// Every icon stem the mapping uses, sorted, each once: the SVGs the app must bundle. Its
/// tests check the bundle against this.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn icon_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = TABLE
        .names
        .values()
        .chain(TABLE.extensions.values())
        .map(|icon| icon.name)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

static TABLE: LazyLock<Table> = LazyLock::new(|| Table::parse(MAPPING));

/// The mapping, keyed by lowercased name and lowercased extension (without its leading dot).
#[derive(Debug, Default)]
struct Table {
    names: HashMap<String, FileIcon>,
    extensions: HashMap<String, FileIcon>,
}

/// One parsed rule: what it matches and the icon it gives.
#[derive(Debug, PartialEq, Eq)]
enum Rule {
    Name(String, FileIcon),
    Extension(String, FileIcon),
}

impl Table {
    /// Later rules replace earlier ones for the same key, as the CSS cascade does upstream.
    fn parse(mapping: &'static str) -> Self {
        let mut table = Self::default();
        for line in mapping.lines() {
            match parse_line(line) {
                Some(Ok(Rule::Name(key, icon))) => {
                    table.names.insert(key, icon);
                }
                Some(Ok(Rule::Extension(key, icon))) => {
                    table.extensions.insert(key, icon);
                }
                Some(Err(_)) | None => {}
            }
        }
        table
    }

    fn lookup(&self, file_name: &str) -> Option<FileIcon> {
        let name = if file_name.bytes().any(|byte| byte.is_ascii_uppercase()) {
            Cow::Owned(file_name.to_ascii_lowercase())
        } else {
            Cow::Borrowed(file_name)
        };
        if let Some(icon) = self.names.get(name.as_ref()) {
            return Some(*icon);
        }
        name.match_indices('.')
            .find_map(|(at, _)| self.extensions.get(&name[at + 1..]).copied())
    }
}

/// `None` for a line that is not a rule; `Some(Err)` for a rule line that does not parse,
/// naming the line.
///
/// A rule is `.icon-set("<key>", "<icon>", @<color>);` (extension or whole name, by the key's
/// leading dot) or `.icon-partial("<key>", "<icon>", @<color>);` (always a whole name, as VS
/// Code reads it). Either quote character may wrap a string.
fn parse_line(line: &'static str) -> Option<Result<Rule, String>> {
    let line = line.trim();
    let (by_extension, rest) = match line.strip_prefix(".icon-set(") {
        Some(rest) => (true, rest),
        None => (false, line.strip_prefix(".icon-partial(")?),
    };
    Some(parse_arguments(rest, by_extension).ok_or_else(|| format!("unparsed Seti rule: {line}")))
}

fn parse_arguments(arguments: &'static str, by_extension: bool) -> Option<Rule> {
    let mut parts = arguments.strip_suffix(");")?.split(", ");
    let key = unquote(parts.next()?)?;
    let name = unquote(parts.next()?)?;
    let tint = tint(parts.next()?.strip_prefix('@')?)?;
    if parts.next().is_some() || key.is_empty() || name.is_empty() {
        return None;
    }
    let icon = FileIcon { name, tint };
    Some(match key.strip_prefix('.') {
        Some(extension) if by_extension => Rule::Extension(extension.to_ascii_lowercase(), icon),
        _ => Rule::Name(key.to_ascii_lowercase(), icon),
    })
}

fn unquote(text: &'static str) -> Option<&'static str> {
    ['"', '\'']
        .into_iter()
        .find_map(|quote| text.strip_prefix(quote)?.strip_suffix(quote))
}

fn tint(color: &str) -> Option<FileTint> {
    Some(match color {
        "blue" => FileTint::Blue,
        "green" => FileTint::Green,
        "yellow" => FileTint::Yellow,
        "orange" => FileTint::Orange,
        "red" => FileTint::Red,
        "purple" => FileTint::Purple,
        "pink" => FileTint::Pink,
        "white" | "grey" | "grey-light" | "ignore" | "seti-primary" => FileTint::Muted,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn icon(name: &'static str, tint: FileTint) -> FileIcon {
        FileIcon { name, tint }
    }

    #[test]
    fn an_extension_picks_its_language() {
        assert_eq!(file_icon("main.rs"), Some(icon("rust", FileTint::Muted)));
        assert_eq!(
            file_icon("index.ts"),
            Some(icon("typescript", FileTint::Blue))
        );
        assert_eq!(
            file_icon("App.swift"),
            Some(icon("swift", FileTint::Orange))
        );
        assert_eq!(
            file_icon("config.yaml"),
            Some(icon("yml", FileTint::Purple))
        );
    }

    #[test]
    fn the_longest_extension_wins() {
        assert_eq!(
            file_icon("index.test.ts"),
            Some(icon("typescript", FileTint::Orange))
        );
        assert_eq!(
            file_icon(".stylelintrc.json"),
            Some(icon("stylelint", FileTint::Muted))
        );
        assert_eq!(file_icon("data.json"), Some(icon("json", FileTint::Yellow)));
    }

    #[test]
    fn a_whole_name_beats_its_extension() {
        assert_eq!(file_icon("README.md"), Some(icon("info", FileTint::Blue)));
        assert_eq!(
            file_icon("NOTES.md"),
            Some(icon("markdown", FileTint::Blue))
        );
        assert_eq!(file_icon("yarn.lock"), Some(icon("yarn", FileTint::Blue)));
    }

    #[test]
    fn a_partial_rule_matches_the_whole_name_only() {
        assert_eq!(
            file_icon("Dockerfile"),
            Some(icon("docker", FileTint::Blue))
        );
        assert_eq!(file_icon("mix"), Some(icon("hex", FileTint::Red)));
        assert_eq!(file_icon("mixins.less"), file_icon("x.less"));
    }

    #[test]
    fn a_dot_file_matches_as_an_extension() {
        assert_eq!(file_icon(".gitignore"), Some(icon("git", FileTint::Muted)));
    }

    #[test]
    fn matching_ignores_ascii_case() {
        assert_eq!(file_icon("MAIN.RS"), file_icon("main.rs"));
        assert_eq!(file_icon("readme.MD"), Some(icon("info", FileTint::Blue)));
    }

    #[test]
    fn a_name_with_no_rule_has_no_icon() {
        assert_eq!(file_icon("notes"), None);
        assert_eq!(file_icon("archive.unknownext"), None);
        assert_eq!(file_icon("trailing."), None);
        assert_eq!(file_icon(""), None);
        assert_eq!(file_icon("."), None);
    }

    #[test]
    fn a_rule_line_that_does_not_parse_is_reported_and_skipped() {
        for line in [
            ".icon-set(\".rs\", \"rust\", @teal);",
            ".icon-set(\".rs\", \"rust\");",
            ".icon-set(\".rs\", rust, @blue);",
            ".icon-set(\"\", \"rust\", @blue);",
            ".icon-set(\".rs\", \"rust\", @blue, @red);",
        ] {
            assert!(parse_line(line).is_some_and(|rule| rule.is_err()), "{line}");
        }
        assert_eq!(parse_line("// .rs files"), None);
        assert_eq!(parse_line("  .icon(\"default\", @white);"), None);
        let table = Table::parse(".icon-set(\".rs\", \"rust\", @teal);\n");
        assert!(table.names.is_empty() && table.extensions.is_empty());
    }

    #[test]
    fn a_later_rule_replaces_an_earlier_one() {
        let table = Table::parse(
            ".icon-set(\".x\", \"first\", @blue);\n.icon-set('.X', 'second', @red);\n",
        );
        assert_eq!(table.lookup("a.x"), Some(icon("second", FileTint::Red)));
    }

    /// The vendored mapping is data from upstream; a rule the parser misreads would ship as a
    /// file with the wrong icon or none.
    #[test]
    fn every_rule_in_the_mapping_parses() {
        let problems: Vec<String> = MAPPING
            .lines()
            .filter_map(parse_line)
            .filter_map(Result::err)
            .collect();
        assert!(problems.is_empty(), "{problems:#?}");
        let rules = MAPPING.lines().filter_map(parse_line).count();
        assert!(rules > 400, "only {rules} rules found in the mapping");
    }

    #[test]
    fn icon_names_lists_each_used_icon_once() {
        let names = icon_names();
        assert!(names.contains(&"rust") && names.contains(&"docker"));
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
