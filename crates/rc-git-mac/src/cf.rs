//! The `CoreFoundation` calls the other modules need, an owning wrapper that releases each
//! object exactly once, and conversion between property lists and [`Value`].

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::ptr::{self, NonNull};

use rc_git_kit::platform::settings::Value;

/// Any `CoreFoundation` object.
pub type CFTypeRef = *const c_void;
type CFIndex = isize;
type CFTypeID = usize;
type Boolean = u8;

const UTF8: u32 = 0x0800_0100;
const NUMBER_SINT64: CFIndex = 4;
const NUMBER_FLOAT64: CFIndex = 6;
/// Nesting deeper than this in a stored value is treated as unreadable.
const MAX_DEPTH: usize = 64;

#[repr(C)]
struct CFRange {
    location: CFIndex,
    length: CFIndex,
}

/// An opaque framework struct only ever used by address.
#[repr(C)]
pub struct Opaque {
    _private: [u8; 0],
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(object: CFTypeRef);
    fn CFRetain(object: CFTypeRef) -> CFTypeRef;
    fn CFGetTypeID(object: CFTypeRef) -> CFTypeID;
    fn CFStringGetTypeID() -> CFTypeID;
    fn CFBooleanGetTypeID() -> CFTypeID;
    fn CFNumberGetTypeID() -> CFTypeID;
    fn CFDataGetTypeID() -> CFTypeID;
    fn CFArrayGetTypeID() -> CFTypeID;
    fn CFDictionaryGetTypeID() -> CFTypeID;
    fn CFStringCreateWithBytes(
        allocator: CFTypeRef,
        bytes: *const u8,
        length: CFIndex,
        encoding: u32,
        external: Boolean,
    ) -> CFTypeRef;
    fn CFStringGetLength(string: CFTypeRef) -> CFIndex;
    fn CFStringGetBytes(
        string: CFTypeRef,
        range: CFRange,
        encoding: u32,
        loss_byte: u8,
        external: Boolean,
        buffer: *mut u8,
        max_length: CFIndex,
        used_length: *mut CFIndex,
    ) -> CFIndex;
    static kCFBooleanTrue: CFTypeRef;
    static kCFBooleanFalse: CFTypeRef;
    fn CFBooleanGetValue(boolean: CFTypeRef) -> Boolean;
    fn CFNumberCreate(allocator: CFTypeRef, kind: CFIndex, value: *const c_void) -> CFTypeRef;
    fn CFNumberGetValue(number: CFTypeRef, kind: CFIndex, value: *mut c_void) -> Boolean;
    fn CFNumberIsFloatType(number: CFTypeRef) -> Boolean;
    fn CFDataCreate(allocator: CFTypeRef, bytes: *const u8, length: CFIndex) -> CFTypeRef;
    fn CFDataGetLength(data: CFTypeRef) -> CFIndex;
    fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
    static kCFTypeArrayCallBacks: Opaque;
    fn CFArrayCreate(
        allocator: CFTypeRef,
        values: *const CFTypeRef,
        count: CFIndex,
        callbacks: *const Opaque,
    ) -> CFTypeRef;
    fn CFArrayGetCount(array: CFTypeRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(array: CFTypeRef, index: CFIndex) -> CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: Opaque;
    static kCFTypeDictionaryValueCallBacks: Opaque;
    fn CFDictionaryCreate(
        allocator: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: CFIndex,
        key_callbacks: *const Opaque,
        value_callbacks: *const Opaque,
    ) -> CFTypeRef;
    fn CFDictionaryGetCount(dictionary: CFTypeRef) -> CFIndex;
    fn CFDictionaryGetKeysAndValues(
        dictionary: CFTypeRef,
        keys: *mut CFTypeRef,
        values: *mut CFTypeRef,
    );
    fn CFDictionaryGetValue(dictionary: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
}

/// An immutable `CoreFoundation` object this process owns one reference to, released on drop.
#[derive(Debug)]
pub struct Owned(NonNull<c_void>);

// SAFETY: only immutable CF objects (strings, numbers, booleans, data, arrays and
// dictionaries made by their non-mutable create functions, or copied out of a framework) are
// wrapped. Immutable CF objects may be read, retained and released from any thread.
unsafe impl Send for Owned {}
// SAFETY: as for `Send`; `&Owned` only ever reads the object.
unsafe impl Sync for Owned {}

impl Owned {
    /// Takes ownership of `object` under the Create rule; `None` for null.
    ///
    /// # Safety
    /// `object` is null or an immutable CF object whose reference the caller owns and hands
    /// over, so it is released exactly once, here.
    #[must_use]
    pub unsafe fn from_create(object: CFTypeRef) -> Option<Self> {
        NonNull::new(object.cast_mut()).map(Self)
    }

    /// Retains a borrowed `object` (the Get rule) so it outlives its owner.
    ///
    /// # Safety
    /// `object` is null or a live immutable CF object.
    #[must_use]
    pub unsafe fn retained(object: CFTypeRef) -> Option<Self> {
        if object.is_null() {
            return None;
        }
        // SAFETY: `object` is a live CF object per the caller; the retain is balanced by the
        // release in `Drop`.
        unsafe { Self::from_create(CFRetain(object)) }
    }

    /// The object, borrowed for as long as `self` lives.
    #[must_use]
    pub fn as_ptr(&self) -> CFTypeRef {
        self.0.as_ptr().cast_const()
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: `self` owns exactly one reference to a live object, released once here.
        unsafe { CFRelease(self.as_ptr()) }
    }
}

/// A CF string holding `text`; `None` only when `CoreFoundation` cannot allocate it.
#[must_use]
pub fn string(text: &str) -> Option<Owned> {
    let length = CFIndex::try_from(text.len()).ok()?;
    // SAFETY: the bytes are valid UTF-8 for `length` bytes and copied by the call
    // (`external` false); the result follows the Create rule.
    unsafe {
        Owned::from_create(CFStringCreateWithBytes(
            ptr::null(),
            text.as_ptr(),
            length,
            UTF8,
            0,
        ))
    }
}

/// A CF array of `items`, each retained by the array.
#[must_use]
pub fn array(items: &[Owned]) -> Option<Owned> {
    let pointers: Vec<CFTypeRef> = items.iter().map(Owned::as_ptr).collect();
    let count = CFIndex::try_from(pointers.len()).ok()?;
    // SAFETY: `pointers` holds `count` live CF objects kept alive by `items` for the call; the
    // type callbacks retain each, and the result follows the Create rule.
    unsafe {
        Owned::from_create(CFArrayCreate(
            ptr::null(),
            pointers.as_ptr(),
            count,
            &raw const kCFTypeArrayCallBacks,
        ))
    }
}

/// A CF dictionary of `pairs`, given as borrowed pointers the caller keeps alive for the call.
#[must_use]
pub fn dictionary(pairs: &[(CFTypeRef, CFTypeRef)]) -> Option<Owned> {
    let keys: Vec<CFTypeRef> = pairs.iter().map(|pair| pair.0).collect();
    let values: Vec<CFTypeRef> = pairs.iter().map(|pair| pair.1).collect();
    let count = CFIndex::try_from(pairs.len()).ok()?;
    // SAFETY: both arrays hold `count` live CF objects (the caller's contract); the type
    // callbacks retain each, and the result follows the Create rule.
    unsafe {
        Owned::from_create(CFDictionaryCreate(
            ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            count,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        ))
    }
}

/// The value under `key` in `dict`, borrowed from it.
///
/// # Safety
/// `dict` is a live CF dictionary and `key` a live CF object.
#[must_use]
pub unsafe fn dictionary_get(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef {
    // SAFETY: the caller's contract.
    unsafe { CFDictionaryGetValue(dict, key) }
}

/// The shared `kCFBooleanTrue` or `kCFBooleanFalse`, borrowed.
#[must_use]
pub fn boolean(flag: bool) -> CFTypeRef {
    // SAFETY: both are framework constants, initialized before any Rust code runs.
    unsafe {
        if flag {
            kCFBooleanTrue
        } else {
            kCFBooleanFalse
        }
    }
}

/// A CF object holding `value`; `None` when `CoreFoundation` cannot build part of it.
#[must_use]
pub fn from_value(value: &Value) -> Option<Owned> {
    match value {
        Value::String(text) => string(text),
        // SAFETY: the shared booleans are live constants.
        Value::Bool(flag) => unsafe { Owned::retained(boolean(*flag)) },
        // SAFETY: the number is read from a live local for the call; Create rule.
        Value::Int(number) => unsafe {
            Owned::from_create(CFNumberCreate(
                ptr::null(),
                NUMBER_SINT64,
                ptr::from_ref(number).cast(),
            ))
        },
        // SAFETY: as above.
        Value::Double(number) => unsafe {
            Owned::from_create(CFNumberCreate(
                ptr::null(),
                NUMBER_FLOAT64,
                ptr::from_ref(number).cast(),
            ))
        },
        Value::Data(bytes) => data(bytes),
        Value::Array(items) => {
            let mut owned = Vec::with_capacity(items.len());
            for item in items {
                owned.push(from_value(item)?);
            }
            array(&owned)
        }
        Value::Dict(map) => dict_from_map(map),
    }
}

/// A CF data object holding a copy of `bytes`.
#[must_use]
pub fn data(bytes: &[u8]) -> Option<Owned> {
    let length = CFIndex::try_from(bytes.len()).ok()?;
    // SAFETY: `bytes` is valid for `length` bytes and copied; Create rule.
    unsafe { Owned::from_create(CFDataCreate(ptr::null(), bytes.as_ptr(), length)) }
}

fn dict_from_map(map: &BTreeMap<String, Value>) -> Option<Owned> {
    let mut keys = Vec::with_capacity(map.len());
    let mut values = Vec::with_capacity(map.len());
    for (key, value) in map {
        keys.push(string(key)?);
        values.push(from_value(value)?);
    }
    let pairs: Vec<(CFTypeRef, CFTypeRef)> = keys
        .iter()
        .zip(&values)
        .map(|(key, value)| (key.as_ptr(), value.as_ptr()))
        .collect();
    dictionary(&pairs)
}

/// The text of a CF string.
///
/// # Safety
/// `object` is a live CF string.
#[must_use]
pub unsafe fn string_text(object: CFTypeRef) -> Option<String> {
    // SAFETY: `object` is a live CF string per the caller.
    let length = unsafe { CFStringGetLength(object) };
    let range = || CFRange {
        location: 0,
        length,
    };
    let mut needed: CFIndex = 0;
    // SAFETY: a null buffer asks only for the byte count, written to `needed`.
    unsafe {
        CFStringGetBytes(
            object,
            range(),
            UTF8,
            0,
            0,
            ptr::null_mut(),
            0,
            &raw mut needed,
        )
    };
    let mut bytes = vec![0_u8; usize::try_from(needed).ok()?];
    let mut used: CFIndex = 0;
    // SAFETY: `bytes` has room for `needed` bytes, the count the previous call reported.
    unsafe {
        CFStringGetBytes(
            object,
            range(),
            UTF8,
            0,
            0,
            bytes.as_mut_ptr(),
            needed,
            &raw mut used,
        );
    }
    bytes.truncate(usize::try_from(used).ok()?);
    String::from_utf8(bytes).ok()
}

/// The bytes of a CF data object.
///
/// # Safety
/// `object` is a live CF data object.
#[must_use]
pub unsafe fn data_bytes(object: CFTypeRef) -> Vec<u8> {
    // SAFETY: `object` is live CF data per the caller.
    let length = unsafe { CFDataGetLength(object) };
    let Ok(length) = usize::try_from(length) else {
        return Vec::new();
    };
    if length == 0 {
        return Vec::new();
    }
    // SAFETY: the pointer is valid for `length` bytes while `object` lives, and copied here.
    unsafe { std::slice::from_raw_parts(CFDataGetBytePtr(object), length).to_vec() }
}

/// The [`Value`] a property-list object holds; `None` for a type outside it (a date) or for
/// nesting past a sane depth. In an array one such element makes the whole array unreadable,
/// so a string list never silently loses an entry; in a dictionary it drops only that entry.
///
/// # Safety
/// `object` is a live CF object.
#[must_use]
pub unsafe fn to_value(object: CFTypeRef) -> Option<Value> {
    // SAFETY: the caller's contract.
    unsafe { to_value_at(object, 0) }
}

unsafe fn to_value_at(object: CFTypeRef, depth: usize) -> Option<Value> {
    if depth > MAX_DEPTH {
        return None;
    }
    // SAFETY: `object` is live per the caller; each branch reads it as the type it has.
    unsafe {
        let kind = CFGetTypeID(object);
        if kind == CFStringGetTypeID() {
            string_text(object).map(Value::String)
        } else if kind == CFBooleanGetTypeID() {
            Some(Value::Bool(CFBooleanGetValue(object) != 0))
        } else if kind == CFNumberGetTypeID() {
            Some(number_value(object))
        } else if kind == CFDataGetTypeID() {
            Some(Value::Data(data_bytes(object)))
        } else if kind == CFArrayGetTypeID() {
            array_value(object, depth)
        } else if kind == CFDictionaryGetTypeID() {
            Some(dict_value(object, depth))
        } else {
            None
        }
    }
}

unsafe fn number_value(object: CFTypeRef) -> Value {
    // SAFETY: `object` is a live CF number; each read writes into a local of the asked type.
    unsafe {
        if CFNumberIsFloatType(object) != 0 {
            let mut number = 0.0_f64;
            CFNumberGetValue(object, NUMBER_FLOAT64, (&raw mut number).cast());
            Value::Double(number)
        } else {
            let mut number = 0_i64;
            CFNumberGetValue(object, NUMBER_SINT64, (&raw mut number).cast());
            Value::Int(number)
        }
    }
}

unsafe fn array_value(object: CFTypeRef, depth: usize) -> Option<Value> {
    // SAFETY: `object` is a live CF array and every index is below its count.
    unsafe {
        let count = CFArrayGetCount(object);
        let mut items = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
        for index in 0..count {
            items.push(to_value_at(
                CFArrayGetValueAtIndex(object, index),
                depth + 1,
            )?);
        }
        Some(Value::Array(items))
    }
}

unsafe fn dict_value(object: CFTypeRef, depth: usize) -> Value {
    // SAFETY: `object` is a live CF dictionary; both buffers hold its count of entries.
    unsafe {
        let count = usize::try_from(CFDictionaryGetCount(object)).unwrap_or(0);
        let mut keys: Vec<CFTypeRef> = vec![ptr::null(); count];
        let mut values: Vec<CFTypeRef> = vec![ptr::null(); count];
        CFDictionaryGetKeysAndValues(object, keys.as_mut_ptr(), values.as_mut_ptr());
        let mut map = BTreeMap::new();
        for (key, value) in keys.into_iter().zip(values) {
            if CFGetTypeID(key) != CFStringGetTypeID() {
                continue;
            }
            let (Some(key), Some(value)) = (string_text(key), to_value_at(value, depth + 1)) else {
                continue;
            };
            map.insert(key, value);
        }
        Value::Dict(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(value: &Value) -> Option<Value> {
        let object = from_value(value)?;
        // SAFETY: `object` is live for the call.
        unsafe { to_value(object.as_ptr()) }
    }

    #[test]
    fn every_value_type_round_trips() {
        let mut dict = BTreeMap::new();
        dict.insert("name".to_owned(), Value::String("Ừ é 🙂".to_owned()));
        dict.insert("n".to_owned(), Value::Int(-7));
        let values = [
            Value::String(String::new()),
            Value::String("rc-git — host".to_owned()),
            Value::Bool(true),
            Value::Bool(false),
            Value::Int(i64::MAX),
            Value::Double(2.5),
            Value::Data(vec![0, 1, 255]),
            Value::Data(Vec::new()),
            Value::Array(vec![Value::String("/a".to_owned()), Value::Int(3)]),
            Value::Dict(dict),
        ];
        for value in values {
            assert_eq!(round_trip(&value), Some(value.clone()), "{value:?}");
        }
    }

    #[test]
    fn a_retained_object_outlives_its_owner() {
        let original = string("kept").unwrap();
        // SAFETY: `original` is live for the call.
        let copy = unsafe { Owned::retained(original.as_ptr()) }.unwrap();
        drop(original);
        // SAFETY: `copy` holds its own reference.
        let text = unsafe { string_text(copy.as_ptr()) };
        assert_eq!(text.as_deref(), Some("kept"));
        // SAFETY: null is allowed and yields nothing.
        assert!(unsafe { Owned::retained(ptr::null()) }.is_none());
    }
}
