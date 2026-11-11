//! Port of `ImportLinks.swift` minus its UTF-16 half: the import links of a source file, and
//! the map a viewer hit-tests the pointer against. Everything stays a byte offset into the
//! UTF-8 text, which is what GPUI hit-tests in, so `ImportLinkMap`'s UTF-16 walk has no
//! successor.

use std::ops::Range;
use std::sync::Arc;

use rc_git_highlight::{ImportLink, Language};

use crate::highlighting::{BYTE_LIMIT, Plain};

/// The links of `text`, read as the language `name`'s extension implies, ordered by start and
/// never overlapping. Empty when the language has no links or the file has none.
///
/// # Errors
/// [`Plain::UnknownLanguage`] when the extension maps to no known language,
/// [`Plain::TooLarge`] over the highlighter's cap, [`Plain::Failed`] when the parse fails. The
/// viewer shows no links for any of them.
pub fn find(name: &str, text: &[u8]) -> Result<Vec<ImportLink>, Plain> {
    let language = Language::from_file_name(name).ok_or(Plain::UnknownLanguage)?;
    if text.len() > BYTE_LIMIT {
        return Err(Plain::TooLarge);
    }
    rc_git_highlight::import_links(text, language).map_err(|error| Plain::Failed(error.to_string()))
}

/// What the pointer is over: the clickable byte range and the specifier it leads to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hit {
    /// The clickable range, in bytes of the text.
    pub range: Range<usize>,
    /// The module specifier as written.
    pub specifier: String,
}

/// The import links of one document, ready for hit-testing under the pointer.
#[derive(Debug, Clone)]
pub struct ImportLinkMap {
    links: Vec<ImportLink>,
    text: Arc<str>,
}

impl ImportLinkMap {
    /// A map over `links` found in `text`.
    #[must_use]
    pub fn new(links: Vec<ImportLink>, text: Arc<str>) -> Self {
        Self { links, text }
    }

    /// Whether there is no link to hit.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// The link covering byte `offset`, found by binary search. `None` between links and for
    /// a specifier whose bytes do not cut the text at character boundaries.
    #[must_use]
    pub fn hit(&self, offset: usize) -> Option<Hit> {
        let index = self
            .links
            .partition_point(|link| link.end as usize <= offset);
        let link = self.links.get(index)?;
        if link.start as usize > offset {
            return None;
        }
        let specifier = self
            .text
            .get(link.specifier_start as usize..link.specifier_end as usize)?;
        Some(Hit {
            range: link.start as usize..link.end as usize,
            specifier: specifier.to_owned(),
        })
    }
}
