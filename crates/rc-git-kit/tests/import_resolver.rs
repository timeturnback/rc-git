//! Port of `ImportResolverTests.swift`: module specifiers to repository paths, over an
//! in-memory tree.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] fns unwrap infallible resolutions and fixed ids"
)]

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;

use rc_git_core::bstr::BString;
use rc_git_core::{EntryKind, Oid, TreeEntry};
use rc_git_kit::import_resolver::{CONFIG_READ_LIMIT, ImportResolver, ImportSource};

type Files = HashMap<String, String>;

/// The children of `directory` that the paths of `files` imply.
fn listing(directory: &str, files: &Files) -> Option<Vec<TreeEntry>> {
    let prefix = if directory.is_empty() {
        String::new()
    } else {
        format!("{directory}/")
    };
    let mut kinds = BTreeMap::new();
    for path in files.keys() {
        let Some(rest) = path.strip_prefix(&prefix) else {
            continue;
        };
        let parts: Vec<&str> = rest.split('/').filter(|part| !part.is_empty()).collect();
        let Some(name) = parts.first() else {
            continue;
        };
        let kind = if parts.len() > 1 {
            EntryKind::Tree
        } else {
            EntryKind::Blob
        };
        kinds.insert((*name).to_owned(), kind);
    }
    if kinds.is_empty() && !directory.is_empty() {
        return None;
    }
    let id = Oid::from_bytes(&[1; 20]).unwrap();
    Some(
        kinds
            .into_iter()
            .map(|(name, kind)| TreeEntry {
                name: BString::from(name),
                oid: id,
                kind,
            })
            .collect(),
    )
}

/// A source over `files`, path to content, counting config reads. Directories are whatever
/// the paths imply.
struct Memory {
    files: Files,
    reads: Cell<usize>,
}

impl ImportSource for &Memory {
    type Error = Infallible;

    fn list(&self, directory: &str) -> Result<Option<Vec<TreeEntry>>, Infallible> {
        Ok(listing(directory, &self.files))
    }

    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, Infallible> {
        self.reads.set(self.reads.get() + 1);
        Ok(self.files.get(path).map(|text| text.as_bytes().to_vec()))
    }
}

fn memory(files: Files) -> Memory {
    Memory {
        files,
        reads: Cell::new(0),
    }
}

fn sources(paths: &[&str]) -> Files {
    paths
        .iter()
        .map(|path| ((*path).to_owned(), String::new()))
        .collect()
}

fn resolve(files: &Files, specifier: &str, from: &str) -> Option<String> {
    let source = memory(files.clone());
    ImportResolver::new(&source)
        .resolve(specifier, from)
        .unwrap()
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "spells an expected resolution, which is an Option"
)]
fn some(path: &str) -> Option<String> {
    Some(path.to_owned())
}

#[test]
fn relative_specifiers_resolve_against_the_importing_file() {
    let tree = sources(&[
        "src/app/page.tsx",
        "src/app/util.ts",
        "src/lib/format.tsx",
        "src/app/a.css",
    ]);
    assert_eq!(
        resolve(&tree, "./util", "src/app/page.tsx"),
        some("src/app/util.ts")
    );
    assert_eq!(
        resolve(&tree, "../lib/format", "src/app/page.tsx"),
        some("src/lib/format.tsx")
    );
    assert_eq!(
        resolve(&tree, "./a.css", "src/app/page.tsx"),
        some("src/app/a.css")
    );
    assert_eq!(
        resolve(&tree, "./util.ts", "src/app/page.tsx"),
        some("src/app/util.ts")
    );
}

#[test]
fn a_missing_file_and_a_climb_out_of_the_repository_resolve_to_nothing() {
    let tree = sources(&["src/page.tsx"]);
    assert_eq!(resolve(&tree, "./nope", "src/page.tsx"), None);
    assert_eq!(resolve(&tree, "./nope/deeper", "src/page.tsx"), None);
    assert_eq!(resolve(&tree, "../../outside", "src/page.tsx"), None);
    assert_eq!(resolve(&tree, "", "src/page.tsx"), None);
    assert_eq!(resolve(&tree, "/etc/passwd", "src/page.tsx"), None);
}

#[test]
fn typescript_sources_are_tried_before_javascript_and_declarations() {
    let tree = sources(&["a.ts", "a.js", "b.d.ts", "b.js", "c.jsx", "main.ts"]);
    assert_eq!(resolve(&tree, "./a", "main.ts"), some("a.ts"));
    assert_eq!(resolve(&tree, "./b", "main.ts"), some("b.d.ts"));
    assert_eq!(resolve(&tree, "./c", "main.ts"), some("c.jsx"));
}

