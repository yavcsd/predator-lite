//! Turbo-button experiment: arm the button the way `PSAdminAgent` does ("Enable Turbo Key" =
//! SetGamingLED(0x10001)), listen on every candidate WMI event class and the keyboard for 30 s
//! while the user presses Turbo, then restore the LED state read at the start.
//! Run elevated: `turbo_button_events.exe` <report-file>
//!
//! Written while looking for a Turbo-button event; the app ended up polling the firmware turbo
//! flag instead (`Controller::poll_turbo_button`).

mod common;

use common::{capture_for_30s, log, save_log};
use predator_lite::controller::Backend;
use predator_lite::protocol::{decode_turbo_led, Method, TURBO_LED_ID, TURBO_LED_OFF, TURBO_LED_ON};
use predator_lite::wmi::{init_security, EventStream, Wmi};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// Turbo LED state.
fn led(hw: &Wmi) -> Option<bool> {
    hw.call(Method::GetGamingLed, TURBO_LED_ID).ok().and_then(|o| decode_turbo_led(o).ok())
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "turbo_button_events.txt".into());
    log("start");
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    init_security();
    let hw = match Wmi::connect() {
        Ok(h) => h,
        Err(e) => {
            let _ = std::fs::write(&out, format!("connect failed: {e}\n"));
            return;
        }
    };

    for class in ["APGeEvent", "WisButtonEvent", "SecEvent", "EsifWmiEvent", "POWER_CHANGE_EVENT"] {
        std::thread::spawn(move || unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            match EventStream::open_query(&format!("SELECT * FROM {class}")) {
                Ok(es) => {
                    log(&format!("WMI: subscribed to {class}"));
                    while let Ok((c, d)) = es.next_any("EventDetail") {
                        log(&format!("WMI {c} EventDetail={d:02X?}"));
                    }
                }
                Err(e) => log(&format!("WMI {class}: {e}")),
            }
        });
    }

    let start_led = led(&hw);
    log(&format!("turbo LED / key state at start: {start_led:?}"));
    let r = hw.call(Method::SetGamingLed, TURBO_LED_ON);
    log(&format!("arm: SetGamingLED(0x10001) -> {:?}; now {:?}", r.map(|v| format!("{v:#x}")), led(&hw)));

    capture_for_30s("capture started: press Turbo now");
    log(&format!("before restore: LED state {:?}", led(&hw)));
    let restore = if start_led == Some(true) { TURBO_LED_ON } else { TURBO_LED_OFF };
    let r = hw.call(Method::SetGamingLed, restore);
    log(&format!("restore: SetGamingLED({restore:#x}) -> {:?}; now {:?}", r.map(|v| format!("{v:#x}")), led(&hw)));
    save_log(&out);
}
