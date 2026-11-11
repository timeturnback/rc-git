//! `CommonMark`'s process-emphasis pass over one block. Runs of `*`, `_` and `~~` outside code
//! spans and escapes become a delimiter stack; each closer, left to right, pairs with the
//! nearest opener that may take it, and the delimiters between the two drop out as literal
//! text. A failed search records where it stopped per closer kind, so no opener is walked twice
//! for nothing and the pass stays linear.

use std::collections::HashMap;

use super::ascii::{is_escapable, is_space};

/// One matched emphasis: `width` delimiter bytes open it, and the `width` bytes at `close`
/// close it.
#[derive(Debug, Clone, Copy)]
pub(super) struct EmphasisPair {
    pub(super) close: usize,
    pub(super) width: usize,
}

/// A link in the delimiter list; `None` ends it at either side.
type Link = Option<usize>;

struct Delimiter {
    marker: u8,
    /// The run's whole length, which the rule of three reads.
    length: usize,
    can_open: bool,
    can_close: bool,
    /// The bytes not yet matched: openers give theirs up from the end, closers from the
    /// start, so the matched bytes are always the ones next to the text they wrap.
    low: usize,
    high: usize,
    previous: Link,
    next: Link,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Neighbor {
    Space,
    Punctuation,
    Other,
}

/// The pairs in `bytes`, keyed by the first byte of each opening delimiter. `spans` maps each
/// code span's start to its end; delimiters inside one do not count.
pub(super) fn pairs(bytes: &[u8], spans: &HashMap<usize, usize>) -> HashMap<usize, EmphasisPair> {
    let mut pairing = Pairing {
        bytes,
        delimiters: Vec::new(),
        pairs: HashMap::new(),
    };
    pairing.collect(spans);
    pairing.process();
    pairing.pairs
}

struct Pairing<'a> {
    bytes: &'a [u8],
    delimiters: Vec<Delimiter>,
    pairs: HashMap<usize, EmphasisPair>,
}