#[test]
fn a_directory_resolves_to_its_index_and_loses_to_a_file_of_the_same_name() {
    let tree = sources(&[
        "ui/index.tsx",
        "kit/index.js",
        "both.ts",
        "both/index.ts",
        "empty/x.css",
        "m.ts",
    ]);
    assert_eq!(resolve(&tree, "./ui", "m.ts"), some("ui/index.tsx"));
    assert_eq!(resolve(&tree, "./kit", "m.ts"), some("kit/index.js"));
    assert_eq!(resolve(&tree, "./both", "m.ts"), some("both.ts"));
    assert_eq!(
        resolve(&tree, "./empty", "m.ts"),
        None,
        "a directory with no index"
    );
    assert_eq!(resolve(&tree, ".", "ui/Button.tsx"), some("ui/index.tsx"));
    assert_eq!(
        resolve(&tree, "..", "both/deep/x.ts"),
        some("both/index.ts")
    );
    assert_eq!(resolve(&tree, "./both/", "m.ts"), some("both/index.ts"));
}

#[test]
fn a_compiled_extension_finds_its_source() {
    let tree = sources(&["a.ts", "b.tsx", "c.mts", "d.js", "main.ts"]);
    assert_eq!(resolve(&tree, "./a.js", "main.ts"), some("a.ts"));
    assert_eq!(resolve(&tree, "./b.js", "main.ts"), some("b.tsx"));
    assert_eq!(resolve(&tree, "./b.jsx", "main.ts"), some("b.tsx"));
    assert_eq!(resolve(&tree, "./c.mjs", "main.ts"), some("c.mts"));
    assert_eq!(
        resolve(&tree, "./d.js", "main.ts"),
        some("d.js"),
        "the file as written wins"
    );
}

#[test]
fn a_query_suffix_is_not_part_of_the_path() {
    let tree = sources(&["src/icon.svg", "src/main.ts"]);
    assert_eq!(
        resolve(&tree, "./icon.svg?react", "src/main.ts"),
        some("src/icon.svg")
    );
}

#[test]
fn path_aliases_come_from_the_nearest_config_with_comments_and_trailing_commas() {
    let mut files = sources(&[
        "src/components/Button.tsx",
        "src/app/page.tsx",
        "src/env.ts",
    ]);
    files.insert(
        "tsconfig.json".into(),
        r#"{
          // what the bundler also reads
          "compilerOptions": {
            "strict": true, /* unrelated */
            "paths": {
              "@/*": ["./src/*"],
              "env": ["./src/env.ts"],
            },
          },
        }"#
        .into(),
    );
    assert_eq!(
        resolve(&files, "@/components/Button", "src/app/page.tsx"),
        some("src/components/Button.tsx")
    );
    assert_eq!(
        resolve(&files, "env", "src/app/page.tsx"),
        some("src/env.ts")
    );
    assert_eq!(resolve(&files, "@/missing", "src/app/page.tsx"), None);
    assert_eq!(
        resolve(&files, "react", "src/app/page.tsx"),
        None,
        "a registry package"
    );
}

#[test]
fn the_longest_pattern_wins_and_targets_are_tried_in_order() {
    let mut files = sources(&[
        "src/x.ts",
        "generated/api/x.ts",
        "fallback/only.ts",
        "main.ts",
    ]);
    files.insert(
        "tsconfig.json".into(),
        r#"{"compilerOptions": {"paths": {
          "@/*": ["./src/*", "./fallback/*"],
          "@/api/*": ["./generated/api/*"]
        }}}"#
            .into(),
    );
    assert_eq!(resolve(&files, "@/x", "main.ts"), some("src/x.ts"));
    assert_eq!(
        resolve(&files, "@/api/x", "main.ts"),
        some("generated/api/x.ts")
    );
    assert_eq!(
        resolve(&files, "@/only", "main.ts"),
        some("fallback/only.ts")
    );
}

#[test]
fn equal_prefix_patterns_pick_the_longer_suffix() {
    let mut files = sources(&[
        "s1/a.css", "s2/a.css", "s3/a.css", "s4/a.css", "s5/a.css", "s6/a.css", "main.ts",
    ]);
    files.insert(
        "tsconfig.json".into(),
        r#"{"compilerOptions": {"paths": {
          "@/*": ["./s1/*"], "@/*s": ["./s2/*s"], "@/*ss": ["./s3/*ss"],
          "@/*css": ["./s4/*css"], "@/*.css": ["./s5/*.css"], "@/*a.css": ["./s6/*a.css"]
        }}}"#
            .into(),
    );
    for _ in 0..20 {
        assert_eq!(resolve(&files, "@/a.css", "main.ts"), some("s6/a.css"));
    }
}

