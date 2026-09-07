//! Object identity.

use std::fmt;

use gix::ObjectId;

use crate::error::{Error, Result};

/// Identity of a git object: the hash of its content.
///
/// Two objects with equal ids have equal content, so anything keyed by `Oid` never goes stale.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Oid(ObjectId);

impl Oid {
    /// Parses a full hexadecimal id (40 characters for SHA-1).
    ///
    /// # Errors
    /// [`Error::InvalidOid`] when `hex` has the wrong length or non-hex characters.
    pub fn from_hex(hex: &str) -> Result<Self> {
        ObjectId::from_hex(hex.as_bytes())
            .map(Self)
            .map_err(|_| Error::InvalidOid {
                input: hex.to_owned(),
            })
    }

    /// Builds an id from raw hash bytes. `None` when the length matches no supported hash.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        ObjectId::try_from(bytes).ok().map(Self)
    }

    /// Raw hash bytes (20 for SHA-1).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }

    /// Lowercase hexadecimal form.
    #[must_use]
    pub fn to_hex(&self) -> String {
        self.0.to_string()
    }

    pub(crate) fn from_gix(id: impl Into<ObjectId>) -> Self {
        Self(id.into())
    }

    pub(crate) fn as_gix(&self) -> &ObjectId {
        &self.0
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Oid({})", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn hex_round_trips() {
        let oid = Oid::from_hex(HEX).unwrap();
        assert_eq!(oid.to_hex(), HEX);
        assert_eq!(oid.to_string(), HEX);
        assert_eq!(format!("{oid:?}"), format!("Oid({HEX})"));
        assert_eq!(oid.as_bytes().len(), 20);
        assert_eq!(Oid::from_bytes(oid.as_bytes()), Some(oid));
    }

    #[test]
    fn uppercase_hex_is_accepted_and_normalised() {
        let oid = Oid::from_hex(&HEX.to_uppercase()).unwrap();
        assert_eq!(oid.to_hex(), HEX);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(
            Oid::from_hex("abc"),
            Err(Error::InvalidOid { .. })
        ));
        assert!(matches!(
            Oid::from_hex(&"g".repeat(40)),
            Err(Error::InvalidOid { .. })
        ));
        assert_eq!(Oid::from_bytes(&[0; 3]), None);
    }
}
