//! The running bundle's `Info.plist` (`Bundle.main.infoDictionary` in the Swift app), read
//! through `CFBundleGetMainBundle` and `CFBundleGetInfoDictionary`, for
//! [`rc_git_kit::app_build::AppBuild::from_info`].

use std::collections::HashMap;

use rc_git_kit::platform::settings::Value;

use crate::cf::{self, CFTypeRef};

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFBundleGetMainBundle() -> CFTypeRef;
    fn CFBundleGetInfoDictionary(bundle: CFTypeRef) -> CFTypeRef;
}

/// The main bundle's `Info.plist` string keys. Keys holding anything but a string are left
/// out, and a process with no bundle (a test binary, a bare `cargo run`) reads as whatever
/// `CoreFoundation` makes of its directory, usually nothing `AppBuild` reads: an unstamped
/// build.
#[must_use]
pub fn main_bundle_info() -> HashMap<String, String> {
    // SAFETY: `CFBundleGetMainBundle` takes no arguments and returns the main bundle, or null,
    // under the Get rule: it is not ours to release and lives as long as the process.
    let bundle = unsafe { CFBundleGetMainBundle() };
    if bundle.is_null() {
        return HashMap::new();
    }
    // SAFETY: `bundle` is the live main bundle. The dictionary comes back under the Get rule
    // (owned by the bundle) or null; `retained` takes a reference of our own before reading.
    let info = unsafe { cf::Owned::retained(CFBundleGetInfoDictionary(bundle)) };
    let Some(info) = info else {
        return HashMap::new();
    };
    // SAFETY: `info` is a live CF dictionary this function holds a reference to.
    let value = unsafe { cf::to_value(info.as_ptr()) };
    strings(value)
}

/// The string entries of a property-list dictionary.
fn strings(value: Option<Value>) -> HashMap<String, String> {
    let mut found = HashMap::new();
    let Some(Value::Dict(entries)) = value else {
        return found;
    };
    for (key, entry) in entries {
        if let Value::String(text) = entry {
            found.insert(key, text);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn only_string_entries_are_kept() {
        let mut entries = BTreeMap::new();
        entries.insert(
            "RCGitSourceRoot".to_owned(),
            Value::String("/src".to_owned()),
        );
        entries.insert("LSUIElement".to_owned(), Value::Bool(true));
        entries.insert("CFBundleVersion".to_owned(), Value::String("42".to_owned()));
        let found = strings(Some(Value::Dict(entries)));
        assert_eq!(found.len(), 2);
        assert_eq!(
            found.get("RCGitSourceRoot").map(String::as_str),
            Some("/src")
        );
        assert_eq!(found.get("CFBundleVersion").map(String::as_str), Some("42"));
    }

    #[test]
    fn anything_but_a_dictionary_reads_as_empty() {
        assert!(strings(None).is_empty());
        assert!(strings(Some(Value::String("x".to_owned()))).is_empty());
    }

    #[test]
    fn the_test_binary_reads_without_crashing() {
        let info = main_bundle_info();
        assert!(info.values().all(|value| !value.contains('\0')));
    }
}
