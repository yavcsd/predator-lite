//! What the user chose, and the settings file that keeps it:
//! `%APPDATA%\PredatorLite\settings.ini`, plain `key=value` lines.
//!
//! Parsing is lenient: unknown keys and bad values fall back to defaults, and keys written by
//! older versions are still understood (Balanced and Eco used to be "silent" and "quiet", fan
//! percentages used to be firmware values rather than duty cycles). Only current keys are
//! written back.

use crate::fan_curve::FanCurve;
use crate::protocol::{
    fan_duty_of, Direction, FanMode, GpuOc, KbBacklight, KbEffect, Rgb, KB_MAX_BRIGHTNESS, KB_MAX_SPEED, KB_ZONES,
    USB_CHARGE_FLOORS,
};
use std::path::PathBuf;
use std::{fs, io};

/// The user's choices. What is actually sent to the firmware is derived from these by the
/// controller (Turbo forces the fans to Max, battery power forces the GPU to Normal, ...).
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::struct_excessive_bools, reason = "one independent on/off choice per feature")]
pub struct Settings {
    /// Fan mode per group, indexed by `FanGroup::index` (CPU, GPU). Key `cpu_fan` / `gpu_fan`.
    pub fan_mode: [FanMode; 2],
    /// Custom fan speed per group, real duty in percent. Key `cpu_pct` / `gpu_pct`.
    pub fan_pct: [u8; 2],
    /// GPU overclock level outside Turbo. Key `gpu_oc`.
    pub gpu_oc: GpuOc,
    /// Turbo mode: Turbo LED, firmware turbo flag, fans at Max, GPU at its Turbo level. Key `turbo`.
    pub turbo: bool,
    /// Panel overdrive (firmware-owned: read at start, see `controller::part::FIRMWARE_OWNED`).
    /// Key `overdrive`.
    pub lcd_overdrive: bool,
    /// Keyboard lighting effect, speed, brightness, direction and color. Keys `kb_*`.
    pub kb: KbBacklight,
    /// Static color of each keyboard zone. Keys `zone1`..`zone4`.
    pub zones: [Rgb; KB_ZONES],
    /// The user has set the keyboard lighting in the app, so `kb` and `zones` are restored at
    /// start and after sleep. Until then the lighting is left as the firmware has it: its effect
    /// can't be read, and restoring the Static default would replace e.g. an effect PredatorSense
    /// set. Key `kb_restore`.
    pub restore_kb_lighting: bool,
    /// Watch the physical Turbo button (the firmware handles it without telling Windows).
    /// Key `turbo_button`.
    pub turbo_button: bool,
    /// Eco mode: CPU Turbo Boost off, GPU capped and at Normal, fans on `eco_curve`. Key `eco`.
    pub eco: bool,
    /// GPU clock cap in Eco mode in MHz, 0 = no cap. Key `eco_gpu_mhz`.
    pub eco_gpu_mhz: u16,
    /// Eco fan curve (real duty). Key `eco_curve`.
    pub eco_curve: FanCurve,
    /// Balanced mode: full performance, fans on `balanced_curve` (off while cool). Key `balanced`.
    pub balanced: bool,
    /// Balanced fan curve (real duty). Key `balanced_curve`.
    pub balanced_curve: FanCurve,
    /// CoolBoost: Acer's stronger Auto fan curve. Key `coolboost`.
    pub coolboost: bool,
    /// Windows and Menu keys work (false = locked by the firmware, e.g. for gaming). Key `win_key`.
    pub win_key: bool,
    /// Boot animation and sound (firmware-owned). Key `boot_animation`.
    pub boot_animation: bool,
    /// Seconds of keyboard idle before the backlight goes off (0 = never). The window offers
    /// `KB_TIMEOUTS`; any value the firmware or settings file holds is kept. Key `kb_timeout`.
    pub kb_timeout: u8,
    /// Power-off USB charging: 0 = off, else the battery floor in percent (`USB_CHARGE_FLOORS`;
    /// firmware-owned). Key `usb_charge`.
    pub usb_charge: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            fan_mode: [FanMode::Auto; 2],
            fan_pct: [50; 2],
            gpu_oc: GpuOc::Normal,
            turbo: false,
            // Acer ships the panel with overdrive on.
            lcd_overdrive: true,
            kb: KbBacklight::default(),
            zones: [Rgb::ACER_CYAN; KB_ZONES],
            // Files from before `kb_restore` existed always restored the lighting, and still do;
            // `Controller::read_state` clears it for a first start.
            restore_kb_lighting: true,
            turbo_button: true,
            eco: false,
            eco_gpu_mhz: 1000,
            eco_curve: FanCurve::eco_default(),
            balanced: false,
            balanced_curve: FanCurve::balanced_default(),
            coolboost: false,
            win_key: true,
            boot_animation: true,
            kb_timeout: 120,
            usb_charge: 0,
        }
    }
}

