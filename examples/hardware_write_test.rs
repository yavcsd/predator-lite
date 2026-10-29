//! Supervised hardware write test. Drives the real `Controller` over WMI, verifies every write
//! with Get* read-backs, and restores the starting state at the end (also after a failure).
//!
//! Run elevated, with Predator Lite closed: `hardware_write_test.exe <report-file>`.
//! It switches Turbo on and off, runs the fans at Max for a few seconds and briefly turns
//! keyboard zone 1 red. Not part of the shipped program.

use predator_lite::controller::{part, Backend, Capabilities, Controller};
use predator_lite::protocol::{
    decode_fan_mode, decode_misc, decode_rgb, decode_turbo_led, format_sensor, FanGroup, FanMode, GpuOc, Method,
    MiscSetting, Rgb, Sensor, FAN_SLOTS, TURBO_LED_ID,
};
use predator_lite::settings::Settings;
use predator_lite::system::power_status;
use predator_lite::wmi::{init_security, Wmi};
use std::fmt::Write as _;
use std::thread::sleep;
use std::time::{Duration, Instant};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// Test report: log lines plus pass/fail counts.
struct Report {
    log: String,
    pass: u32,
    fail: u32,
}

impl Report {
    fn line(&mut self, s: impl AsRef<str>) {
        self.log.push_str(s.as_ref());
        self.log.push('\n');
    }

    fn check(&mut self, ok: bool, what: impl AsRef<str>) {
        if ok {
            self.pass += 1;
        } else {
            self.fail += 1;
        }
        self.line(format!("  [{}] {}", if ok { "PASS" } else { "FAIL" }, what.as_ref()));
    }
}

fn turbo_led(hw: &Wmi) -> Option<bool> {
    hw.call(Method::GetGamingLed, TURBO_LED_ID).ok().and_then(|o| decode_turbo_led(o).ok())
}

/// Raw read-back of a misc setting, for the log.
fn misc_raw(hw: &Wmi, idx: MiscSetting) -> String {
    match hw.call(Method::GetGamingMiscSetting, idx as u64) {
        Ok(v) => format!("{v:#x}"),
        Err(e) => e.to_string(),
    }
}

fn misc(hw: &Wmi, idx: MiscSetting) -> Option<u8> {
    hw.call(Method::GetGamingMiscSetting, idx as u64).ok().and_then(|o| decode_misc(o).ok())
}

/// Fan mode read back for every fan-behavior slot this machine has.
fn slot_modes(hw: &Wmi, caps: &Capabilities) -> Vec<(u32, Option<FanMode>)> {
    (0..FAN_SLOTS)
        .filter(|&slot| caps.has_slot(slot))
        .map(|slot| {
            let mode = hw.call(Method::GetGamingFanBehavior, 1 << slot).ok();
            (slot, mode.and_then(|o| decode_fan_mode(slot, o).ok().flatten()))
        })
        .collect()
}

/// Color of keyboard zone `zone` (0-based) as the firmware reports it.
fn zone_color(hw: &Wmi, zone: usize) -> Option<Rgb> {
    hw.call(Method::GetGamingRgbKb, 1 << zone).ok().and_then(|o| decode_rgb(o).ok())
}