#[test]
fn base_url_places_bare_specifiers_and_path_targets() {
    let mut files = sources(&[
        "app/src/components/Button.tsx",
        "app/src/lib/x.ts",
        "app/src/main.ts",
    ]);
    files.insert(
        "app/jsconfig.json".into(),
        r#"{"compilerOptions": {"baseUrl": "./src", "paths": {"~lib/*": ["lib/*"]}}}"#.into(),
    );
    assert_eq!(
        resolve(&files, "components/Button", "app/src/main.ts"),
        some("app/src/components/Button.tsx")
    );
    assert_eq!(
        resolve(&files, "~lib/x", "app/src/main.ts"),
        some("app/src/lib/x.ts")
    );
}

#[test]
fn an_extended_config_supplies_what_the_nearest_leaves_unset() {
    let mut files = sources(&["packages/ui/src/Button.tsx", "packages/web/src/page.tsx"]);
    files.insert(
        "tsconfig.base.json".into(),
        r#"{"compilerOptions": {"paths": {"@ui/*": ["./packages/ui/src/*"]}}}"#.into(),
    );
    files.insert(
        "packages/web/tsconfig.json".into(),
        r#"{"extends": "../../tsconfig.base", "compilerOptions": {"strict": true}}"#.into(),
    );
    assert_eq!(
        resolve(&files, "@ui/Button", "packages/web/src/page.tsx"),
        some("packages/ui/src/Button.tsx"),
        "targets start from the config that wrote them"
    );
}

#[test]
fn an_extends_cycle_ends() {
    let mut files = sources(&["src/a.ts"]);
    files.insert(
        "tsconfig.json".into(),
        r#"{"extends": "./other.json"}"#.into(),
    );
    files.insert(
        "other.json".into(),
        r#"{"extends": "./tsconfig.json"}"#.into(),
    );
    assert_eq!(resolve(&files, "@/a", "src/a.ts"), None);
}

#[test]
fn a_solution_config_hands_over_to_the_configs_it_references() {
    let mut files = sources(&["src/lib/x.ts", "src/main.tsx"]);
    files.insert(
        "tsconfig.json".into(),
        r#"{"files": [], "references": [{"path": "./tsconfig.node.json"},
        {"path": "./tsconfig.app.json"}]}"#
            .into(),
    );
    files.insert(
        "tsconfig.node.json".into(),
        r#"{"compilerOptions": {"composite": true}}"#.into(),
    );
    files.insert(
        "tsconfig.app.json".into(),
        r#"{"compilerOptions": {"paths": {"@/*": ["./src/*"]}}}"#.into(),
    );
    assert_eq!(
        resolve(&files, "@/lib/x", "src/main.tsx"),
        some("src/lib/x.ts")
    );
}

#[test]
fn a_reference_may_name_a_directory_and_the_last_extended_config_wins() {
    let mut files = sources(&["app/src/x.ts", "lib/x.ts", "lib/y.ts"]);
    files.insert(
        "tsconfig.json".into(),
        r#"{"references": [{"path": "./app"}]}"#.into(),
    );
    files.insert(
        "app/tsconfig.json".into(),
        r#"{"extends": ["../base.json", "./local.json"]}"#.into(),
    );
    files.insert(
        "base.json".into(),
        r#"{"compilerOptions": {"paths": {"@/*": ["./lib/*"]}}}"#.into(),
    );
    files.insert(
        "app/local.json".into(),
        r#"{"compilerOptions": {"paths": {"@/*": ["./src/*"]}}}"#.into(),
    );
    assert_eq!(resolve(&files, "@/x", "lib/y.ts"), some("app/src/x.ts"));
}

#[test]
fn an_inherited_base_url_places_the_targets_of_paths_written_below_it() {
    let mut files = sources(&["lib/y.ts", "app/src/main.ts"]);
    files.insert(
        "base.json".into(),
        r#"{"compilerOptions": {"baseUrl": "./lib"}}"#.into(),
    );
    files.insert(
        "app/tsconfig.json".into(),
        r#"{"extends": "../base.json", "compilerOptions": {"paths": {"~/*": ["./*"]}}}"#.into(),
    );
    assert_eq!(resolve(&files, "~/y", "app/src/main.ts"), some("lib/y.ts"));
    assert_eq!(resolve(&files, "y", "app/src/main.ts"), some("lib/y.ts"));
}

#[test]
fn a_config_farther_up_answers_when_the_nearest_cannot() {
    let mut files = sources(&["shared/x.ts", "apps/web/src/page.tsx"]);
    files.insert(
        "apps/web/tsconfig.json".into(),
        r#"{"compilerOptions": {"strict": true}}"#.into(),
    );
    files.insert(
        "tsconfig.json".into(),
        r#"{"compilerOptions": {"paths": {"shared/*": ["./shared/*"]}}}"#.into(),
    );
    assert_eq!(
        resolve(&files, "shared/x", "apps/web/src/page.tsx"),
        some("shared/x.ts")
    );
}

