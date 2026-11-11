//! The successor to `UserDefaults`: a key-value store of property-list values. The app's store
//! (`rc-git-mac`) reads and writes the same `CFPreferences` domain `NSUserDefaults.standard`
//! does, so a value the Swift app saved reads back here under the same key.
//!
//! Typed reads follow `UserDefaults`: a value stored with another type reads as absent (or
//! as `false`/`0` for the scalar reads), never as an error, since another build under the
//! same bundle id may have written the key with a different shape.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// One stored value: the property-list types `UserDefaults` keeps.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A string.
    String(String),
    /// A boolean (`CFBoolean`).
    Bool(bool),
    /// An integer number.
    Int(i64),
    /// A floating-point number.
    Double(f64),
    /// Raw bytes.
    Data(Vec<u8>),
    /// An ordered list.
    Array(Vec<Value>),
    /// String keys to values.
    Dict(BTreeMap<String, Value>),
}

impl Value {
    /// A list of strings.
    #[must_use]
    pub fn strings(items: &[String]) -> Self {
        let mut array = Vec::with_capacity(items.len());
        for item in items {
            array.push(Self::String(item.clone()));
        }
        Self::Array(array)
    }

    /// A map of strings to strings.
    #[must_use]
    pub fn string_map(map: &BTreeMap<String, String>) -> Self {
        let mut dict = BTreeMap::new();
        for (key, value) in map {
            dict.insert(key.clone(), Self::String(value.clone()));
        }
        Self::Dict(dict)
    }

    /// The string, when this is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(text) => Some(text),
            Self::Bool(_)
            | Self::Int(_)
            | Self::Double(_)
            | Self::Data(_)
            | Self::Array(_)
            | Self::Dict(_) => None,
        }
    }

    /// Every element as a string, or `None` when this is not a list or any element is not a
    /// string (`stringArray(forKey:)`).
    #[must_use]
    pub fn as_string_array(&self) -> Option<Vec<String>> {
        let Self::Array(items) = self else {
            return None;
        };
        let mut strings = Vec::with_capacity(items.len());
        for item in items {
            strings.push(item.as_str()?.to_owned());
        }
        Some(strings)
    }

    /// The string entries of a map, dropping each entry of another type one by one, so one
    /// bad entry hides no other; `None` when this is not a map.
    #[must_use]
    pub fn as_string_map(&self) -> Option<BTreeMap<String, String>> {
        let Self::Dict(dict) = self else {
            return None;
        };
        let mut map = BTreeMap::new();
        for (key, value) in dict {
            if let Some(text) = value.as_str() {
                map.insert(key.clone(), text.to_owned());
            }
        }
        Some(map)
    }
}

/// A write the store could not make: the operation and the key it was for.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("settings {op} failed for key '{key}': {reason}")]
pub struct SettingsError {
    /// What was attempted: `"set"`, `"remove"` or `"synchronize"`.
    pub op: &'static str,
    /// The key, or the domain for a synchronize.
    pub key: String,
    /// Why, in words.
    pub reason: String,
}

/// A store of settings keyed by string. Shared across threads; every method takes `&self`.
pub trait SettingsStore: Send + Sync {
    /// The value under `key`, or `None` when nothing is stored there or the stored value has
    /// a type outside [`Value`].
    fn value(&self, key: &str) -> Option<Value>;

    /// Stores `value` under `key`, or removes the key when `value` is `None`.
    ///
    /// # Errors
    /// [`SettingsError`] naming the key when the value could not be handed to the store.
    fn set_value(&self, key: &str, value: Option<Value>) -> Result<(), SettingsError>;

    /// Forces pending writes out, so another process reading the domain sees them now.
    ///
    /// # Errors
    /// [`SettingsError`] with op `"synchronize"` when the store reports the flush failed.
    fn synchronize(&self) -> Result<(), SettingsError>;

    /// `string(forKey:)`: the string under `key`.
    fn string(&self, key: &str) -> Option<String> {
        self.value(key)?.as_str().map(str::to_owned)
    }

    /// `bool(forKey:)`: a boolean, or a number read as nonzero; `false` otherwise.
    fn bool(&self, key: &str) -> bool {
        match self.value(key) {
            Some(Value::Bool(flag)) => flag,
            Some(Value::Int(number)) => number != 0,
            Some(Value::Double(number)) => number != 0.0,
            Some(Value::String(_) | Value::Data(_) | Value::Array(_) | Value::Dict(_)) | None => {
                false
            }
        }
    }

    /// `integer(forKey:)`: an integer, a boolean as 0 or 1, a double truncated toward zero, or
    /// a string that parses as an integer; 0 otherwise.
    fn int(&self, key: &str) -> i64 {
        match self.value(key) {
            Some(Value::Int(number)) => number,
            Some(Value::Bool(flag)) => i64::from(flag),
            #[expect(
                clippy::cast_possible_truncation,
                reason = "integer(forKey:) truncates a stored double; out-of-range saturates"
            )]
            Some(Value::Double(number)) => number.trunc() as i64,
            Some(Value::String(text)) => text.trim().parse().unwrap_or(0),
            Some(Value::Data(_) | Value::Array(_) | Value::Dict(_)) | None => 0,
        }
    }

    /// `double(forKey:)`: a number or a boolean as a double; 0 otherwise.
    fn double(&self, key: &str) -> f64 {
        match self.value(key) {
            Some(Value::Double(number)) => number,
            #[expect(
                clippy::cast_precision_loss,
                reason = "double(forKey:) reads an integer as the nearest double"
            )]
            Some(Value::Int(number)) => number as f64,
            Some(Value::Bool(flag)) => f64::from(u8::from(flag)),
            Some(Value::String(_) | Value::Data(_) | Value::Array(_) | Value::Dict(_)) | None => {
                0.0
            }
        }
    }

    /// `stringArray(forKey:)`: a list whose every element is a string.
    fn string_array(&self, key: &str) -> Option<Vec<String>> {
        self.value(key)?.as_string_array()
    }

    /// `dictionary(forKey:)` narrowed to string values, dropping other entries one by one.
    fn string_map(&self, key: &str) -> Option<BTreeMap<String, String>> {
        self.value(key)?.as_string_map()
    }

    /// `data(forKey:)`: the bytes under `key`.
    fn data(&self, key: &str) -> Option<Vec<u8>> {
        match self.value(key)? {
            Value::Data(bytes) => Some(bytes),
            Value::String(_)
            | Value::Bool(_)
            | Value::Int(_)
            | Value::Double(_)
            | Value::Array(_)
            | Value::Dict(_) => None,
        }
    }
}

