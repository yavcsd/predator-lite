//! `--status`: read-only hardware report (capabilities, sensors, decoded state and the raw
//! words behind it), for bug reports and checking a new model. Only Get* methods are called.

use crate::controller::{Backend, Capabilities, Controller};
use crate::protocol::{
    decode_fan_mode, fan_speed_id, format_sensor, kb_timeout_label, running_gpu_mode, usb_charge_label, GpuMode,
    Method, MiscSetting, COOLBOOST_GET, FAN_SLOTS, KB_TIMEOUT_GET, KB_ZONES, PROFILE_GET, USB_CHARGE_GET,
};
use crate::settings::Settings;
use crate::system::{display_adapter_ids, pending_gpu_mode};
use std::fmt::Write;

/// Raw output of a call as hex, or the error.
fn raw_hex<B: Backend>(hw: &B, m: Method, input: u64) -> crate::controller::Result<String> {
    hw.call(m, input).map(|v| format!("{v:#x}"))
}

/// Build the report. Takes the backend by value and hands it back, so tests can inspect the
/// calls that were made.
pub fn report<B: Backend>(hw: B) -> (String, B) {
    let t0 = std::time::Instant::now();
    let caps = Capabilities::detect(&hw);
    let detect_ms = t0.elapsed().as_millis();
    let ctl = Controller::new(hw, caps, Settings::default());
    let caps = &ctl.caps;
    let hw = &ctl.hw;
    let mut out = String::new();
    let _ = writeln!(out, "predator-lite {} status (read-only)", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(out, "capability detection: {detect_ms} ms");
    let _ = writeln!(
        out,
        "fan slots: {:#07b}  zones: {:#06b}  turbo LED: {}  turbo flag: {}  GPU OC: {}  LCD overdrive: {}",
        caps.fan_slots, caps.zones, caps.turbo_led, caps.turbo_flag, caps.gpu_oc, caps.lcd_overdrive
    );

    let t1 = std::time::Instant::now();
    let readings = ctl.read_sensors();
    let _ = writeln!(out, "sensors ({} ms for {}):", t1.elapsed().as_millis(), readings.len());
    for (sensor, value) in &readings {
        let _ = writeln!(out, "  {}", format_sensor(*sensor, *value, ctl.read_fan_duty(*sensor)));
    }

    let state = ctl.read_state();
    let _ = writeln!(out, "fan mode: CPU {}  GPU {}", state.fan_mode[0].as_str(), state.fan_mode[1].as_str());
    let slots: Vec<u32> = (0..FAN_SLOTS).filter(|&slot| caps.has_slot(slot)).collect();
    for &slot in &slots {
        let r = hw.call(Method::GetGamingFanBehavior, 1 << slot);
        let _ = writeln!(out, "  slot {slot}: {:?}", r.map(|v| decode_fan_mode(slot, v)));
    }
    for &slot in &slots {
        let id = fan_speed_id(slot);
        let _ = writeln!(out, "  fan speed id {id}: {:?}", raw_hex(hw, Method::GetGamingFanSpeed, id));
    }
    let _ = writeln!(out, "turbo: {}  LCD overdrive: {}", state.turbo, state.lcd_overdrive);
    let _ = writeln!(
        out,
        "GPU mode: running {:?}, chosen since Windows started {:?}, firmware value {:?}",
        running_gpu_mode(&display_adapter_ids()),
        pending_gpu_mode().and_then(GpuMode::from_u8),
        ctl.gpu_mode_setting()
    );
    let _ = writeln!(
        out,
        "CoolBoost: supported {}, on {}  (raw {:?})",
        caps.coolboost,
        state.coolboost,
        raw_hex(hw, Method::ApgeGetFunction, COOLBOOST_GET)
    );
    let _ = writeln!(
        out,
        "Win key: supported {}, on {}  (profile {:?})",
        caps.win_key,
        state.win_key,
        raw_hex(hw, Method::GetGamingProfile, PROFILE_GET)
    );
    let _ = writeln!(out, "boot animation: supported {}, on {}", caps.boot_animation, state.boot_animation);
    let _ = writeln!(
        out,
        "power-off USB charging: supported {}, {}  (raw {:?})",
        caps.usb_charge,
        usb_charge_label(state.usb_charge),
        raw_hex(hw, Method::ApgeGetFunction, USB_CHARGE_GET)
    );
    let _ = writeln!(
        out,
        "keyboard backlight timeout: supported {}, {}  (raw {:?})",
        caps.kb_timeout,
        kb_timeout_label(state.kb_timeout),
        raw_hex(hw, Method::ApgeGetFunction, KB_TIMEOUT_GET)
    );
    for idx in [MiscSetting::Turbo, MiscSetting::GpuMode, MiscSetting::GpuOc, MiscSetting::BootAnimation] {
        let raw = raw_hex(hw, Method::GetGamingMiscSetting, idx as u64);
        let _ = writeln!(out, "  misc {:?} ({}): {:?}", idx, idx as u8, raw);
    }
    for z in 0..KB_ZONES {
        if caps.has_zone(z) {
            let _ = writeln!(out, "zone {}: {}", z + 1, state.zones[z].to_hex());
        }
    }
    for input in [0u64, 1] {
        let r = hw.get_bytes(Method::GetGamingKbBacklight, input);
        let hex = r.map(|bytes| bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "));
        let _ = writeln!(out, "kb backlight({input}): {hex:?}");
    }
    (out, ctl.hw)
}

#[cfg(test)]
mod tests {
    use crate::controller::mock::Mock;

    #[test]
    fn status_is_read_only() {
        let (text, hw) = super::report(Mock::pt515());
        assert!(text.contains("CPU: 77 °C"));
        assert!(text.contains("CPU fan: 2960 RPM, 38%"));
        assert!(text.contains("power-off USB charging: supported true, Until 30%"));
        assert!(text.contains("zone 1: #00AEC7"));
        assert!(!text.contains("zone 4"));
        assert!(text.contains("fan mode: CPU auto  GPU auto"));
        let calls = hw.take();
        assert_ne!(calls, []);
        assert!(calls.iter().all(|(m, _)| m.name().starts_with("Get")), "{calls:?}");
        assert!(hw.byte_calls.borrow().is_empty());
    }

    use crate::protocol::Method;

    /// Report of `hw`, checked to have every section and to have made Get* calls only.
    fn checked_report(hw: Mock) -> (String, Mock) {
        let (text, hw) = super::report(hw);
        for section in [
            "predator-lite ",
            "capability detection: ",
            "fan slots: ",
            "sensors (",
            "fan mode: CPU ",
            "turbo: ",
            "GPU mode: running ",
            "CoolBoost: supported ",
            "Win key: supported ",
            "boot animation: supported ",
            "power-off USB charging: supported ",
            "keyboard backlight timeout: supported ",
            "misc Turbo (1): ",
            "misc GpuMode (2): ",
            "misc GpuOc (5): ",
            "misc BootAnimation (6): ",
            "kb backlight(0): ",
            "kb backlight(1): ",
        ] {
            assert!(text.contains(section), "missing {section:?} in\n{text}");
        }
        let calls = hw.take();
        assert!(calls.iter().all(|(m, _)| m.name().starts_with("Get")), "{calls:?}");
        assert!(hw.byte_calls.borrow().is_empty());
        (text, hw)
    }

    #[test]
    fn status_with_wmi_down_renders_every_section() {
        let hw = Mock::default();
        *hw.wmi_down.borrow_mut() = true;
        let (text, _) = checked_report(hw);
        assert!(text.contains("fan slots: 0b00000  zones: 0b0000"), "{text}");
        assert!(text.contains(" ms for 0):"), "{text}");
        assert!(text.contains(", firmware value None"), "{text}");
        assert!(text.contains("CoolBoost: supported false, on false  (raw Err(Wmi(\"0x80041003\")))"), "{text}");
        assert!(text.contains("misc Turbo (1): Err("));
        assert!(text.contains("kb backlight(0): Err("));
        assert!(!text.contains("zone 1"));
        assert!(!text.contains("  slot "));
    }

    #[test]
    fn status_with_blank_firmware_shows_no_fans_and_dashes() {
        let (text, _) = checked_report(Mock::default());
        assert!(text.contains("fan slots: 0b00000"), "{text}");
        assert!(text.contains("  CPU: --"), "{text}");
        assert!(text.contains("  CPU fan: 0 RPM, 0%"), "{text}");
        assert!(text.contains(", firmware value None"), "{text}");
        assert!(text.contains("zone 4: #000000"));
        assert!(!text.contains("  slot "));
        assert!(!text.contains("fan speed id"));
    }

    #[test]
    fn status_without_sensors_still_reports_the_rest() {
        let hw = Mock::pt515();
        *hw.fail.borrow_mut() = Some((Method::GetGamingSysInfo, 1));
        let (text, _) = checked_report(hw);
        assert!(text.contains(" ms for 0):"), "{text}");
        assert!(!text.contains("CPU: "));
        assert!(!text.contains("RPM"));
        assert!(text.contains("zone 1: #00AEC7"));
        assert!(text.contains("  slot 0: Ok(Ok(Some(Auto)))"), "{text}");
        assert!(text.contains("fan speed id 1: "));
    }

    #[test]
    fn status_with_failing_apge_reports_those_features_absent() {
        let hw = Mock::pt515();
        *hw.fail.borrow_mut() = Some((Method::ApgeGetFunction, 0xE2));
        let (text, _) = checked_report(hw);
        assert!(text.contains("CoolBoost: supported false, on false  (raw Ok(\"0xe2\"))"), "{text}");
        assert!(text.contains("power-off USB charging: supported false"));
        assert!(text.contains("keyboard backlight timeout: supported false"));
        assert!(text.contains("Win key: supported true"));
        assert!(text.contains("CPU: 77 °C"));
    }

    #[test]
    fn status_skips_missing_zones_and_dashes_bad_readings() {
        let hw = Mock::pt515();
        hw.replies.borrow_mut().extend([
            ((Method::GetGamingRgbKb, 0x2), 0x1),
            ((Method::GetGamingSysInfo, 0x101), 0xFF00),
            ((Method::GetGamingSysInfo, 0x601), 0xFF_FF00),
        ]);
        let (text, _) = checked_report(hw);
        assert!(text.contains("zones: 0b0101"), "{text}");
        assert!(text.contains("zone 1: #00AEC7") && text.contains("zone 3: #00AEC7"));
        assert!(!text.contains("zone 2:"));
        assert!(text.contains("  CPU: --"), "{text}");
        assert!(text.contains("  GPU fan: --"), "{text}");
        assert!(text.contains("  GPU: 59 °C"));
    }
}
