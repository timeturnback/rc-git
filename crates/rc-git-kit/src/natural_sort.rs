//! Port of Foundation's `localizedStandardCompare`, as `RefFilter`, `ChangeTree` and
//! `HostKeyStore` use it: the order Finder sorts names in.
//!
//! Runs of ASCII digits compare by value, so `v2` sorts before `v10`; letters compare without
//! case. Names equal under those rules are told apart by case, lowercase first, and then by
//! their bytes, so the order is total and only equal strings compare equal: a sort by it is the
//! same on every run.
//!
//! Where it differs from Foundation: there is no locale and no collation table. Characters
//! that are not ASCII digits or letters sort before digits, digits before letters, and within
//! each class by their lowercased code points. Accented letters compare by code point, not
//! next to their base letter, and only ASCII digits form numbers.

use std::cmp::Ordering;

/// One unit of a name as the comparison sees it.
#[derive(Debug, PartialEq, Eq)]
enum Token<'a> {
    /// A run of ASCII digits with its leading zeros dropped (empty for an all-zero run).
    Number(&'a str),
    /// One character that is not an ASCII digit.
    Char(char),
}

/// Where a character sorts relative to the other kinds, lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    Symbol,
    Digit,
    Letter,
}

/// Compares two names the way Finder sorts them. Total: equal only for equal strings.
#[must_use]
pub fn compare(a: &str, b: &str) -> Ordering {
    primary(a, b)
        .then_with(|| case_order(a, b))
        .then_with(|| a.cmp(b))
}

/// Whether `a` sorts strictly before `b`; the shape `sort_by` and filters want.
#[must_use]
pub fn precedes(a: &str, b: &str) -> bool {
    compare(a, b) == Ordering::Less
}

/// Sorts `names` in natural order.
pub fn sort<S: AsRef<str>>(names: &mut [S]) {
    names.sort_by(|a, b| compare(a.as_ref(), b.as_ref()));
}

/// Numbers by value, everything else without case.
fn primary(a: &str, b: &str) -> Ordering {
    let mut left = Tokens::new(a);
    let mut right = Tokens::new(b);
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let order = compare_tokens(&x, &y);
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

fn compare_tokens(x: &Token<'_>, y: &Token<'_>) -> Ordering {
    match (x, y) {
        (Token::Number(m), Token::Number(n)) => m.len().cmp(&n.len()).then_with(|| m.cmp(n)),
        (Token::Number(_), Token::Char(c)) => Class::Digit.cmp(&class(*c)),
        (Token::Char(c), Token::Number(_)) => class(*c).cmp(&Class::Digit),
        (Token::Char(c), Token::Char(d)) => class(*c)
            .cmp(&class(*d))
            .then_with(|| c.to_lowercase().cmp(d.to_lowercase())),
    }
}

fn class(c: char) -> Class {
    if c.is_ascii_digit() {
        Class::Digit
    } else if c.is_alphabetic() {
        Class::Letter
    } else {
        Class::Symbol
    }
}

/// Lowercase before uppercase at the first character where the names differ in case, for
/// names the primary order holds equal.
fn case_order(a: &str, b: &str) -> Ordering {
    let flip = |c: char| -> (bool, char) { (c.is_uppercase(), c) };
    a.chars().map(flip).cmp(b.chars().map(flip))
}

/// Splits a name into numbers and single characters.
struct Tokens<'a> {
    rest: &'a str,
}

impl<'a> Tokens<'a> {
    fn new(text: &'a str) -> Self {
        Self { rest: text }
    }
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        let first = self.rest.chars().next()?;
        if first.is_ascii_digit() {
            let end = self
                .rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(self.rest.len());
            let (digits, rest) = self.rest.split_at(end);
            self.rest = rest;
            return Some(Token::Number(digits.trim_start_matches('0')));
        }
        self.rest = &self.rest[first.len_utf8()..];
        Some(Token::Char(first))
    }
}