/// A [`SettingsStore`] held in memory, for tests and for models run without the app.
#[derive(Default)]
pub struct MemorySettings {
    state: Mutex<MemoryState>,
}

#[derive(Debug, Default)]
struct MemoryState {
    values: HashMap<String, Value>,
    synchronizes: usize,
    fail_writes: bool,
}

impl MemorySettings {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, MemoryState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many times [`SettingsStore::synchronize`] ran.
    #[must_use]
    pub fn synchronize_count(&self) -> usize {
        self.lock().synchronizes
    }

    /// Makes every later write and synchronize fail (or succeed again), to test error paths.
    pub fn fail_writes(&self, fail: bool) {
        self.lock().fail_writes = fail;
    }

    /// Every stored key, sorted.
    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.lock().values.keys().cloned().collect();
        keys.sort();
        keys
    }
}

impl fmt::Debug for MemorySettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemorySettings")
            .field("keys", &self.keys())
            .finish_non_exhaustive()
    }
}

impl SettingsStore for MemorySettings {
    fn value(&self, key: &str) -> Option<Value> {
        self.lock().values.get(key).cloned()
    }

    fn set_value(&self, key: &str, value: Option<Value>) -> Result<(), SettingsError> {
        let mut state = self.lock();
        if state.fail_writes {
            return Err(SettingsError {
                op: if value.is_some() { "set" } else { "remove" },
                key: key.to_owned(),
                reason: "writes are set to fail".to_owned(),
            });
        }
        match value {
            Some(value) => state.values.insert(key.to_owned(), value),
            None => state.values.remove(key),
        };
        Ok(())
    }

    fn synchronize(&self) -> Result<(), SettingsError> {
        let mut state = self.lock();
        state.synchronizes += 1;
        if state.fail_writes {
            return Err(SettingsError {
                op: "synchronize",
                key: "memory".to_owned(),
                reason: "writes are set to fail".to_owned(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_reads_follow_user_defaults() {
        let store = MemorySettings::new();
        assert_eq!(store.string("k"), None);
        assert!(!store.bool("k"));
        assert_eq!(store.int("k"), 0);
        assert!(store.double("k").abs() < f64::EPSILON);

        store.set_value("b", Some(Value::Bool(true))).unwrap();
        store.set_value("i", Some(Value::Int(7))).unwrap();
        store.set_value("d", Some(Value::Double(2.9))).unwrap();
        store
            .set_value("s", Some(Value::String(" 12 ".into())))
            .unwrap();
        assert!(store.bool("b"));
        assert!(store.bool("i"));
        assert_eq!(store.int("b"), 1);
        assert_eq!(store.int("d"), 2);
        assert_eq!(store.int("s"), 12);
        assert!((store.double("i") - 7.0).abs() < f64::EPSILON);
        assert!(!store.bool("s"));
        assert_eq!(store.string("i"), None);
        assert_eq!(store.string("s").as_deref(), Some(" 12 "));
    }

    #[test]
    fn a_list_with_one_non_string_is_no_string_array() {
        let store = MemorySettings::new();
        store
            .set_value(
                "a",
                Some(Value::Array(vec![
                    Value::String("/a".into()),
                    Value::Int(3),
                ])),
            )
            .unwrap();
        assert_eq!(store.string_array("a"), None);
        store
            .set_value("a", Some(Value::strings(&["/a".into(), "/b".into()])))
            .unwrap();
        assert_eq!(
            store.string_array("a"),
            Some(vec!["/a".into(), "/b".into()])
        );
        store
            .set_value("a", Some(Value::String("oops".into())))
            .unwrap();
        assert_eq!(store.string_array("a"), None);
    }

    #[test]
    fn a_map_drops_only_its_non_string_entries() {
        let store = MemorySettings::new();
        let mut dict = BTreeMap::new();
        dict.insert("good".to_owned(), Value::String("x".into()));
        dict.insert("bad".to_owned(), Value::Int(42));
        store.set_value("m", Some(Value::Dict(dict))).unwrap();
        let map = store.string_map("m").unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("good").map(String::as_str), Some("x"));
    }

    #[test]
    fn data_reads_back_and_remove_clears() {
        let store = MemorySettings::new();
        store.set_value("d", Some(Value::Data(vec![1, 2]))).unwrap();
        assert_eq!(store.data("d"), Some(vec![1, 2]));
        assert_eq!(store.data("missing"), None);
        store.set_value("d", None).unwrap();
        assert_eq!(store.value("d"), None);
    }

    #[test]
    fn a_failing_store_names_the_key() {
        let store = MemorySettings::new();
        store.fail_writes(true);
        let error = store
            .set_value("rc-git.x", Some(Value::Bool(true)))
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "settings set failed for key 'rc-git.x': writes are set to fail"
        );
        assert!(store.synchronize().is_err());
        assert_eq!(store.synchronize_count(), 1);
    }
}
