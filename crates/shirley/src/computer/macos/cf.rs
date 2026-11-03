//! Minimal CoreFoundation helpers.
//!
//! Raw `core-foundation-sys` bindings instead of the high-level `core-foundation` crate on purpose:
//! its `CFArray`/`CFDictionary` generics changed shape between 0.9 and 0.10, and this module needs
//! six functions, not a type system. Every wrapper here is just "own it, release it on drop".

use core_foundation_sys::{
    array::{CFArrayGetCount, CFArrayGetTypeID, CFArrayGetValueAtIndex, CFArrayRef},
    base::{CFGetTypeID, CFRelease, CFRetain, CFTypeRef},
    number::{
        CFBooleanGetTypeID, CFBooleanGetValue, CFNumberGetTypeID, CFNumberGetValue,
        kCFNumberSInt64Type,
    },
    string::{
        CFStringCreateWithCString, CFStringGetCString, CFStringGetTypeID, CFStringRef,
        kCFStringEncodingUTF8,
    },
};
use std::ffi::{CStr, CString, c_char, c_void};

/// An owned reference-counted CoreFoundation value. Releases on drop.
pub(crate) struct CfOwned(CFTypeRef);

impl CfOwned {
    /// Take ownership of a `+1` reference returned by a CoreFoundation function.
    ///
    /// # Safety
    /// `raw` must be a non-null `+1` reference the caller owns.
    pub(crate) const unsafe fn from_owned(raw: CFTypeRef) -> Self {
        Self(raw)
    }

    /// Borrow the raw reference.
    pub(crate) const fn as_ref(&self) -> CFTypeRef {
        self.0
    }
}

impl Drop for CfOwned {
    fn drop(&mut self) {
        // SAFETY: we own exactly one reference (see `from_owned` / `retain`) and never hand it out
        // beyond the lifetime of `self`.
        unsafe { CFRelease(self.0) };
    }
}

/// Add a `+1` reference to a borrowed value.
///
/// # Safety
/// `raw` must be a valid CoreFoundation value that outlives this call.
pub(crate) unsafe fn retain(raw: CFTypeRef) -> CfOwned {
    // SAFETY: the caller guarantees `raw` is a valid CoreFoundation value.
    let owned = unsafe { CFRetain(raw) };
    // SAFETY: `CFRetain` returns the same value with a `+1` reference, which we now own.
    unsafe { CfOwned::from_owned(owned) }
}

/// A CoreFoundation string built from Rust text.
pub(crate) struct CfStr(CFStringRef);

impl CfStr {
    pub(crate) fn new(text: &str) -> Option<Self> {
        let c_string = CString::new(text).ok()?;
        // SAFETY: `c_string` is NUL-terminated and outlives the call; UTF-8 is a valid encoding.
        let raw = unsafe {
            CFStringCreateWithCString(std::ptr::null(), c_string.as_ptr(), kCFStringEncodingUTF8)
        };
        if raw.is_null() { None } else { Some(Self(raw)) }
    }

    pub(crate) const fn as_ref(&self) -> CFStringRef {
        self.0
    }
}

impl Drop for CfStr {
    fn drop(&mut self) {
        // SAFETY: created with `CFStringCreateWithCString` (a `+1` reference) and released once.
        unsafe { CFRelease(self.0.cast()) };
    }
}

/// `true` when `value` is a CoreFoundation string.
pub(crate) fn is_string(value: CFTypeRef) -> bool {
    // SAFETY: `CFGetTypeID` / `CFStringGetTypeID` accept any valid CF value.
    unsafe { !value.is_null() && CFGetTypeID(value) == CFStringGetTypeID() }
}

/// `true` when `value` is a CoreFoundation array.
pub(crate) fn is_array(value: CFTypeRef) -> bool {
    // SAFETY: see `is_string`.
    unsafe { !value.is_null() && CFGetTypeID(value) == CFArrayGetTypeID() }
}

/// `true` when `value` is a CoreFoundation boolean.
pub(crate) fn is_boolean(value: CFTypeRef) -> bool {
    // SAFETY: see `is_string`.
    unsafe { !value.is_null() && CFGetTypeID(value) == CFBooleanGetTypeID() }
}

/// `true` when `value` is a CoreFoundation number.
pub(crate) fn is_number(value: CFTypeRef) -> bool {
    // SAFETY: see `is_string`.
    unsafe { !value.is_null() && CFGetTypeID(value) == CFNumberGetTypeID() }
}

/// Copy a CF string into Rust text, or `None` when it is not a string / does not fit the buffer.
pub(crate) fn to_string(value: CFTypeRef) -> Option<String> {
    if !is_string(value) {
        return None;
    }
    let mut buffer = vec![c_char::default(); 8192];
    // SAFETY: `value` is a CF string (checked above) and `buffer` is a valid, sized destination.
    let ok = unsafe {
        CFStringGetCString(
            value.cast(),
            buffer.as_mut_ptr(),
            buffer.len() as isize,
            kCFStringEncodingUTF8,
        )
    };
    if ok == 0 {
        return None;
    }
    // SAFETY: on success `CFStringGetCString` wrote a NUL-terminated string into the buffer.
    let text = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    Some(text.to_string_lossy().into_owned())
}

/// Read a CF number as an integer.
pub(crate) fn to_i64(value: CFTypeRef) -> Option<i64> {
    if !is_number(value) {
        return None;
    }
    let mut out: i64 = 0;
    // SAFETY: `value` is a CF number and `out` is a valid `i64` destination.
    let ok = unsafe {
        CFNumberGetValue(
            value.cast(),
            kCFNumberSInt64Type,
            (&raw mut out).cast::<c_void>(),
        )
    };
    ok.then_some(out)
}

/// Read a CF boolean.
pub(crate) fn to_bool(value: CFTypeRef) -> Option<bool> {
    if !is_boolean(value) {
        return None;
    }
    // SAFETY: `value` is a CF boolean.
    Some(unsafe { CFBooleanGetValue(value.cast()) })
}

/// Borrow every element of a CF array. The borrows live as long as `value`.
pub(crate) fn array_items(value: CFTypeRef) -> Vec<CFTypeRef> {
    if !is_array(value) {
        return Vec::new();
    }
    let array: CFArrayRef = value.cast();
    // SAFETY: `value` is a CF array (checked above).
    let count = unsafe { CFArrayGetCount(array) };
    (0..count.max(0))
        .filter_map(|index| {
            // SAFETY: `index` is within `0..CFArrayGetCount`.
            let item = unsafe { CFArrayGetValueAtIndex(array, index) };
            (!item.is_null()).then_some(item)
        })
        .collect()
}