#[test]
fn a_fan_out_of_extends_stays_within_the_read_budget() {
    let mut files = sources(&["src/a.ts"]);
    let entries = vec![r#""./tsconfig.json""#; 50].join(", ");
    files.insert(
        "tsconfig.json".into(),
        format!(r#"{{"extends": [{entries}]}}"#),
    );
    let source = memory(files);
    assert_eq!(
        ImportResolver::new(&source)
            .resolve("@/a", "src/a.ts")
            .unwrap(),
        None
    );
    assert!(source.reads.get() <= CONFIG_READ_LIMIT);
}

#[test]
fn a_fan_out_across_distinct_configs_stays_within_the_read_budget() {
    let mut files = sources(&["src/a.ts"]);
    let names: Vec<String> = (0..20).map(|n| format!("c{n}.json")).collect();
    let list = names
        .iter()
        .map(|name| format!(r#""./{name}""#))
        .collect::<Vec<_>>()
        .join(", ");
    let extends = format!(r#"{{"extends": [{list}]}}"#);
    files.insert("tsconfig.json".into(), extends.clone());
    for name in &names {
        files.insert(name.clone(), extends.clone());
    }
    let source = memory(files);
    assert_eq!(
        ImportResolver::new(&source)
            .resolve("@/a", "src/a.ts")
            .unwrap(),
        None
    );
    assert_eq!(source.reads.get(), CONFIG_READ_LIMIT);
}

#[derive(Debug, PartialEq, Eq)]
struct Unreadable;

/// A source over `files` that fails to list `bad_directory` and, when `reads_fail`,
/// every config read.
struct Failing {
    files: Files,
    bad_directory: &'static str,
    reads_fail: bool,
}

impl ImportSource for Failing {
    type Error = Unreadable;

    fn list(&self, directory: &str) -> Result<Option<Vec<TreeEntry>>, Unreadable> {
        if directory == self.bad_directory {
            return Err(Unreadable);
        }
        Ok(listing(directory, &self.files))
    }

    fn read(&self, _path: &str) -> Result<Option<Vec<u8>>, Unreadable> {
        if self.reads_fail {
            return Err(Unreadable);
        }
        Ok(None)
    }
}

#[test]
fn a_failed_read_errors_rather_than_resolving_to_nothing() {
    let files = sources(&["src/page.tsx", "src/util.ts"]);
    let failing_list = ImportResolver::new(Failing {
        files: files.clone(),
        bad_directory: "src",
        reads_fail: false,
    });
    assert_eq!(
        failing_list.resolve("./util", "src/page.tsx"),
        Err(Unreadable)
    );
    let mut configured = files;
    configured.insert(
        "tsconfig.json".into(),
        r#"{"compilerOptions": {"paths": {"@/*": ["./src/*"]}}}"#.into(),
    );
    let failing_read = ImportResolver::new(Failing {
        files: configured,
        bad_directory: "no such directory",
        reads_fail: true,
    });
    assert_eq!(
        failing_read.resolve("@/util", "src/page.tsx"),
        Err(Unreadable)
    );
}

#[test]
fn an_unreadable_config_resolves_nothing() {
    let mut files = sources(&["src/a.ts"]);
    files.insert("tsconfig.json".into(), "{ not json".into());
    assert_eq!(resolve(&files, "@/a", "src/a.ts"), None);
    files.insert("tsconfig.json".into(), "[1, 2]".into());
    assert_eq!(
        resolve(&files, "@/a", "src/a.ts"),
        None,
        "a top level that is not an object"
    );
}

#[test]
fn a_paths_map_with_a_value_that_is_not_a_string_list_is_ignored() {
    let mut files = sources(&["src/a.ts", "main.ts"]);
    files.insert(
        "tsconfig.json".into(),
        r#"{"compilerOptions": {"paths": {"@/*": ["./src/*"], "bad": "./src/a.ts"}}}"#.into(),
    );
    assert_eq!(
        resolve(&files, "@/a", "main.ts"),
        None,
        "Swift's cast refused the whole map"
    );
}

#[test]
fn a_config_with_a_byte_order_mark_still_parses() {
    let mut files = sources(&["src/a.ts", "main.ts"]);
    files.insert(
        "tsconfig.json".into(),
        "\u{FEFF}{\"compilerOptions\": {\"paths\": {\"@/*\": [\"./src/*\"]}}}".into(),
    );
    assert_eq!(resolve(&files, "@/a", "main.ts"), some("src/a.ts"));
}