impl Pairing<'_> {
    fn collect(&mut self, spans: &HashMap<usize, usize>) {
        let mut cursor = 0;
        while let Some(&marker) = self.bytes.get(cursor) {
            if marker == b'\\' {
                cursor += 2;
                continue;
            }
            if let Some(&span_end) = spans.get(&cursor) {
                cursor = span_end;
                continue;
            }
            if marker != b'*' && marker != b'_' && marker != b'~' {
                cursor += 1;
                continue;
            }
            let mut stop = cursor;
            while self.bytes.get(stop) == Some(&marker) {
                stop += 1;
            }
            self.push(marker, cursor, stop);
            cursor = stop;
        }
    }

    /// Classifies the run by what flanks it. Strikethrough takes only `~~`.
    fn push(&mut self, marker: u8, start: usize, end: usize) {
        let length = end - start;
        if marker == b'~' && length != 2 {
            return;
        }
        let before = start
            .checked_sub(1)
            .and_then(|at| self.bytes.get(at))
            .map_or(Neighbor::Space, |&byte| neighbor(byte));
        let after = self
            .bytes
            .get(end)
            .map_or(Neighbor::Space, |&byte| neighbor(byte));
        let left = after != Neighbor::Space
            && (after != Neighbor::Punctuation || before != Neighbor::Other);
        let right = before != Neighbor::Space
            && (before != Neighbor::Punctuation || after != Neighbor::Other);
        let underscore = marker == b'_';
        let can_open = left && (!underscore || !right || before == Neighbor::Punctuation);
        let can_close = right && (!underscore || !left || after == Neighbor::Punctuation);
        if !can_open && !can_close {
            return;
        }
        let index = self.delimiters.len();
        self.delimiters.push(Delimiter {
            marker,
            length,
            can_open,
            can_close,
            low: start,
            high: end,
            previous: index.checked_sub(1),
            next: Some(index + 1),
        });
    }

    fn process(&mut self) {
        if let Some(last) = self.delimiters.last_mut() {
            last.next = None;
        }
        let mut floors: HashMap<usize, Link> = HashMap::new();
        let mut current = (!self.delimiters.is_empty()).then_some(0);
        while let Some(index) = current {
            let Some(closer) = self.delimiters.get(index) else {
                break;
            };
            if !closer.can_close {
                current = closer.next;
                continue;
            }
            let kind = (usize::from(closer.marker) << 3)
                + if closer.can_open { 3 } else { 0 }
                + closer.length % 3;
            let floor = floors.get(&kind).copied().flatten();
            if let Some(opener) = self.opener(index, floor) {
                current = self.matched(opener, index);
                continue;
            }
            let (previous, next, can_open) = (closer.previous, closer.next, closer.can_open);
            floors.insert(kind, previous);
            if !can_open {
                self.unlink(index);
            }
            current = next;
        }
    }

    /// The nearest opener after `floor` that the closer at `closer` may pair with.
    fn opener(&self, closer: usize, floor: Link) -> Option<usize> {
        let closing = self.delimiters.get(closer)?;
        let mut candidate = closing.previous;
        while let Some(index) = candidate {
            if floor.is_some_and(|floor| index <= floor) {
                return None;
            }
            let opener = self.delimiters.get(index)?;
            if opener.can_open && opener.marker == closing.marker && may_pair(opener, closing) {
                return Some(index);
            }
            candidate = opener.previous;
        }
        None
    }

    /// Records the pair, drops the delimiters between the two, and returns the delimiter the
    /// pass looks at next: the closer again while it has bytes left.
    fn matched(&mut self, opener: usize, closer: usize) -> Link {
        let (Some(open), Some(close)) = (self.delimiters.get(opener), self.delimiters.get(closer))
        else {
            return None;
        };
        let opener_left = open.high - open.low;
        let closer_left = close.high - close.low;
        let strikethrough = open.marker == b'~';
        let width = if strikethrough || (opener_left >= 2 && closer_left >= 2) {
            2
        } else {
            1
        };
        let close_at = close.low;
        if let Some(open) = self.delimiters.get_mut(opener) {
            open.high -= width;
            open.next = Some(closer);
            self.pairs.insert(
                open.high,
                EmphasisPair {
                    close: close_at,
                    width,
                },
            );
        }
        if let Some(close) = self.delimiters.get_mut(closer) {
            close.low += width;
            close.previous = Some(opener);
        }
        if opener_left == width {
            self.unlink(opener);
        }
        if closer_left != width {
            return Some(closer);
        }
        let next = self.delimiters.get(closer).and_then(|close| close.next);
        self.unlink(closer);
        next
    }

    fn unlink(&mut self, index: usize) {
        let Some(delimiter) = self.delimiters.get(index) else {
            return;
        };
        let (previous, next) = (delimiter.previous, delimiter.next);
        if let Some(before) = previous.and_then(|at| self.delimiters.get_mut(at)) {
            before.next = next;
        }
        if let Some(after) = next.and_then(|at| self.delimiters.get_mut(at)) {
            after.previous = previous;
        }
    }
}

/// The rule of three: when either run could both open and close, their lengths may not sum to
/// a multiple of three unless both are multiples of three.
fn may_pair(opener: &Delimiter, closer: &Delimiter) -> bool {
    if !opener.can_close && !closer.can_open {
        return true;
    }
    let sum = opener.length + closer.length;
    !sum.is_multiple_of(3) || (opener.length.is_multiple_of(3) && closer.length.is_multiple_of(3))
}

/// Bytes past ASCII count as letters: they are never whitespace here and rarely punctuation.
fn neighbor(byte: u8) -> Neighbor {
    if is_space(byte) || byte == b'\n' || byte == b'\r' {
        return Neighbor::Space;
    }
    if is_escapable(byte) {
        Neighbor::Punctuation
    } else {
        Neighbor::Other
    }
}
