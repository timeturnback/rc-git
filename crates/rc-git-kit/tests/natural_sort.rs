//! Natural order: the example orderings the ref list, change tree and host key list pin, and
//! the properties that make it a total order, checked over generated names.

use std::cmp::Ordering;

use rc_git_kit::natural_sort::{compare, precedes, sort};

fn sorted(names: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    sort(&mut names);
    names
}

#[test]
fn numbers_compare_by_value() {
    assert_eq!(sorted(&["v10", "v2", "v1"]), ["v1", "v2", "v10"]);
    assert_eq!(sorted(&["main-10", "main-2"]), ["main-2", "main-10"]);
    assert_eq!(
        sorted(&["refs/tags/v10", "refs/remotes/origin/HEAD", "refs/tags/v2"]),
        ["refs/remotes/origin/HEAD", "refs/tags/v2", "refs/tags/v10"]
    );
    assert!(precedes("file9.txt", "file10.txt"));
    assert!(precedes("a2b", "a10b"));
}

#[test]
fn letters_compare_without_case() {
    assert_eq!(
        sorted(&["Main", "feature/MAIN-fix"]),
        ["feature/MAIN-fix", "Main"]
    );
    assert_eq!(
        sorted(&[
            "main",
            "feature/main-fix",
            "domain-model",
            "chore/maintenance"
        ]),
        [
            "chore/maintenance",
            "domain-model",
            "feature/main-fix",
            "main"
        ]
    );
    assert!(precedes("apple", "Banana"));
    assert!(precedes("Apple", "banana"));
}

#[test]
fn case_alone_breaks_a_tie_lowercase_first() {
    assert_eq!(compare("readme", "README"), Ordering::Less);
    assert_eq!(compare("README", "readme"), Ordering::Greater);
    assert_eq!(compare("same", "same"), Ordering::Equal);
}

#[test]
fn leading_zeros_tie_on_value_and_still_order() {
    assert_ne!(compare("v01", "v1"), Ordering::Equal);
    assert!(precedes("v01", "v2"));
    assert!(precedes("v1", "v02"));
}

#[test]
fn symbols_sort_before_digits_before_letters() {
    assert_eq!(sorted(&["a", "1", "-"]), ["-", "1", "a"]);
    assert!(precedes("", "a"));
    assert!(precedes("a", "a1"));
}

#[test]
fn a_long_number_does_not_overflow() {
    let big = "v123456789012345678901234567890";
    assert!(precedes("v99", big));
    assert!(precedes(big, "v1234567890123456789012345678901"));
}

/// A deterministic generator: names drawn from an alphabet that mixes case, digits with
/// leading zeros, separators and a non-ASCII letter, so ties and near ties are common.
struct Names {
    state: u64,
}

impl Names {
    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state >> 33
    }

    fn name(&mut self) -> String {
        const ALPHABET: [&str; 12] = ["a", "A", "b", "B", "0", "1", "2", "9", "-", "/", "é", "10"];
        let length = self.next_u64() % 5;
        let mut name = String::new();
        for _ in 0..length {
            let pick = usize::try_from(self.next_u64() % 12).unwrap_or(0);
            name.push_str(ALPHABET[pick]);
        }
        name
    }
}

fn generated(count: usize) -> Vec<String> {
    let mut names = Names { state: 42 };
    (0..count).map(|_| names.name()).collect()
}

#[test]
fn the_order_is_antisymmetric_and_equal_only_for_equal_names() {
    let names = generated(300);
    for a in &names {
        for b in &names {
            assert_eq!(compare(a, b), compare(b, a).reverse(), "{a:?} vs {b:?}");
            assert_eq!(compare(a, b) == Ordering::Equal, a == b, "{a:?} vs {b:?}");
        }
    }
}

#[test]
fn the_order_is_transitive() {
    let names = generated(80);
    for a in &names {
        for b in &names {
            if compare(a, b) != Ordering::Less {
                continue;
            }
            for c in &names {
                if compare(b, c) == Ordering::Less {
                    assert_eq!(compare(a, c), Ordering::Less, "{a:?} < {b:?} < {c:?}");
                }
            }
        }
    }
}

#[test]
fn sorting_any_shuffle_gives_one_order() {
    let names = generated(200);
    let mut forward = names.clone();
    let mut backward: Vec<String> = names.into_iter().rev().collect();
    sort(&mut forward);
    sort(&mut backward);
    assert_eq!(forward, backward);
    for pair in forward.windows(2) {
        assert_ne!(compare(&pair[0], &pair[1]), Ordering::Greater);
    }
}
