//! [`SettingsStore`] on `CFPreferences`. With `kCFPreferencesCurrentApplication` it reads and
//! writes the domain `NSUserDefaults.standard` does, so the bundle identifier scopes it and
//! every value the Swift app saved reads back under the same key.

use std::fmt;
use std::ptr;

use rc_git_kit::platform::settings::{SettingsError, SettingsStore, Value};

use crate::cf::{self, CFTypeRef, Owned};

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFPreferencesCurrentApplication: CFTypeRef;
    fn CFPreferencesCopyAppValue(key: CFTypeRef, application: CFTypeRef) -> CFTypeRef;
    fn CFPreferencesSetAppValue(key: CFTypeRef, value: CFTypeRef, application: CFTypeRef);
    fn CFPreferencesAppSynchronize(application: CFTypeRef) -> u8;
}

/// The settings of one preferences domain.
pub struct Preferences {
    /// `None` for the current application's domain.
    domain: Option<Owned>,
    name: String,
}

impl fmt::Debug for Preferences {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Preferences")
            .field("domain", &self.name)
            .finish()
    }
}

impl Preferences {
    /// The running app's own domain, the one `NSUserDefaults.standard` uses.
    #[must_use]
    pub fn current_application() -> Self {
        Self {
            domain: None,
            name: "current application".to_owned(),
        }
    }

    /// The domain named `application`. An absolute path stores the domain at `<path>.plist`
    /// instead of in `~/Library/Preferences`, which is how tests get a scratch domain.
    ///
    /// # Errors
    /// [`SettingsError`] with op `"open"` when the name cannot be made a CF string.
    pub fn domain(application: &str) -> Result<Self, SettingsError> {
        let domain = cf::string(application).ok_or_else(|| SettingsError {
            op: "open",
            key: application.to_owned(),
            reason: "could not make the domain name a CF string".to_owned(),
        })?;
        Ok(Self {
            domain: Some(domain),
            name: application.to_owned(),
        })
    }

    fn application(&self) -> CFTypeRef {
        match &self.domain {
            Some(domain) => domain.as_ptr(),
            // SAFETY: a framework constant, initialized before any Rust code runs.
            None => unsafe { kCFPreferencesCurrentApplication },
        }
    }
}

impl SettingsStore for Preferences {
    fn value(&self, key: &str) -> Option<Value> {
        let key = cf::string(key)?;
        // SAFETY: both arguments are live CF strings; the result follows the Copy rule and is
        // released by `Owned`.
        let stored = unsafe {
            Owned::from_create(CFPreferencesCopyAppValue(key.as_ptr(), self.application()))
        }?;
        // SAFETY: `stored` is live for the call.
        unsafe { cf::to_value(stored.as_ptr()) }
    }

    fn set_value(&self, key: &str, value: Option<Value>) -> Result<(), SettingsError> {
        let op = if value.is_some() { "set" } else { "remove" };
        let failed = |reason: &str| SettingsError {
            op,
            key: key.to_owned(),
            reason: reason.to_owned(),
        };
        let cf_key = cf::string(key).ok_or_else(|| failed("could not make the key a CF string"))?;
        let cf_value = match &value {
            Some(value) => Some(
                cf::from_value(value)
                    .ok_or_else(|| failed("could not make the value a property list"))?,
            ),
            None => None,
        };
        let value_ptr = cf_value.as_ref().map_or(ptr::null(), Owned::as_ptr);
        // SAFETY: key and domain are live CF strings and the value is null (remove) or a live
        // property list; the call copies what it keeps.
        unsafe { CFPreferencesSetAppValue(cf_key.as_ptr(), value_ptr, self.application()) };
        Ok(())
    }

    fn synchronize(&self) -> Result<(), SettingsError> {
        // SAFETY: the domain is a live CF string or the framework constant.
        let flushed = unsafe { CFPreferencesAppSynchronize(self.application()) };
        if flushed == 0 {
            return Err(SettingsError {
                op: "synchronize",
                key: self.name.clone(),
                reason: "CFPreferencesAppSynchronize returned false".to_owned(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
