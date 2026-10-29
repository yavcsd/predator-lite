//! Supervised test of what the unit tests can't reach: the live WMI connection and every read
//! the app makes, the `APGeEvent` subscription, Eco's power plan and the `nvidia-smi` clock cap.
//! Results are checked independently with `powercfg`, and the active power plan is put back at
//! the end (also after a failure). No firmware setting is written.
//!
//! Run elevated, with Predator Lite closed: `system_integration_test.exe <report-file>`.
//! Not part of the shipped program.

use predator_lite::controller::{Backend, Capabilities, Controller, EcoLimiter};
use predator_lite::eco_limits::WindowsEcoLimiter;
use predator_lite::protocol::{format_sensor, Method, Sensor, PROFILE_GET};
use predator_lite::settings::Settings;
use predator_lite::status_report;
use predator_lite::wmi::{init_security, EventStream, Wmi};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

const ECO_PLAN: &str = "Predator Lite Eco";
const OLD_ECO_PLAN: &str = "Predator Lite Quiet";
const WINDOWS_BALANCED: &str = "381b4222-f694-41f0-9685-ff5bb260df2e";
/// `powercfg` aliases: processor power management, processor performance boost mode.
const BOOST_SETTING: [&str; 2] = ["SUB_PROCESSOR", "PERFBOOSTMODE"];

/// Test report: log lines plus pass/fail counts.
#[derive(Default)]
struct Report {
    log: String,
    pass: u32,
    fail: u32,
}

impl Report {
    fn line(&mut self, s: &str) {
        self.log.push_str(s);
        self.log.push('\n');
    }

    fn check(&mut self, ok: bool, what: &str) {
        if ok {
            self.pass += 1;
        } else {
            self.fail += 1;
        }
        self.line(&format!("  [{}] {what}", if ok { "PASS" } else { "FAIL" }));
    }
}

/// Output of `powercfg` with these arguments.
fn powercfg(args: &[&str]) -> String {
    Command::new("powercfg")
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// First GUID (8-4-4-4-12 hex digits) in `text`, lowercase.
fn first_guid(text: &str) -> Option<String> {
    let is_guid = |w: &str| {
        let parts: Vec<&str> = w.split('-').collect();
        parts.iter().map(|p| p.len()).eq([8, 4, 4, 4, 12])
            && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_hexdigit()))
    };
    text.split(|c: char| !(c.is_ascii_hexdigit() || c == '-')).find(|w| is_guid(w)).map(str::to_lowercase)
}

/// GUID of the active plan.
fn active_plan() -> Option<String> {
    first_guid(&powercfg(&["/getactivescheme"]))
}

/// (GUID, name) of every power plan.
fn plans() -> Vec<(String, String)> {
    powercfg(&["/list"])
        .lines()
        .filter_map(|l| {
            let guid = first_guid(l)?;
            let name = l.split_once('(')?.1.rsplit_once(')')?.0.to_string();
            Some((guid, name))
        })
        .collect()
}

/// GUIDs of the plans called `name`.
fn plans_named(name: &str) -> Vec<String> {
    plans().into_iter().filter(|(_, n)| n == name).map(|(g, _)| g).collect()
}

/// (AC, DC) value of "processor performance boost mode" in `plan`: the last two hex values of
/// `powercfg /qh` (the setting is hidden, so `/query` leaves it out), the current AC and DC
/// indexes.
fn boost_mode(plan: &str) -> Option<(u32, u32)> {
    let out = powercfg(&["/qh", plan, BOOST_SETTING[0], BOOST_SETTING[1]]);
    let values: Vec<u32> = out
        .lines()
        .filter_map(|l| l.trim().rsplit_once(": 0x").and_then(|(_, hex)| u32::from_str_radix(hex.trim(), 16).ok()))
        .collect();
    match values[..] {
        [.., ac, dc] => Some((ac, dc)),
        _ => None,
    }
}

/// `%APPDATA%\PredatorLite\eco_prev_plan`.
fn prev_plan_file() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("PredatorLite").join("eco_prev_plan")
}

