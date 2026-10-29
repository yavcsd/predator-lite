//! Shared by the capture tools: a timestamped log, a keyboard hook that logs only non-typing
//! keys, and the 30 s capture loop.

use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetTimer, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION,
    KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYUP, WM_SYSKEYUP, WM_TIMER,
};

/// How long a capture runs.
const CAPTURE_MS: u32 = 30_000;

/// Log lines from every thread, written to the report at the end.
static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
static START: OnceLock<Instant> = OnceLock::new();

/// Add a line, prefixed with the seconds since the first line.
pub fn log(s: &str) {
    let t = START.get_or_init(Instant::now).elapsed().as_secs_f32();
    LOG.lock().unwrap().push(format!("{t:7.2}s  {s}"));
}

/// Write every logged line to `path`.
pub fn save_log(path: &str) {
    let _ = std::fs::write(path, LOG.lock().unwrap().join("\n") + "\n");
}

/// Virtual keys that type text (space, digits, letters, numpad, punctuation): never logged.
fn is_typing_key(vk: u32) -> bool {
    matches!(vk, 0x20 | 0x30..=0x39 | 0x41..=0x5A | 0x60..=0x6F | 0xBA..=0xC0 | 0xDB..=0xDF | 0xE2)
}

/// Low-level keyboard hook: logs non-typing keys.
unsafe extern "system" fn keyboard_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION.cast_signed() {
        let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        if !is_typing_key(k.vkCode) {
            let up = matches!(u32::try_from(wp.0), Ok(WM_KEYUP | WM_SYSKEYUP));
            let dir = if up { "up  " } else { "down" };
            log(&format!("KEY {dir} vk={:#04x} scan={:#04x} flags={:#x}", k.vkCode, k.scanCode, k.flags.0));
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// Install the keyboard hook, pump messages (for the hook and any window of the caller) for
/// 30 s, then remove the hook.
pub fn capture_for_30s(prompt: &str) {
    unsafe {
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), None, 0);
        log(&format!("keyboard hook: {}", if hook.is_ok() { "installed" } else { "FAILED" }));
        log(prompt);
        SetTimer(None, 0, CAPTURE_MS, None);
        let mut msg = MSG::default();
        while GetMessageW(&raw mut msg, None, 0, 0).as_bool() {
            // The thread timer (no window) ends the capture.
            if msg.message == WM_TIMER && msg.hwnd.is_invalid() {
                break;
            }
            DispatchMessageW(&raw const msg);
        }
        if let Ok(h) = hook {
            let _ = UnhookWindowsHookEx(h);
        }
    }
    log("capture finished");
}