/// Fan RPMs and the CPU temperature, for the log.
fn readings(ctl: &Controller<Wmi>) -> String {
    ctl.read_sensors()
        .into_iter()
        .filter(|(s, _)| !s.is_temp() || *s == Sensor::CpuTemp)
        .map(|(s, v)| format_sensor(s, v, None))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Check that every fan slot reads back `want`.
fn check_all_slots(r: &mut Report, hw: &Wmi, caps: &Capabilities, want: FanMode, label: &str) {
    let modes = slot_modes(hw, caps);
    r.check(modes.iter().all(|(_, m)| *m == Some(want)), format!("{label}: fan slots read back {modes:?}"));
}

/// Snapshot, run the steps, then restore and verify the starting state.
fn run(r: &mut Report) -> Result<(), String> {
    let hw = Wmi::connect().map_err(|e| e.to_string())?;
    let caps = Capabilities::detect(&hw);
    r.line(format!("capabilities: fan slots {:#07b}, zones {:#06b}", caps.fan_slots, caps.zones));
    let mut ctl = Controller::new(hw, caps.clone(), Settings::default());
    let (ac, pct) = power_status();
    ctl.power.update(ac, pct);
    r.line(format!("power: AC={ac} battery={pct:?} -> overclock allowed={}", ctl.power.allowed()));

    // Snapshot of the starting state (read-only).
    let start = ctl.read_state();
    let start_led = turbo_led(&ctl.hw);
    let start_turbo_flag = misc(&ctl.hw, MiscSetting::Turbo);
    let start_zone1 = zone_color(&ctl.hw, 0);
    r.line(format!(
        "start: fans {:?}, turbo LED {start_led:?}, turbo flag {start_turbo_flag:?}, GPU OC raw {}, zone 1 {:?}",
        start.fan_mode,
        misc_raw(&ctl.hw, MiscSetting::GpuOc),
        start_zone1.map(Rgb::to_hex)
    ));
    r.line(format!("start readings: {}", readings(&ctl)));
    ctl.settings = start.clone();

    steps(r, &mut ctl);

    // Restore: Turbo flag and fans as at the start, GPU overclock Normal, zone 1 color.
    r.line("--- restore ---");
    ctl.settings = Settings { gpu_oc: GpuOc::Normal, ..start.clone() };
    if let Err(e) = ctl.apply(part::TURBO | part::FANS | part::GPU) {
        r.check(false, format!("restore apply: {e}"));
    }
    if let Some(c) = start_zone1 {
        ctl.settings.zones[0] = Rgb::new(c.r ^ 1, c.g, c.b); // differs, so the color is sent again
        let res = ctl.set_zone_color(0, c);
        r.check(res.is_ok(), format!("restore zone 1 -> {} ({res:?})", c.to_hex()));
    }
    sleep(Duration::from_millis(500));
    r.check(turbo_led(&ctl.hw) == start_led, format!("restored turbo LED = {start_led:?}"));
    r.check(
        misc(&ctl.hw, MiscSetting::Turbo) == start_turbo_flag,
        format!("restored turbo flag = {start_turbo_flag:?}"),
    );
    let modes = slot_modes(&ctl.hw, &caps);
    // Slot 0 is the CPU fan, every other slot a GPU fan.
    let group_of = |slot: u32| if slot == 0 { FanGroup::Cpu } else { FanGroup::Gpu };
    r.check(
        caps.has_group(FanGroup::Cpu)
            && modes.iter().all(|&(slot, m)| m == Some(start.fan_mode[group_of(slot).index()])),
        format!("restored fan slots {modes:?}"),
    );
    r.check(
        zone_color(&ctl.hw, 0) == start_zone1,
        format!("restored zone 1 = {:?}", zone_color(&ctl.hw, 0).map(Rgb::to_hex)),
    );
    r.line(format!("end: GPU OC raw {}", misc_raw(&ctl.hw, MiscSetting::GpuOc)));
    Ok(())
}

/// The writes under test, each followed by read-back checks.
fn steps(r: &mut Report, ctl: &mut Controller<Wmi>) {
    let caps = ctl.caps.clone();

    r.line("--- 1. Turbo on ---");
    if ctl.power.allowed() {
        let t0 = Instant::now();
        let res = ctl.set_turbo(true);
        r.check(res.is_ok(), format!("set_turbo(true) -> {res:?} in {} ms", t0.elapsed().as_millis()));
        sleep(Duration::from_millis(500));
        r.check(turbo_led(&ctl.hw) == Some(true), "turbo LED reads ON");
        r.check(
            misc(&ctl.hw, MiscSetting::Turbo) == Some(1),
            format!("turbo flag = {:?}", misc(&ctl.hw, MiscSetting::Turbo)),
        );
        check_all_slots(r, &ctl.hw, &caps, FanMode::Max, "turbo");
        r.line(format!("  GPU OC raw while Turbo: {}", misc_raw(&ctl.hw, MiscSetting::GpuOc)));
        sleep(Duration::from_secs(5));
        r.line(format!("  readings after 5 s: {}", readings(ctl)));

        r.line("--- 2. Turbo off ---");
        let t0 = Instant::now();
        let res = ctl.set_turbo(false);
        r.check(res.is_ok(), format!("set_turbo(false) -> {res:?} in {} ms", t0.elapsed().as_millis()));
        sleep(Duration::from_millis(500));
        r.check(turbo_led(&ctl.hw) == Some(false), "turbo LED reads OFF");
        r.check(
            misc(&ctl.hw, MiscSetting::Turbo) == Some(0),
            format!("turbo flag = {:?}", misc(&ctl.hw, MiscSetting::Turbo)),
        );
        let want = ctl.settings.fan_mode[0];
        check_all_slots(r, &ctl.hw, &caps, want, "turbo off restores fans");
        r.line(format!("  GPU OC raw with GPU Normal: {}", misc_raw(&ctl.hw, MiscSetting::GpuOc)));
    } else {
        r.line("  skipped: on battery (Turbo needs AC), as designed");
    }

    r.line("--- 3. Fans Max ---");
    let res =
        ctl.set_fan_mode(FanGroup::Cpu, FanMode::Max).and_then(|()| ctl.set_fan_mode(FanGroup::Gpu, FanMode::Max));
    r.check(res.is_ok(), format!("set_fan_mode(Max) x2 -> {res:?}"));
    sleep(Duration::from_millis(500));
    check_all_slots(r, &ctl.hw, &caps, FanMode::Max, "fans max");
    sleep(Duration::from_secs(5));
    r.line(format!("  readings after 5 s: {}", readings(ctl)));

    r.line("--- 4. Fans Auto ---");
    let res =
        ctl.set_fan_mode(FanGroup::Cpu, FanMode::Auto).and_then(|()| ctl.set_fan_mode(FanGroup::Gpu, FanMode::Auto));
    r.check(res.is_ok(), format!("set_fan_mode(Auto) x2 -> {res:?}"));
    sleep(Duration::from_millis(500));
    check_all_slots(r, &ctl.hw, &caps, FanMode::Auto, "fans auto");
    sleep(Duration::from_secs(5));
    r.line(format!("  readings after 5 s: {}", readings(ctl)));

    r.line("--- 5. Zone 1 color ---");
    if !caps.has_zone(0) {
        r.line("  skipped: no zone 1");
        return;
    }
    let red = Rgb::new(0xFF, 0x00, 0x00);
    let res = ctl.set_zone_color(0, red);
    r.check(res.is_ok(), format!("set_zone_color(1, #FF0000) -> {res:?}"));
    sleep(Duration::from_millis(300));
    r.check(zone_color(&ctl.hw, 0) == Some(red), format!("zone 1 reads {:?}", zone_color(&ctl.hw, 0).map(Rgb::to_hex)));
    r.check(
        zone_color(&ctl.hw, 1) == Some(ctl.settings.zones[1]),
        format!("zone 2 untouched: {:?}", zone_color(&ctl.hw, 1).map(Rgb::to_hex)),
    );
    sleep(Duration::from_secs(2));
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "hardware_write_test.txt".into());
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    init_security();
    let mut report = Report { log: String::new(), pass: 0, fail: 0 };
    if let Err(e) = run(&mut report) {
        report.check(false, format!("aborted: {e}"));
    }
    let _ = writeln!(report.log, "RESULT: {} passed, {} failed", report.pass, report.fail);
    let _ = std::fs::write(&out, &report.log);
    std::process::exit(i32::from(report.fail != 0));
}