/// The GUID the prev-plan file holds (32 hex digits), formatted like `powercfg` prints it.
fn prev_plan_recorded() -> Option<String> {
    let hex = std::fs::read_to_string(prev_plan_file()).ok()?;
    let h = hex.trim().to_lowercase();
    (h.len() == 32).then(|| format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]))
}

/// Live WMI: connect, detect, read everything the app reads, subscribe to events.
fn wmi_checks(r: &mut Report) {
    r.line("--- WMI ---");
    let t0 = Instant::now();
    let hw = match Wmi::connect() {
        Ok(hw) => hw,
        Err(e) => {
            r.check(false, &format!("Wmi::connect: {e}"));
            return;
        }
    };
    r.check(true, &format!("Wmi::connect in {} ms", t0.elapsed().as_millis()));
    let caps = Capabilities::detect(&hw);
    r.check(caps.fan_slots != 0 && !caps.sensors.is_empty(), &format!("detected capabilities: {caps:?}"));
    let raw = hw.call(Method::GetGamingProfile, PROFILE_GET);
    r.check(raw.is_ok(), &format!("raw GetGamingProfile -> {raw:x?}"));

    let ctl = Controller::new(hw, caps.clone(), Settings::default());
    let readings = ctl.read_sensors();
    let all_read = readings.iter().all(|(_, v)| v.is_some());
    let text: Vec<String> = readings.iter().map(|&(s, v)| format_sensor(s, v, ctl.read_fan_duty(s))).collect();
    r.check(all_read, &format!("every detected sensor answers: {}", text.join(", ")));
    for s in [Sensor::CpuTemp, Sensor::GpuTemp, Sensor::SysTemp] {
        if let Some((_, Some(t))) = readings.iter().find(|(x, _)| *x == s) {
            r.check((20..=105).contains(t), &format!("{} temperature plausible: {t} °C", s.label()));
        }
    }
    let state = ctl.read_state();
    r.check(true, &format!("read_state: fans {:?}, GPU OC {:?}, turbo {}", state.fan_mode, state.gpu_oc, state.turbo));
    r.check(!state.restore_kb_lighting, "read_state leaves the keyboard lighting alone");
    r.line(&format!("  GPU mode for the next start: {:?}", ctl.gpu_mode_setting()));

    // Call latency, as the window's 2 s refresh sees it.
    let t0 = Instant::now();
    for _ in 0..20 {
        let _ = ctl.read_sensors();
    }
    let per_refresh = t0.elapsed().as_millis() / 20;
    r.check(per_refresh < 500, &format!("one sensor refresh takes {per_refresh} ms"));

    let (report, _) = status_report::report(ctl.hw);
    r.check(!report.starts_with("error") && report.contains("sensors"), "status report renders from live WMI");
    match EventStream::open() {
        Ok(_) => r.check(true, "APGeEvent subscription opens"),
        Err(e) => r.check(false, &format!("APGeEvent subscription: {e}")),
    }
}

