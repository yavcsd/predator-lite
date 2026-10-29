//! Read-only: poll Turbo-related firmware state every 200 ms for 40 s and log every change,
//! to see what the Turbo button does by itself. Run elevated: `watch_turbo_state.exe` <report-file>

use predator_lite::controller::Backend;
use predator_lite::protocol::{decode_sensor, format_sensor, Method, Sensor};
use predator_lite::wmi::{init_security, Wmi};
use std::time::{Duration, Instant};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// Raw Turbo LED, turbo flag (misc 1), GPU level (misc 5) and fan modes of slots 0, 3, 4.
fn snapshot(hw: &Wmi) -> String {
    let raw = |m, i| hw.call(m, i).map_or_else(|e| e.to_string(), |v| format!("{v:#x}"));
    format!(
        "LED1={} misc1={} misc5={} fan0={} fan3={} fan4={}",
        raw(Method::GetGamingLed, 1),
        raw(Method::GetGamingMiscSetting, 1),
        raw(Method::GetGamingMiscSetting, 5),
        raw(Method::GetGamingFanBehavior, 0x1),
        raw(Method::GetGamingFanBehavior, 0x8),
        raw(Method::GetGamingFanBehavior, 0x10),
    )
}

/// Speeds of the three fans.
fn rpm(hw: &Wmi) -> String {
    [Sensor::CpuFan, Sensor::GpuFan, Sensor::Fan3]
        .iter()
        .map(|s| {
            format_sensor(
                *s,
                hw.call(Method::GetGamingSysInfo, s.input()).ok().and_then(|o| decode_sensor(o).ok()),
                None,
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "watch_turbo_state.txt".into());
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    init_security();
    let mut log = Vec::new();
    let hw = match Wmi::connect() {
        Ok(h) => h,
        Err(e) => {
            let _ = std::fs::write(&out, format!("connect failed: {e}\n"));
            return;
        }
    };
    let t0 = Instant::now();
    let mut last = String::new();
    let mut next_rpm = Duration::ZERO;
    // Total time spent in snapshot(), to report the cost of one poll.
    let mut cost = Duration::ZERO;
    let mut polls = 0u32;
    while t0.elapsed() < Duration::from_secs(40) {
        let started = Instant::now();
        let snap = snapshot(&hw);
        cost += started.elapsed();
        polls += 1;
        if snap != last {
            log.push(format!("{:6.2}s  {snap}", t0.elapsed().as_secs_f32()));
            last = snap;
        }
        if t0.elapsed() >= next_rpm {
            log.push(format!("{:6.2}s    {}", t0.elapsed().as_secs_f32(), rpm(&hw)));
            next_rpm += Duration::from_secs(4);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    log.push(format!("avg snapshot cost: {:.2} ms for 6 calls", cost.as_secs_f64() * 1000.0 / f64::from(polls)));
    let _ = std::fs::write(&out, log.join("\n") + "\n");
}