/// `%APPDATA%\PredatorLite\settings.ini`; `None` if APPDATA is not set.
#[must_use]
pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("PredatorLite").join("settings.ini"))
}

/// The saved settings, or `None` when there is no settings file (first start).
#[must_use]
pub fn load() -> Option<Settings> {
    let text = fs::read_to_string(path()?).ok()?;
    Some(parse(&text))
}

/// Write via a temporary file so a crash never leaves a half-written config.
///
/// # Errors
/// When `%APPDATA%` is not set, or the folder or file can't be written.
pub fn save(s: &Settings) -> io::Result<()> {
    let p = path().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "APPDATA not set"))?;
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, serialize(s))?;
    fs::rename(&tmp, &p)
}

/// Eco GPU clock caps accepted from the file (MHz), besides 0 = no cap.
const ECO_GPU_MHZ: std::ops::RangeInclusive<u16> = 300..=3000;

/// A number in `0..=max`.
fn num(v: &str, max: u8) -> Option<u8> {
    v.parse::<u8>().ok().filter(|&n| n <= max)
}

/// `1`/`true` or `0`/`false`.
fn flag(v: &str) -> Option<bool> {
    match v {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

/// Settings from the file's text; never fails (see the module comment).
pub fn parse(text: &str) -> Settings {
    let mut s = Settings::default();
    // Older files stored fan percentages as firmware values, which are not duty cycles; they have
    // no `fan_units=duty` and are converted below so the fans sound the same.
    let mut duty_units = false;
    // Which fan percentages the file set (only those need converting).
    let mut has_fan_pct = [false; 2];
    let (mut has_balanced_curve, mut has_eco_curve) = (false, false);
    // Editors such as Notepad may save a UTF-8 byte-order mark, which `trim` does not remove.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(['#', ';', '[']) {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "fan_units" => duty_units = v == "duty",
            "cpu_fan" => s.fan_mode[0] = FanMode::parse(v).unwrap_or(s.fan_mode[0]),
            "gpu_fan" => s.fan_mode[1] = FanMode::parse(v).unwrap_or(s.fan_mode[1]),
            "cpu_pct" => {
                if let Some(p) = num(v, 100) {
                    s.fan_pct[0] = p;
                    has_fan_pct[0] = true;
                }
            }
            "gpu_pct" => {
                if let Some(p) = num(v, 100) {
                    s.fan_pct[1] = p;
                    has_fan_pct[1] = true;
                }
            }
            "coolboost" => s.coolboost = flag(v).unwrap_or(s.coolboost),
            "gpu_oc" => s.gpu_oc = num(v, 255).and_then(GpuOc::from_u8).unwrap_or(s.gpu_oc),
            "turbo" => s.turbo = flag(v).unwrap_or(s.turbo),
            "turbo_button" => s.turbo_button = flag(v).unwrap_or(s.turbo_button),
            // Balanced and Eco were called "silent" and "quiet" in older versions.
            "balanced" | "silent" => s.balanced = flag(v).unwrap_or(s.balanced),
            "eco" | "quiet" => s.eco = flag(v).unwrap_or(s.eco),
            // Invalid or unsafe curves (not reaching 100 % by 95 °C) keep the default.
            "balanced_curve" | "silent_curve" => {
                if let Some(c) = FanCurve::parse(v) {
                    s.balanced_curve = c;
                    has_balanced_curve = true;
                }
            }
            "eco_curve" | "quiet_curve" => {
                if let Some(c) = FanCurve::parse(v) {
                    s.eco_curve = c;
                    has_eco_curve = true;
                }
            }
            "overdrive" => s.lcd_overdrive = flag(v).unwrap_or(s.lcd_overdrive),
            // Older versions saved misc setting 6 as "lcd_overdrive", thinking it was LCD
            // overdrive; it is the boot animation, so keep what it did.
            "boot_animation" | "lcd_overdrive" => s.boot_animation = flag(v).unwrap_or(s.boot_animation),
            "usb_charge" => {
                s.usb_charge = num(v, 255).filter(|f| USB_CHARGE_FLOORS.contains(f)).unwrap_or(s.usb_charge);
            }
            "win_key" => s.win_key = flag(v).unwrap_or(s.win_key),
            "kb_timeout" => s.kb_timeout = num(v, 255).unwrap_or(s.kb_timeout),
            // 0 = no GPU cap; otherwise a sane clock range.
            "eco_gpu_mhz" | "quiet_gpu_mhz" => {
                s.eco_gpu_mhz =
                    v.parse::<u16>().ok().filter(|&m| m == 0 || ECO_GPU_MHZ.contains(&m)).unwrap_or(s.eco_gpu_mhz);
            }
            "kb_effect" => s.kb.effect = num(v, 255).and_then(KbEffect::from_u8).unwrap_or(s.kb.effect),
            "kb_speed" => s.kb.speed = num(v, KB_MAX_SPEED).unwrap_or(s.kb.speed),
            "kb_brightness" => s.kb.brightness = num(v, KB_MAX_BRIGHTNESS).unwrap_or(s.kb.brightness),
            "kb_direction" => {
                s.kb.direction = match v {
                    "1" => Direction::RightToLeft,
                    "2" => Direction::LeftToRight,
                    _ => s.kb.direction,
                }
            }
            "kb_color" => s.kb.color = Rgb::parse_hex(v).unwrap_or(s.kb.color),
            "kb_restore" => s.restore_kb_lighting = flag(v).unwrap_or(s.restore_kb_lighting),
            _ => {
                if let Some(i) = k.strip_prefix("zone").and_then(|n| n.parse::<usize>().ok()) {
                    if (1..=KB_ZONES).contains(&i) {
                        s.zones[i - 1] = Rgb::parse_hex(v).unwrap_or(s.zones[i - 1]);
                    }
                }
            }
        }
    }
    if !duty_units {
        // Each group converted with its own fans' response (slot 0 = CPU fan, 3 = GPU fan).
        if has_fan_pct[0] {
            s.fan_pct[0] = fan_duty_of(0, s.fan_pct[0]);
        }
        if has_fan_pct[1] {
            s.fan_pct[1] = fan_duty_of(3, s.fan_pct[1]);
        }
        // One curve drives both groups; converting with the CPU fan's response keeps the CPU fan
        // exactly as before (the GPU fans end up a few points faster).
        let to_duty = |p| fan_duty_of(0, p);
        if has_balanced_curve {
            s.balanced_curve = s.balanced_curve.map_pct(to_duty).unwrap_or_else(FanCurve::balanced_default);
        }
        if has_eco_curve {
            s.eco_curve = s.eco_curve.map_pct(to_duty).unwrap_or_else(FanCurve::eco_default);
        }
    }
    s
}