/// Eco's power plan and GPU cap through `WindowsEcoLimiter`, checked with `powercfg`.
fn eco_checks(r: &mut Report, user_plan: &str) {
    r.line("--- Eco power plan ---");
    let mut eco = WindowsEcoLimiter;
    let user_boost = boost_mode(user_plan);
    r.line(&format!("  user plan {user_plan}, boost mode (AC, DC) {user_boost:?}"));

    let note = eco.set_eco(true, 1000);
    r.check(note.is_ok(), &format!("Eco on -> {note:?}"));
    let eco_plans = plans_named(ECO_PLAN);
    r.check(eco_plans.len() == 1, &format!("exactly one '{ECO_PLAN}' plan: {eco_plans:?}"));
    let Some(eco_plan) = eco_plans.first().cloned() else { return };
    r.check(active_plan().as_deref() == Some(eco_plan.as_str()), "the Eco plan is active");
    r.check(boost_mode(&eco_plan) == Some((0, 0)), &format!("Eco plan boost mode is off: {:?}", boost_mode(&eco_plan)));
    r.check(
        prev_plan_recorded().as_deref() == Some(user_plan),
        &format!("user plan recorded: {:?}", prev_plan_recorded()),
    );
    r.check(note.as_ref().is_ok_and(Option::is_none), "nvidia-smi accepted the 300-1000 MHz clock lock");

    // Eco on again (as after a crash with Eco active): the recorded plan must stay the user's.
    let again = eco.set_eco(true, 1000);
    r.check(again.is_ok(), &format!("Eco on while already on -> {again:?}"));
    r.check(prev_plan_recorded().as_deref() == Some(user_plan), "recorded plan is still the user's, not Eco's");

    let off = eco.set_eco(false, 1000);
    r.check(off.as_ref().is_ok_and(Option::is_none), &format!("Eco off -> {off:?} (clock lock removed)"));
    r.check(active_plan().as_deref() == Some(user_plan), &format!("user plan active again: {:?}", active_plan()));
    r.check(!prev_plan_file().exists(), "recorded-plan file removed");
    r.check(boost_mode(user_plan) == user_boost, "user plan's boost mode unchanged");

    let off_again = eco.set_eco(false, 1000);
    r.check(off_again.as_ref().is_ok_and(Option::is_none), &format!("Eco off while off is a no-op: {off_again:?}"));
    r.check(active_plan().as_deref() == Some(user_plan), "still the user plan");

    r.line("--- Eco without a GPU cap ---");
    let on = eco.set_eco(true, 0);
    r.check(on.as_ref().is_ok_and(Option::is_none), &format!("Eco on, no cap -> {on:?}"));
    let off = eco.set_eco(false, 0);
    r.check(off.as_ref().is_ok_and(Option::is_none), &format!("Eco off, no cap -> {off:?}"));
    r.check(active_plan().as_deref() == Some(user_plan), "user plan active again");

    r.line("--- Plan left by an older version ---");
    let _ = powercfg(&["/changename", &eco_plan, OLD_ECO_PLAN]);
    r.check(plans_named(OLD_ECO_PLAN) == [eco_plan.clone()], "Eco plan renamed to the old name for the test");
    let on = eco.set_eco(true, 0);
    r.check(on.is_ok(), &format!("Eco on -> {on:?}"));
    r.check(
        plans_named(ECO_PLAN) == [eco_plan.clone()] && plans_named(OLD_ECO_PLAN).is_empty(),
        "renamed back, not duplicated",
    );
    r.check(active_plan().as_deref() == Some(eco_plan.as_str()), "that plan is active");
    let _ = eco.set_eco(false, 0);

    r.line("--- Recorded plan lost while Eco is on ---");
    let _ = eco.set_eco(true, 0);
    let _ = std::fs::remove_file(prev_plan_file());
    let off = eco.set_eco(false, 0);
    r.check(off.is_ok(), &format!("Eco off without a recorded plan -> {off:?}"));
    r.check(active_plan().as_deref() == Some(WINDOWS_BALANCED), "falls back to Windows' Balanced plan");
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "system_integration_test.txt".into());
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    init_security();
    let mut r = Report::default();
    let user_plan = active_plan();
    r.line(&format!("active plan at start: {user_plan:?}"));
    wmi_checks(&mut r);
    match &user_plan {
        Some(plan) if plans_named(ECO_PLAN).first() != Some(plan) => eco_checks(&mut r, plan),
        _ => r.check(false, "Eco checks skipped: no plan, or the Eco plan itself is active"),
    }
    // Put the user's plan back whatever happened, and make sure no clock lock is left.
    if let Some(plan) = &user_plan {
        let _ = powercfg(&["/setactive", plan]);
    }
    let _ = std::fs::remove_file(prev_plan_file());
    let _ = Command::new(std::env::var("SystemRoot").unwrap_or_default() + "\\System32\\nvidia-smi.exe")
        .arg("-rgc")
        .output();
    r.check(active_plan() == user_plan, &format!("end: user plan active ({:?})", active_plan()));
    let _ = writeln!(r.log, "RESULT: {} passed, {} failed", r.pass, r.fail);
    let _ = std::fs::write(&out, &r.log);
    std::process::exit(i32::from(r.fail != 0));
}
