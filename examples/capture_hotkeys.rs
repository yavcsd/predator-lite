//! Read-only capture of hardware-button events for 30 s: every `APGeEvent` (WMI) plus
//! non-typing keyboard keys (letters, digits and punctuation are never logged).
//!
//! Run elevated: `capture_hotkeys.exe` <report-file>

mod common;

use common::{capture_for_30s, log, save_log};
use predator_lite::wmi::{init_security, EventStream};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "capture_hotkeys.txt".into());
    log("start");

    std::thread::spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        init_security();
        match EventStream::open() {
            Ok(es) => {
                log("WMI: subscribed to APGeEvent");
                loop {
                    match es.next() {
                        Ok(d) => log(&format!("WMI APGeEvent EventDetail = {d:02X?}")),
                        Err(e) => {
                            log(&format!("WMI error: {e}"));
                            break;
                        }
                    }
                }
            }
            Err(e) => log(&format!("WMI subscribe failed: {e}")),
        }
    });

    capture_for_30s("capture started: press the buttons now");
    save_log(&out);
}