/// The settings-file text for `s` (current keys only, fan values in duty units).
#[must_use]
pub fn serialize(s: &Settings) -> String {
    let mut out = String::from("[predator-lite]\n");
    let mut kv = |k: &str, v: String| {
        out.push_str(k);
        out.push('=');
        out.push_str(&v);
        out.push('\n');
    };
    // Fans. Percentages (cpu_pct, gpu_pct, curves) are real duty cycles.
    kv("fan_units", "duty".into());
    kv("cpu_fan", s.fan_mode[0].as_str().into());
    kv("gpu_fan", s.fan_mode[1].as_str().into());
    kv("cpu_pct", s.fan_pct[0].to_string());
    kv("gpu_pct", s.fan_pct[1].to_string());
    kv("coolboost", u8::from(s.coolboost).to_string());
    // Performance modes.
    kv("gpu_oc", (s.gpu_oc as u8).to_string());
    kv("turbo", u8::from(s.turbo).to_string());
    kv("turbo_button", u8::from(s.turbo_button).to_string());
    kv("balanced", u8::from(s.balanced).to_string());
    kv("balanced_curve", s.balanced_curve.to_string());
    kv("eco", u8::from(s.eco).to_string());
    kv("eco_curve", s.eco_curve.to_string());
    kv("eco_gpu_mhz", s.eco_gpu_mhz.to_string());
    // Display and system.
    kv("overdrive", u8::from(s.lcd_overdrive).to_string());
    kv("boot_animation", u8::from(s.boot_animation).to_string());
    kv("usb_charge", s.usb_charge.to_string());
    // Keyboard.
    kv("win_key", u8::from(s.win_key).to_string());
    kv("kb_timeout", s.kb_timeout.to_string());
    kv("kb_restore", u8::from(s.restore_kb_lighting).to_string());
    kv("kb_effect", (s.kb.effect as u8).to_string());
    kv("kb_speed", s.kb.speed.to_string());
    kv("kb_brightness", s.kb.brightness.to_string());
    kv("kb_direction", (s.kb.direction as u8).to_string());
    kv("kb_color", s.kb.color.to_hex());
    for (i, z) in s.zones.iter().enumerate() {
        kv(&format!("zone{}", i + 1), z.to_hex());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Settings {
            fan_mode: [FanMode::Custom, FanMode::Max],
            fan_pct: [0, 100],
            gpu_oc: GpuOc::Faster,
            turbo: true,
            lcd_overdrive: false,
            kb: KbBacklight {
                effect: KbEffect::Zoom,
                speed: 9,
                brightness: 0,
                direction: Direction::LeftToRight,
                color: Rgb::new(1, 2, 3),
            },
            zones: [Rgb::new(255, 0, 0), Rgb::new(0, 255, 0), Rgb::new(0, 0, 255), Rgb::new(9, 9, 9)],
            restore_kb_lighting: false,
            turbo_button: false,
            eco: true,
            eco_gpu_mhz: 0,
            eco_curve: FanCurve::parse("70:30,90:100").unwrap(),
            balanced: true,
            balanced_curve: FanCurve::parse("55:20,95:100").unwrap(),
            coolboost: true,
            win_key: false,
            boot_animation: false,
            kb_timeout: 30,
            usb_charge: 20,
        };
        assert_eq!(parse(&serialize(&s)), s);
    }

    #[test]
    fn old_lcd_overdrive_key_means_boot_animation() {
        let d = Settings::default();
        let s = parse("lcd_overdrive=0\n");
        assert!(!s.boot_animation);
        // The real overdrive keeps the default (on) until the user changes it.
        assert_eq!(s.lcd_overdrive, d.lcd_overdrive);
        assert!(!serialize(&s).contains("lcd_overdrive"));
        assert!(!parse("overdrive=0\n").lcd_overdrive);
    }

    #[test]
    fn kb_restore_defaults_to_on_for_older_files() {
        assert!(parse("kb_effect=3\n").restore_kb_lighting);
        assert!(!parse("kb_restore=0\n").restore_kb_lighting);
        assert!(serialize(&Settings { restore_kb_lighting: false, ..Default::default() }).contains("kb_restore=0\n"));
    }

    #[test]
    fn kb_timeout_values() {
        assert_eq!(parse("kb_timeout=30").kb_timeout, 30);
        assert_eq!(parse("kb_timeout=0").kb_timeout, 0);
        assert_eq!(parse("kb_timeout=300").kb_timeout, Settings::default().kb_timeout);
    }

    #[test]
    fn usb_charge_values() {
        assert_eq!(parse("usb_charge=30").usb_charge, 30);
        assert_eq!(parse("usb_charge=15").usb_charge, Settings::default().usb_charge);
    }

    #[test]
    fn win_key_defaults_to_working_keys() {
        // Settings files from before the Win key setting must not lock the keys.
        assert!(parse("turbo=0\n").win_key);
        assert!(!parse("win_key=0\n").win_key);
        assert!(parse("win_key=maybe\n").win_key);
    }

    #[test]
    fn empty_and_garbage_give_defaults() {
        let d = Settings::default();
        assert_eq!(parse(""), d);
        assert_eq!(parse("\u{feff}\0\0garbage\n=\n==\nkey\n[x]\n"), d);
        assert_eq!(parse(&"x".repeat(100_000)), d);
    }

    #[test]
    fn out_of_range_values_are_ignored() {
        let s = parse(
            "cpu_pct=101\ngpu_pct=-1\ngpu_oc=3\nkb_effect=6\nkb_speed=10\nkb_brightness=255\n\
             kb_direction=0\nturbo=yes\ncpu_fan=AUTO\nzone0=#FF0000\nzone5=#FF0000\nzone1=#FF00\n\
             gpu_oc=99999999999\n",
        );
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn whitespace_comments_and_crlf() {
        let s = parse("; comment\r\n# other\r\nfan_units=duty\r\n  cpu_pct = 70 \r\nzone2=#ff0000\r\nturbo=true\r\n");
        assert_eq!(s.fan_pct[0], 70);
        assert_eq!(s.zones[1], Rgb::new(255, 0, 0));
        assert!(s.turbo);
    }

    #[test]
    fn eco_values_validated() {
        let s = parse("eco=1\neco_gpu_mhz=900\n");
        assert!(s.eco);
        assert_eq!(s.eco_gpu_mhz, 900);
        let d = Settings::default();
        let s = parse("eco_gpu_mhz=100\n");
        assert_eq!(s.eco_gpu_mhz, d.eco_gpu_mhz);
        // The old fixed Eco speed is ignored now (fans follow the curve).
        assert_eq!(parse("quiet_fan_pct=30\n"), d);
        let s = parse("eco_gpu_mhz=99999\n");
        assert_eq!(s.eco_gpu_mhz, d.eco_gpu_mhz);
        assert_eq!(parse("eco_gpu_mhz=0").eco_gpu_mhz, 0);
        // Older settings files have no Eco keys: Eco stays off.
        assert!(!parse("turbo=0\n").eco);
    }

    #[test]
    fn old_mode_names_still_load() {
        // Balanced and Eco were saved as "silent" and "quiet" by earlier versions.
        let s = parse(
            "fan_units=duty\nsilent=1\nsilent_curve=60:20,90:100\n\
             quiet=1\nquiet_curve=70:30,90:100\nquiet_gpu_mhz=800\n",
        );
        assert!(s.balanced && s.eco);
        assert_eq!(s.balanced_curve.to_string(), "60:20,90:100");
        assert_eq!(s.eco_curve.to_string(), "70:30,90:100");
        assert_eq!(s.eco_gpu_mhz, 800);
        // Written back under the new names only.
        let text = serialize(&s);
        assert!(text.contains("\nbalanced=1\n") && text.contains("\neco=1\n"));
        assert!(!text.contains("silent") && !text.contains("quiet"));
    }

    #[test]
    fn curves_and_balanced() {
        let d = Settings::default();
        let s = parse("fan_units=duty\nsilent=1\nsilent_curve=55:20,75:50,90:100\nquiet_curve=70:25,92:100\n");
        assert!(s.balanced);
        assert_eq!(s.balanced_curve.to_string(), "55:20,75:50,90:100");
        assert_eq!(s.eco_curve.to_string(), "70:25,92:100");
        // Unsafe curve (never full speed) or garbage keeps the default.
        let s = parse("balanced_curve=60:20,80:50\nquiet_curve=nonsense\n");
        assert_eq!((s.balanced_curve, s.eco_curve), (d.balanced_curve, d.eco_curve));
    }

    #[test]
    fn later_keys_win() {
        assert_eq!(parse("fan_units=duty\ncpu_pct=10\ncpu_pct=20").fan_pct[0], 20);
        assert_eq!(parse("fan_units=duty\ncpu_pct=10\ncpu_pct=bad").fan_pct[0], 10);
    }

    #[test]
    fn old_firmware_percentages_become_duty() {
        // The fan part of a settings file written before the calibration (this laptop's).
        let s = parse(
            "cpu_pct=0\ngpu_pct=80\nquiet_curve=65:20,75:35,85:60,90:100\n\
             silent=1\nsilent_curve=60:25,70:35,80:55,87:80,92:100\n",
        );
        assert_eq!(s.fan_pct, [0, 55]);
        assert_eq!(s.balanced_curve, FanCurve::balanced_default());
        assert_eq!(s.eco_curve, FanCurve::eco_default());
        // Saved again, it is marked and read back unchanged (no second conversion).
        let text = serialize(&s);
        assert!(text.contains("fan_units=duty"));
        assert_eq!(parse(&text), s);
        // Keys left out keep the defaults, which are already duty.
        assert_eq!(parse("cpu_fan=auto\n"), Settings::default());
    }

    /// Parse with the fan values already in duty units (no conversion).
    fn duty(text: &str) -> Settings {
        parse(&format!("fan_units=duty\n{text}"))
    }

    #[test]
    fn every_flag_accepts_only_its_four_spellings() {
        type Get = fn(&Settings) -> bool;
        let fields: [(&str, Get); 9] = [
            ("coolboost", |s| s.coolboost),
            ("turbo", |s| s.turbo),
            ("turbo_button", |s| s.turbo_button),
            ("balanced", |s| s.balanced),
            ("eco", |s| s.eco),
            ("overdrive", |s| s.lcd_overdrive),
            ("boot_animation", |s| s.boot_animation),
            ("win_key", |s| s.win_key),
            ("kb_restore", |s| s.restore_kb_lighting),
        ];
        let d = Settings::default();
        for (key, get) in fields {
            for (v, want) in [("1", true), ("true", true), ("0", false), ("false", false)] {
                assert_eq!(get(&parse(&format!("{key}={v}"))), want, "{key}={v}");
            }
            for v in ["", "2", "-1", "01", "TRUE", "True", "yes", "on", "1 1", "\u{661}"] {
                assert_eq!(parse(&format!("{key}={v}")), d, "{key}={v:?}");
            }
        }
    }

    #[test]
    fn fan_keys_at_their_limits() {
        let d = Settings::default();
        assert_eq!(duty("cpu_pct=0").fan_pct, [0, 50]);
        assert_eq!(duty("cpu_pct=100").fan_pct, [100, 50]);
        assert_eq!(duty("gpu_pct=0").fan_pct, [50, 0]);
        assert_eq!(duty("gpu_pct=100").fan_pct, [50, 100]);
        for bad in ["101", "256", "-1", "", "50%", "5 0", "1e2", "0x10", "fifty"] {
            assert_eq!(duty(&format!("cpu_pct={bad}\ngpu_pct={bad}")), d, "{bad:?}");
        }
        for (v, mode) in [("auto", FanMode::Auto), ("max", FanMode::Max), ("custom", FanMode::Custom)] {
            assert_eq!(parse(&format!("cpu_fan={v}")).fan_mode, [mode, FanMode::Auto]);
            assert_eq!(parse(&format!("gpu_fan={v}")).fan_mode, [FanMode::Auto, mode]);
        }
        for bad in ["Max", "", "manual", "1", "max max"] {
            assert_eq!(parse(&format!("cpu_fan={bad}\ngpu_fan={bad}")), d, "{bad:?}");
        }
    }

    #[test]
    fn performance_keys_at_their_limits() {
        let d = Settings::default();
        for (v, oc) in [("0", GpuOc::Normal), ("1", GpuOc::Faster), ("2", GpuOc::Turbo)] {
            assert_eq!(parse(&format!("gpu_oc=1\ngpu_oc={v}")).gpu_oc, oc);
        }
        for key in ["eco_gpu_mhz", "quiet_gpu_mhz"] {
            for (v, mhz) in [("0", 0), ("300", 300), ("3000", 3000)] {
                assert_eq!(parse(&format!("{key}={v}")).eco_gpu_mhz, mhz, "{key}={v}");
            }
            for bad in ["1", "299", "3001", "65535", "65536", "-1", "", "1000 MHz"] {
                assert_eq!(parse(&format!("{key}={bad}")), d, "{key}={bad:?}");
            }
        }
        for bad in ["3", "255", "256", "", "turbo"] {
            assert_eq!(parse(&format!("gpu_oc={bad}")), d, "{bad:?}");
        }
        for &f in &USB_CHARGE_FLOORS {
            assert_eq!(parse(&format!("usb_charge={f}")).usb_charge, f);
        }
        for bad in ["1", "9", "11", "31", "255", "256", ""] {
            assert_eq!(parse(&format!("usb_charge={bad}")), d, "{bad:?}");
        }
    }

    #[test]
    fn keyboard_keys_at_their_limits() {
        let d = Settings::default();
        for e in KbEffect::ALL {
            assert_eq!(parse(&format!("kb_effect={}", e as u8)).kb.effect, e);
        }
        assert_eq!(parse("kb_speed=0").kb.speed, 0);
        assert_eq!(parse(&format!("kb_speed={KB_MAX_SPEED}")).kb.speed, KB_MAX_SPEED);
        assert_eq!(parse("kb_brightness=0").kb.brightness, 0);
        assert_eq!(parse(&format!("kb_brightness={KB_MAX_BRIGHTNESS}")).kb.brightness, KB_MAX_BRIGHTNESS);
        assert_eq!(parse("kb_direction=2").kb.direction, Direction::LeftToRight);
        assert_eq!(parse("kb_direction=2\nkb_direction=1").kb.direction, Direction::RightToLeft);
        assert_eq!(parse("kb_timeout=255").kb_timeout, 255);
        assert_eq!(parse("kb_color=#abcdef").kb.color, Rgb::new(0xAB, 0xCD, 0xEF));
        assert_eq!(parse("kb_color=010203").kb.color, Rgb::new(1, 2, 3));
        for bad in [
            "kb_effect=6",
            "kb_effect=256",
            "kb_speed=10",
            "kb_brightness=101",
            "kb_direction=0",
            "kb_direction=3",
            "kb_direction=",
            "kb_timeout=256",
            "kb_timeout=",
            "kb_color=#12345",
            "kb_color=#1234567",
            "kb_color=##123456",
            "kb_color=+12345",
            "kb_color=#GGGGGG",
            "kb_color=",
        ] {
            assert_eq!(parse(bad), d, "{bad}");
        }
    }

    #[test]
    fn zone_indices() {
        let (red, cyan) = (Rgb::new(255, 0, 0), Rgb::ACER_CYAN);
        assert_eq!(parse("zone1=#FF0000").zones, [red, cyan, cyan, cyan]);
        assert_eq!(parse("zone4=#FF0000").zones, [cyan, cyan, cyan, red]);
        let d = Settings::default();
        for bad in ["zone0", "zone5", "zone", "zone-1", "zone18446744073709551617", "Zone1"] {
            assert_eq!(parse(&format!("{bad}=#FF0000")), d, "{bad}");
        }
        assert_eq!(parse("zone4=nope"), d);
    }

    #[test]
    fn kb_restore_values() {
        assert!(parse("kb_restore=1").restore_kb_lighting);
        assert!(parse("kb_restore=true").restore_kb_lighting);
        assert!(!parse("kb_restore=false").restore_kb_lighting);
        // A bad value keeps the default (restore), or the earlier valid one.
        assert!(parse("kb_restore=off").restore_kb_lighting);
        assert!(!parse("kb_restore=0\nkb_restore=off").restore_kb_lighting);
        assert!(serialize(&Settings::default()).contains("\nkb_restore=1\n"));
    }

    #[test]
    fn line_syntax() {
        assert!(parse("  turbo   =   1  ").turbo);
        assert!(parse("\tturbo\t=\t1\t").turbo);
        let d = Settings::default();
        for bad in [
            "TURBO=1",
            "Turbo=1",
            "turbo==1",
            "turbo=1=1",
            "turbo 1",
            "tur bo=1",
            "turbo=1 # on",
            "# turbo=1",
            "  ; turbo=1",
            "[turbo=1]",
            "=1",
            "kb_color=#FF0000=x",
            "balanced_curve=60:20,90:100=",
        ] {
            assert_eq!(parse(bad), d, "{bad}");
        }
    }

    #[test]
    fn line_endings() {
        let lines = ["fan_units=duty", "cpu_pct=70", "turbo=1", "zone2=#FF0000"];
        let s = parse(&lines.join("\n"));
        assert_eq!((s.fan_pct[0], s.turbo, s.zones[1]), (70, true, Rgb::new(255, 0, 0)));
        assert_eq!(parse(&lines.join("\r\n")), s);
        assert_eq!(parse(&(lines.join("\r\n") + "\r\n")), s);
        assert_eq!(parse(&(lines.join("\n") + "\n\n\n")), s);
        assert_eq!(parse(&lines.join("\n\r\n\n")), s);
        // A lone CR does not end a line (no Windows editor writes it): all of it is one bad value.
        assert_eq!(parse(&lines.join("\r")), Settings::default());
    }

    #[test]
    fn bom_before_the_header_is_harmless() {
        let s = parse("\u{feff}[predator-lite]\r\nturbo=1\r\n");
        assert!(s.turbo);
    }

    #[test]
    fn bom_before_a_first_line_key_is_ignored() {
        assert!(parse("\u{feff}turbo=1\n").turbo);
    }

    #[test]
    fn very_long_lines() {
        let long = "9".repeat(100_000);
        let curve = vec!["60:20"; 10_000].join(",");
        let s = parse(&format!(
            "fan_units=duty\ncpu_pct={long}\nkb_color=#{long}\n{long}=1\nbalanced_curve={curve}\nturbo=1\n"
        ));
        assert_eq!(s, Settings { turbo: true, ..Default::default() });
    }

    #[test]
    fn duplicates_last_valid_value_wins() {
        assert!(!parse("turbo=1\nturbo=0").turbo);
        assert!(parse("turbo=1\nturbo=maybe").turbo);
        assert_eq!(parse("zone1=#FF0000\nzone1=#00FF00").zones[0], Rgb::new(0, 255, 0));
        assert_eq!(duty("balanced_curve=60:20,90:100\nbalanced_curve=bad").balanced_curve.to_string(), "60:20,90:100");
        assert_eq!(parse("eco_gpu_mhz=800\neco_gpu_mhz=5").eco_gpu_mhz, 800);
    }

    #[test]
    fn legacy_and_current_key_last_one_wins() {
        assert!(!parse("balanced=1\nsilent=0").balanced);
        assert!(parse("silent=0\nbalanced=1").balanced);
        assert!(!parse("eco=1\nquiet=0").eco);
        assert!(parse("quiet=0\neco=1").eco);
        assert_eq!(parse("eco_gpu_mhz=800\nquiet_gpu_mhz=900").eco_gpu_mhz, 900);
        assert_eq!(parse("quiet_gpu_mhz=900\neco_gpu_mhz=800").eco_gpu_mhz, 800);
        assert_eq!(
            duty("balanced_curve=60:20,90:100\nsilent_curve=70:30,90:100").balanced_curve.to_string(),
            "70:30,90:100"
        );
        assert_eq!(duty("quiet_curve=70:30,90:100\neco_curve=60:20,90:100").eco_curve.to_string(), "60:20,90:100");
        let s = parse("boot_animation=1\nlcd_overdrive=0");
        assert!(!s.boot_animation && s.lcd_overdrive);
        // An invalid legacy value does not undo a valid current one.
        assert!(parse("balanced=1\nsilent=maybe").balanced);
        assert_eq!(duty("balanced_curve=60:20,90:100\nsilent_curve=bad").balanced_curve.to_string(), "60:20,90:100");
    }

    #[test]
    fn key_order_does_not_matter() {
        let s = Settings {
            fan_mode: [FanMode::Custom, FanMode::Custom],
            fan_pct: [30, 70],
            turbo: true,
            eco: true,
            kb_timeout: 60,
            zones: [Rgb::new(1, 1, 1), Rgb::new(2, 2, 2), Rgb::new(3, 3, 3), Rgb::new(4, 4, 4)],
            eco_curve: FanCurve::parse("50:10,80:100").unwrap(),
            ..Default::default()
        };
        let text = serialize(&s);
        // `fan_units=duty` now comes last.
        let reversed: Vec<&str> = text.lines().rev().collect();
        assert_eq!(parse(&reversed.join("\n")), s);
    }

    #[test]
    fn fan_pct_migration_edges() {
        // 0 and 100 % are the same in both units.
        assert_eq!(parse("cpu_pct=0\ngpu_pct=100").fan_pct, [0, 100]);
        assert_eq!(parse("cpu_pct=100\ngpu_pct=0").fan_pct, [100, 0]);
        // Each group converts with its own fans; a missing or invalid one keeps the (duty) default.
        assert_eq!(parse("cpu_pct=80").fan_pct, [62, 50]);
        assert_eq!(parse("gpu_pct=80").fan_pct, [50, 55]);
        assert_eq!(parse("cpu_pct=101\ngpu_pct=80").fan_pct, [50, 55]);
        // The marker counts wherever it is, but only spelled exactly `duty`; the last one wins.
        assert_eq!(parse("cpu_pct=80\ngpu_pct=80\nfan_units=duty").fan_pct, [80, 80]);
        assert_eq!(parse("fan_units = duty \ncpu_pct=80").fan_pct[0], 80);
        for other in ["firmware", "DUTY", "", "duty2"] {
            assert_eq!(parse(&format!("fan_units={other}\ncpu_pct=80")).fan_pct[0], 62, "{other:?}");
        }
        assert_eq!(parse("fan_units=duty\nfan_units=raw\ncpu_pct=80").fan_pct[0], 62);
        assert_eq!(parse("fan_units=raw\nfan_units=duty\ncpu_pct=80").fan_pct[0], 80);
    }

    #[test]
    fn curve_migration_edges() {
        let d = Settings::default();
        // A missing or invalid old curve keeps the default, which is already duty: not converted.
        let s = parse("silent_curve=60:20,80:50\nquiet_curve=bad\ncpu_pct=0");
        assert_eq!((&s.balanced_curve, &s.eco_curve), (&d.balanced_curve, &d.eco_curve));
        // Only the curve that was set is converted.
        let s = parse("balanced_curve=60:80,90:100");
        assert_eq!(s.balanced_curve.to_string(), "60:62,90:100");
        assert_eq!(s.eco_curve, d.eco_curve);
        // 0 and 100 % stay, small values may round to 0.
        assert_eq!(parse("eco_curve=40:0,95:100").eco_curve.to_string(), "40:0,95:100");
        assert_eq!(parse("eco_curve=40:1,41:2,95:100").eco_curve.to_string(), "40:0,41:1,95:100");
        assert_eq!(parse("eco_curve=40:100").eco_curve.to_string(), "40:100");
        assert_eq!(duty("eco_curve=40:1,41:2,95:100").eco_curve.to_string(), "40:1,41:2,95:100");
    }

    #[test]
    fn converted_old_curves_stay_valid() {
        // The conversion is monotonic and keeps 100 %, so the default fallback never triggers.
        for p in 0..=100 {
            for q in p..=100 {
                let c = FanCurve::parse(&format!("60:{p},70:{q},90:100")).unwrap();
                assert!(c.map_pct(|v| fan_duty_of(0, v)).is_some(), "{c}");
            }
        }
    }

    #[test]
    fn serialize_is_stable_and_writes_current_keys_only() {
        let legacy = ["silent", "quiet", "silent_curve", "quiet_curve", "quiet_gpu_mhz", "lcd_overdrive"];
        let inputs = [
            String::new(),
            "\u{feff}garbage\n=\n".into(),
            "cpu_pct=80\ngpu_pct=80\nsilent=1\nsilent_curve=60:25,92:100\nquiet=1\n\
             quiet_curve=65:20,90:100\nquiet_gpu_mhz=800\nlcd_overdrive=0\n"
                .into(),
            "fan_units=duty\ncpu_fan=custom\ngpu_fan=max\nkb_effect=5\nkb_direction=2\nzone4=#123456\n\
             kb_restore=0\nusb_charge=30\neco_gpu_mhz=0\n"
                .into(),
            serialize(&Settings::default()),
        ];
        for input in &inputs {
            let once = serialize(&parse(input));
            assert_eq!(serialize(&parse(&once)), once, "{input:?}");
            assert_eq!(parse(&once), parse(input), "{input:?}");
            let mut lines = once.lines();
            assert_eq!(lines.next(), Some("[predator-lite]"));
            let mut keys: Vec<&str> = lines.map(|l| l.split_once('=').unwrap().0).collect();
            assert!(keys.iter().all(|k| !legacy.contains(k)), "{once}");
            let n = keys.len();
            keys.sort_unstable();
            keys.dedup();
            assert_eq!(keys.len(), n, "duplicate keys in {once}");
        }
    }
}
