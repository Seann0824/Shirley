//! Synthetic keyboard / text input through CoreGraphics event taps.
//!
//! Used only where the accessibility API has no direct answer: a text field that refuses
//! `AXValue`, or a shortcut the agent must press (`cmd+s`). Events are posted to the HID tap, so
//! they land in whatever app currently has keyboard focus -- callers activate the target app first.

use core_foundation_sys::base::CFRelease;
use std::ffi::c_void;
use std::thread::sleep;
use std::time::Duration;

type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        virtual_key: u16,
        key_down: bool,
    ) -> CGEventRef;
    fn CGEventKeyboardSetUnicodeString(event: CGEventRef, length: usize, text: *const u16);
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventPost(tap: u32, event: CGEventRef);
}

/// `kCGHIDEventTap`: events go through the normal input path, exactly like a real keyboard.
const HID_EVENT_TAP: u32 = 0;

const FLAG_SHIFT: u64 = 0x0002_0000;
const FLAG_CONTROL: u64 = 0x0004_0000;
const FLAG_OPTION: u64 = 0x0008_0000;
const FLAG_COMMAND: u64 = 0x0010_0000;

/// Modifier set parsed from a chord like `cmd+shift+s`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Modifiers {
    flags: u64,
}

impl Modifiers {
    pub(crate) fn add(&mut self, name: &str) -> bool {
        let flag = match name {
            "cmd" | "command" | "meta" | "super" => FLAG_COMMAND,
            "shift" => FLAG_SHIFT,
            "ctrl" | "control" => FLAG_CONTROL,
            "alt" | "opt" | "option" => FLAG_OPTION,
            _ => return false,
        };
        self.flags |= flag;
        true
    }

    pub(crate) const fn is_empty(self) -> bool {
        self.flags == 0
    }
}

/// Post one key press (down + up) with the given modifiers.
pub(crate) fn press_key(key_code: u16, modifiers: Modifiers) {
    for key_down in [true, false] {
        // SAFETY: a null event source means "use the default"; the returned event is owned by us.
        let event = unsafe { CGEventCreateKeyboardEvent(std::ptr::null_mut(), key_code, key_down) };
        if event.is_null() {
            return;
        }
        if modifiers.flags != 0 {
            // SAFETY: `event` is a live keyboard event we own.
            unsafe { CGEventSetFlags(event, modifiers.flags) };
        }
        // SAFETY: `event` is a live keyboard event; posting consumes nothing (we still own it).
        unsafe {
            CGEventPost(HID_EVENT_TAP, event);
            CFRelease(event.cast::<std::ffi::c_void>());
        }
        sleep(Duration::from_millis(8));
    }
}

/// Type literal text, including characters a keyboard layout cannot produce (CJK, emoji).
///
/// This bypasses the input method: it delivers Unicode scalars straight to the focused text view,
/// which is what an agent wants. The keyboard *layout* is irrelevant.
pub(crate) fn type_text(text: &str) {
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.is_empty() {
        return;
    }
    // Chunk long strings: `CGEventKeyboardSetUnicodeString` has an undocumented practical limit
    // (20 is the documented-safe maximum for some input paths), so send in modest slices.
    for chunk in units.chunks(16) {
        // SAFETY: null source is fine; the returned event is owned by us.
        let event = unsafe { CGEventCreateKeyboardEvent(std::ptr::null_mut(), 0, true) };
        if event.is_null() {
            return;
        }
        // SAFETY: `event` is a live keyboard event, `chunk` is a valid UTF-16 slice.
        unsafe {
            CGEventKeyboardSetUnicodeString(event, chunk.len(), chunk.as_ptr());
            CGEventPost(HID_EVENT_TAP, event);
            CFRelease(event.cast::<std::ffi::c_void>());
        }
        sleep(Duration::from_millis(4));
    }
}

/// Key codes for the keys an agent actually presses. US layout: the code identifies the physical
/// key, so letters are positional and shortcuts (`cmd+s`) behave as the user expects.
pub(crate) fn key_code(name: &str) -> Option<u16> {
    let code = match name {
        "return" | "enter" => 36,
        "tab" => 48,
        "space" => 49,
        "delete" | "backspace" => 51,
        "forward_delete" => 117,
        "escape" | "esc" => 53,
        "left" | "arrow_left" => 123,
        "right" | "arrow_right" => 124,
        "down" | "arrow_down" => 125,
        "up" | "arrow_up" => 126,
        "home" => 115,
        "end" => 119,
        "page_up" | "pageup" => 116,
        "page_down" | "pagedown" => 121,
        "f1" => 122,
        "f2" => 120,
        "f3" => 99,
        "f4" => 118,
        "f5" => 96,
        "f6" => 97,
        "f7" => 98,
        "f8" => 100,
        "f9" => 101,
        "f10" => 109,
        "f11" => 103,
        "f12" => 111,
        "a" => 0,
        "s" => 1,
        "d" => 2,
        "f" => 3,
        "h" => 4,
        "g" => 5,
        "z" => 6,
        "x" => 7,
        "c" => 8,
        "v" => 9,
        "b" => 11,
        "q" => 12,
        "w" => 13,
        "e" => 14,
        "r" => 15,
        "y" => 16,
        "t" => 17,
        "1" => 18,
        "2" => 19,
        "3" => 20,
        "4" => 21,
        "6" => 22,
        "5" => 23,
        "9" => 25,
        "7" => 26,
        "8" => 28,
        "0" => 29,
        "o" => 31,
        "u" => 32,
        "i" => 34,
        "p" => 35,
        "l" => 37,
        "j" => 38,
        "k" => 40,
        "n" => 45,
        "m" => 46,
        "-" => 27,
        "=" => 24,
        "[" => 33,
        "]" => 30,
        ";" => 41,
        "'" => 39,
        "," => 43,
        "." => 47,
        "/" => 44,
        "\\" => 42,
        "`" => 50,
        _ => return None,
    };
    Some(code)
}
