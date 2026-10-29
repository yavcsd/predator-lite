//! Guided experiment: what makes the firmware re-engage Turbo after a Turbo-button press?
//! After each press it performs exactly one kind of write (or none) and watches the turbo flag.
//! Run elevated with the app closed: `turbo_rearm_probe.exe` <report-file>
//! See `Controller::set_misc` and `poll_turbo_button` for what the app does about it.

use predator_lite::controller::Backend;
use predator_lite::protocol::{decode_misc, fan_behavior_input, FanGroup, FanMode, Method, MiscSetting};
use predator_lite::wmi::{init_security, Wmi};
use std::io::Write as _;
use std::time::{Duration, Instant};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// Fan slots of the PT515-51 (CPU 0, GPU 3 and 4); not detected, this is a one-machine tool.
const SLOTS: u32 = 0b11001;

/// The firmware turbo flag (misc 1).
fn flag(hw: &Wmi) -> Option<u8> {
    hw.call(Method::GetGamingMiscSetting, MiscSetting::Turbo as u64).ok().and_then(|o| decode_misc(o).ok())
}

/// GPU level (misc 5), raw.
fn gpu(hw: &Wmi) -> String {
    hw.call(Method::GetGamingMiscSetting, MiscSetting::GpuOc as u64)
        .map_or_else(|e| e.to_string(), |v| format!("{v:#x}"))
}

/// One line with the LED, flag, GPU level and fan modes, raw.
fn state(hw: &Wmi) -> String {
    let raw = |m, i| hw.call(m, i).map_or_else(|e| e.to_string(), |v| format!("{v:#x}"));
    format!(
        "LED1={} flag={:?} GPU={} fans cpu={} gpu={}/{}",
        raw(Method::GetGamingLed, 1),
        flag(hw),
        gpu(hw),
        raw(Method::GetGamingFanBehavior, 0x1),
        raw(Method::GetGamingFanBehavior, 0x8),
        raw(Method::GetGamingFanBehavior, 0x10)
    )
}

/// Set every fan to `m`; returns a log line.
fn fans(hw: &Wmi, m: FanMode) -> String {
    let input = fan_behavior_input(&[(FanGroup::Cpu, m), (FanGroup::Gpu, m)], SLOTS);
    format!(
        "SetGamingFanBehavior({:?}) -> {:?}",
        m,
        hw.call(Method::SetGamingFanBehavior, input).map(|v| format!("{v:#x}"))
    )
}

/// The session log, echoed to the console and kept in the report file.
struct Run {
    log: Vec<String>,
    t0: Instant,
    out: String,
}

impl Run {
    /// Log a line with a timestamp.
    fn say(&mut self, s: &str) {
        let line = format!("{:6.2}s  {s}", self.t0.elapsed().as_secs_f32());
        println!("{line}");
        let _ = std::io::stdout().flush();
        self.log.push(line);
        // Saved after every line so a closed window still leaves a record.
        let _ = std::fs::write(&self.out, self.log.join("\n") + "\n");
    }

    /// Prompt for a press and wait until the flag changes. Returns the new flag.
    fn press(&mut self, hw: &Wmi, n: u32) -> Option<u8> {
        let before = flag(hw);
        self.say(&format!(">>> PRESS TURBO NOW (press {n}/6)   [flag={before:?}]"));
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut tick = Instant::now();
        while Instant::now() < deadline {
            if tick.elapsed() >= Duration::from_secs(5) {
                tick = Instant::now();
                let st = state(hw);
                self.say(&format!("    waiting... {st}"));
            }
            let f = flag(hw);
            if f != before {
                self.say(&format!("press seen: flag {before:?} -> {f:?}, GPU {}", gpu(hw)));
                std::thread::sleep(Duration::from_millis(500));
                return f;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.say("no press within 60 s");
        None
    }

    /// Watch the flag for 4 s and report whether it flipped away from `expect`.
    fn watch(&mut self, hw: &Wmi, expect: Option<u8>, what: &str) {
        let end = Instant::now() + Duration::from_secs(4);
        while Instant::now() < end {
            let f = flag(hw);
            if f != expect {
                self.say(&format!("RESULT [{what}]: flag flipped back to {f:?} (GPU {}) -> RE-TRIGGERS", gpu(hw)));
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.say(&format!("RESULT [{what}]: flag stayed {expect:?} for 4 s -> safe"));
    }
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "turbo_rearm_probe.txt".into());
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    init_security();
    let mut r = Run { log: Vec::new(), t0: Instant::now(), out: out.clone() };
    let hw = match Wmi::connect() {
        Ok(h) => h,
        Err(e) => {
            r.say(&format!("connect failed: {e}"));
            let _ = std::fs::write(&out, r.log.join("\n"));
            return;
        }
    };
    let st = state(&hw);
    r.say(&format!("start: {st}"));
    // Six presses; after each, either nothing or a fan write matching the new flag (what the app
    // does after a press). Pairs check whether that write, or the firmware alone, re-engages Turbo.
    let plan: [(&str, bool); 6] = [
        ("nothing (baseline)", false),
        ("fans to match: Max after an 'on' press", true),
        ("fans to match: Auto after an 'off' press", true),
        ("nothing (fans stay Auto)", false),
        ("fans to match: Max after an 'on' press", true),
        ("nothing (fans left at Max after 'off')", false),
    ];
    for (n, (what, write_fans)) in (1..).zip(plan) {
        let Some(f) = r.press(&hw, n) else { break };
        if write_fans {
            let m = if f == 1 { FanMode::Max } else { FanMode::Auto };
            let line = fans(&hw, m);
            r.say(&line);
        }
        r.watch(&hw, Some(f), what);
    }
    // Leave fans on Auto; the turbo flag stays wherever the firmware has it.
    let line = fans(&hw, FanMode::Auto);
    r.say(&format!("cleanup: {line}; final flag {:?}, GPU {}", flag(&hw), gpu(&hw)));
    let _ = std::fs::write(&out, r.log.join("\n") + "\n");
    r.say("done - this window closes in 5 s");
    std::thread::sleep(Duration::from_secs(5));
}
