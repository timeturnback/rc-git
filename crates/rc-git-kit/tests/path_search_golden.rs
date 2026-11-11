//! Golden ranking: `fixtures/path_search/ranking.json` holds what Swift's `FuzzyMatch` and
//! `PathIndex.search` ordering produced over this repository's paths (plus a few non-ASCII
//! ones, stored precomposed and decomposed) for a set of queries. The port must reproduce
//! every ranking and every score exactly.
#![expect(
    clippy::unwrap_used,
    reason = "fixture parsing helpers outside #[test] fns unwrap the committed fixture"
)]

use std::sync::atomic::AtomicBool;

use jsonc_parser::{JsonArray, JsonObject, JsonValue, ParseOptions, parse_to_value};
use rc_git_core::{EntryKind, Oid};
use rc_git_kit::path_search::{PathIndex, PathIndexEntry, score, search_key};

const FIXTURE: &str = include_str!("fixtures/path_search/ranking.json");

fn strings(array: &JsonArray<'_>) -> Vec<String> {
    array
        .iter()
        .map(|value| match value {
            JsonValue::String(text) => text.to_string(),
            other => unreachable!("corpus holds strings, found {other:?}"),
        })
        .collect()
}

fn ranked(results: &JsonArray<'_>) -> Vec<(String, i64)> {
    results
        .iter()
        .map(|value| {
            let JsonValue::Object(result) = value else {
                unreachable!("a result is an object, found {value:?}");
            };
            let path = result.get_string("path").unwrap().to_string();
            let score = result.get_number("score").unwrap().parse().unwrap();
            (path, score)
        })
        .collect()
}

fn fixture() -> JsonObject<'static> {
    let Some(JsonValue::Object(root)) = parse_to_value(FIXTURE, &ParseOptions::default()).unwrap()
    else {
        unreachable!("the fixture is a JSON object");
    };
    root
}

#[test]
fn rankings_and_scores_match_the_swift_scorer() {
    let root = fixture();
    let corpus = strings(root.get_array("corpus").unwrap());
    let limit: usize = root.get_number("limit").unwrap().parse().unwrap();
    let entries = corpus
        .iter()
        .map(|path| PathIndexEntry::new(path.clone(), None, EntryKind::Blob, false, true))
        .collect();
    let index = PathIndex {
        root: Oid::from_bytes(&[0; 20]).unwrap(),
        entries,
        truncated: false,
    };
    let cancel = AtomicBool::new(false);
    let mut checked = 0;
    let mut failures = Vec::new();
    for case in root.get_array("queries").unwrap().iter() {
        let JsonValue::Object(case) = case else {
            unreachable!("a query case is an object");
        };
        let query = case.get_string("query").unwrap().to_string();
        let expected = ranked(case.get_array("results").unwrap());
        let found = index.search(&query, limit, true, &cancel).unwrap();
        let prepared = search_key(query.trim());
        let actual: Vec<(String, i64)> = found
            .iter()
            .map(|entry| {
                let points = if prepared.is_empty() {
                    0
                } else {
                    score(entry.search_key(), &prepared).unwrap()
                };
                (entry.path.clone(), points)
            })
            .collect();
        checked += 1;
        if actual != expected {
            failures.push(query);
        }
    }
    assert!(
        checked >= 40,
        "the fixture covers at least 40 queries, found {checked}"
    );
    assert!(
        failures.is_empty(),
        "rankings differ from Swift for queries {failures:?}"
    );
}
