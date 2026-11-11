//! The browser model suites, ported from the `RcGitKit` Swift tests, in one test binary so
//! they share one set of helpers.

#![expect(
    clippy::unwrap_used,
    reason = "helpers outside the test fns look up refs and ids the fixtures always hold"
)]

mod diff;
mod history;
mod model;
mod support;
