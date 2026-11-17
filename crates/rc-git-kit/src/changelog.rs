//! The changelog Help ▸ Changelog shows: the repository's `CHANGELOG.md`, compiled into the
//! binary. Cargo rebuilds this crate when the file changes, so the window always shows the
//! changelog of the build that is running, and a build made with an uncommitted edit to it is
//! marked dirty by the same stamp as any other uncommitted source.
//!
//! The file is markdown the preview draws as it is. [`problems`] holds it to the shape the
//! repository's `CLAUDE.md` describes, and a test runs it on the shipped file.

use crate::text_document::TextDocument;

/// `CHANGELOG.md` at the repository root, as it stood when this crate was built.
pub const SOURCE: &str = include_str!("../../../CHANGELOG.md");

/// The title the file opens with.
pub const TITLE: &str = "# Changelog";

/// The section headings a day may hold, in the order they appear.
pub const SECTIONS: [&str; 3] = ["Added", "Changed", "Fixed"];

/// [`SOURCE`] prepared for the markdown preview.
#[must_use]
pub fn document() -> TextDocument {
    let mut starts = Vec::new();
    if !SOURCE.is_empty() {
        starts.push(0);
    }
    for (index, byte) in SOURCE.bytes().enumerate() {
        if byte == b'\n' && index + 1 < SOURCE.len() {
            starts.push(index + 1);
        }
    }
    TextDocument::new(SOURCE.as_bytes().to_vec(), &starts, true)
}

/// Every way `text` departs from the changelog's shape, one message per problem naming its
/// line: the file opens with [`TITLE`]; each day is a `## YYYY-MM-DD` heading older than the
/// one above it; a day holds one or more of [`SECTIONS`], each once and in that order; each
/// section holds at least one `- ` entry. Prose between the title and the first day is
/// allowed, and so are wrapped entry lines. Empty when the file is well formed.
#[must_use]
pub fn problems(text: &str) -> Vec<String> {
    let mut check = Check::default();
    let mut lines = text.lines().enumerate();
    match lines.next() {
        Some((_, line)) if line == TITLE => {}
        Some(_) | None => check.report(1, &format!("the file must open with `{TITLE}`")),
    }
    for (index, line) in lines {
        check.line(index + 1, line);
    }
    check.close_day(text.lines().count() + 1);
    check.problems
}

/// The walk's state: the day and section it is inside, and what each has held so far.
#[derive(Default)]
struct Check {
    problems: Vec<String>,
    /// The current day's date and the line its heading is on.
    day: Option<(String, usize)>,
    /// The rank in [`SECTIONS`] of the current day's last section.
    section: Option<usize>,
    /// The line of the current section's heading, while it has no entry yet.
    empty_section: Option<usize>,
}

impl Check {
    fn report(&mut self, line: usize, message: &str) {
        self.problems.push(format!("line {line}: {message}"));
    }

    fn line(&mut self, number: usize, line: &str) {
        if let Some(date) = line.strip_prefix("## ") {
            self.day(number, date);
        } else if let Some(name) = line.strip_prefix("### ") {
            self.section(number, name);
        } else if line.starts_with('#') {
            self.report(
                number,
                &format!("only `## ` days and `### ` sections: `{line}`"),
            );
        } else if line.starts_with("- ") {
            if self.section.is_none() {
                self.report(number, "an entry outside a section");
            }
            self.empty_section = None;
        }
    }

    fn day(&mut self, number: usize, date: &str) {
        self.close_day(number);
        if !is_date(date) {
            self.report(number, &format!("`{date}` is not a YYYY-MM-DD date"));
        } else if let Some((above, _)) = &self.day
            && date >= above.as_str()
        {
            let message = format!("{date} is not older than {above} above it");
            self.report(number, &message);
        }
        self.day = Some((date.to_owned(), number));
        self.section = None;
    }

    fn section(&mut self, number: usize, name: &str) {
        self.close_section();
        if self.day.is_none() {
            self.report(number, &format!("section `{name}` comes before any day"));
        }
        let Some(rank) = SECTIONS.iter().position(|section| *section == name) else {
            let message = format!("`{name}` is not one of {}", SECTIONS.join(", "));
            self.report(number, &message);
            return;
        };
        if self.section.is_some_and(|previous| rank <= previous) {
            let message = format!("`{name}` repeats or comes out of the order {SECTIONS:?}");
            self.report(number, &message);
        }
        self.section = Some(rank);
        self.empty_section = Some(number);
    }

    fn close_section(&mut self) {
        if let Some(heading) = self.empty_section.take() {
            self.report(heading, "a section with no `- ` entry");
        }
    }

    /// Ends the current day at `number`, the line after it.
    fn close_day(&mut self, number: usize) {
        self.close_section();
        if let Some((date, heading)) = &self.day
            && self.section.is_none()
        {
            let message = format!("{date} has no section (ends before line {number})");
            self.problems.push(format!("line {heading}: {message}"));
        }
    }
}

/// Whether `text` is a `YYYY-MM-DD` date with a month 01-12 and a day 01-31.
fn is_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    let shape = bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        });
    if !shape {
        return false;
    }
    let month = text.get(5..7).and_then(|month| month.parse::<u8>().ok());
    let day = text.get(8..10).and_then(|day| day.parse::<u8>().ok());
    matches!(month, Some(1..=12)) && matches!(day, Some(1..=31))
}
