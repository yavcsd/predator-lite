//! Hardware controller: turns user settings into firmware calls (`protocol` encodings sent
//! through a `Backend`, which is WMI in the app and `mock::Mock` in tests).
//!
//! `Settings` holds what the user asked for; what is sent to the firmware is derived from it
//! (Turbo forces fans to Max and the GPU to its Turbo level, battery power forces the GPU back
//! to Normal, Balanced and Eco drive the fans from a software curve). Every change is applied
//! as a unit and rolled back if the firmware rejects it.
//!
//! The three performance modes exclude each other:
//! - Turbo: Turbo LED and firmware turbo flag on, fans Max, GPU at its Turbo level.
//! - Balanced: full performance, fans on `Settings::balanced_curve`.
//! - Eco: OS limits through an `EcoLimiter` (CPU boost off, GPU clock cap), GPU Normal, fans
//!   on `Settings::eco_curve`.
//!
//! With none of them on, the user's own fan modes and GPU level apply.
//!
//! Writes are skipped when the firmware already holds the value: some settings react to a
//! redundant write (see `Controller::set_misc`).

use crate::fan_curve::{FanCurve, FanTrack};
use crate::protocol::{
    byte, check, coolboost_set_input, decode_coolboost, decode_duty, decode_fan_mode, decode_kb_timeout,
    decode_lcd_overdrive, decode_misc, decode_rgb, decode_sensor, decode_turbo_led, decode_usb_charge, decode_win_key,
    fan_behavior_input, fan_speed_input, kb_backlight_payload, kb_timeout_set_input, lcd_overdrive_set_input,
    misc_input, rgb_zone_input, usb_charge_set_input, win_key_set_input, FanGroup, FanMode, FirmwareError, GpuMode,
    GpuOc, KbBacklight, KbEffect, Method, MiscSetting, Rgb, Sensor, COOLBOOST_GET, FAN_SLOTS, KB_MAX_BRIGHTNESS,
    KB_MAX_SPEED, KB_TIMEOUTS, KB_TIMEOUT_GET, KB_ZONES, PROFILE_GET, TURBO_LED_ID, TURBO_LED_OFF, TURBO_LED_ON,
    USB_CHARGE_FLOORS, USB_CHARGE_GET, VALID_TEMP_C,
};
use crate::settings::Settings;
use std::fmt;

/// Everything a controller operation can fail with.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// COM/WMI failure (message includes the HRESULT).
    Wmi(String),
    /// The firmware answered with a non-zero status.
    Hw(Method, FirmwareError),
    /// Value out of range.
    Range(&'static str),
    /// Overclocking needs AC power (and >= 40 % battery), like in PredatorSense.
    NeedsAc,
    /// The feature does not exist on this machine.
    NotPresent(&'static str),
    /// A Windows setting (power plan) could not be changed.
    Os(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Wmi(m) => write!(f, "WMI error: {m}"),
            Error::Hw(m, e) => write!(f, "{}: {e}", m.name()),
            Error::Range(what) => write!(f, "{what} is out of range"),
            Error::NeedsAc => f.write_str("Overclocking needs the AC adapter and at least 40% battery"),
            Error::NotPresent(what) => write!(f, "{what} is not available on this model"),
            Error::Os(m) => write!(f, "Windows: {m}"),
        }
    }
}

/// Result of a controller or firmware call.
pub type Result<T> = std::result::Result<T, Error>;

/// Raw access to the firmware methods.
pub trait Backend {
    /// Call a method with an integer input; returns the raw output (`gmOutput` / `uiOutput`).
    ///
    /// # Errors
    /// `Error::Wmi` when the call itself fails. The firmware's status byte is not checked here.
    fn call(&self, m: Method, input: u64) -> Result<u64>;
    /// Call a method whose input is `uint8[]` (`SetGamingKBBacklight`).
    ///
    /// # Errors
    /// `Error::Wmi` when the call itself fails.
    fn call_bytes(&self, m: Method, input: &[u8]) -> Result<u64>;
    /// Call a method whose output is `uint8[]` (`GetGamingKBBacklight`).
    ///
    /// # Errors
    /// `Error::Wmi` when the call fails or its output is not a byte array.
    fn get_bytes(&self, m: Method, input: u64) -> Result<Vec<u8>>;
}

/// Call a method and check its status byte; returns the payload.
fn exec<B: Backend + ?Sized>(b: &B, m: Method, input: u64) -> Result<u64> {
    query(b, m, input, check)
}

/// Call a method and decode its output with one of the `protocol::decode_*` functions.
fn query<B: Backend + ?Sized, T>(
    b: &B,
    m: Method,
    input: u64,
    decode: impl FnOnce(u64) -> std::result::Result<T, FirmwareError>,
) -> Result<T> {
    decode(b.call(m, input)?).map_err(|e| Error::Hw(m, e))
}

/// What this particular machine supports, detected with read-only calls.
#[derive(Clone, Debug, PartialEq, Default)]
#[allow(clippy::struct_excessive_bools, reason = "one independent flag per firmware feature")]
pub struct Capabilities {
    /// Bit n set = fan-behavior slot n has a fan.
    pub fan_slots: u32,
    /// Sensors that answer, in `Sensor::ALL` order.
    pub sensors: Vec<Sensor>,
    /// Fans whose duty cycle (%) can be read.
    pub fan_duty: Vec<Sensor>,
    /// Bit n set = keyboard zone n+1 exists.
    pub zones: u8,
    /// The Turbo LED can be read and set.
    pub turbo_led: bool,
    /// The firmware turbo flag (misc 1) exists; the Turbo button flips it.
    pub turbo_flag: bool,
    /// GPU overclock levels (misc 5) can be set.
    pub gpu_oc: bool,
    /// LCD overdrive can be switched.
    pub lcd_overdrive: bool,
    /// Lighting effects can be set (assumed whenever zone colors work).
    pub kb_effects: bool,
    /// Display MUX (Hybrid / Discrete only) can be switched.
    pub gpu_mode: bool,
    /// CoolBoost (stronger Auto fan curve) is available.
    pub coolboost: bool,
    /// The Windows and Menu keys can be locked and unlocked.
    pub win_key: bool,
    /// Boot animation and sound can be switched.
    pub boot_animation: bool,
    /// Keyboard backlight idle timeout can be set.
    pub kb_timeout: bool,
    /// Power-off USB charging can be set.
    pub usb_charge: bool,
}

impl Capabilities {
    /// Probe the machine with Get* calls only. A feature counts as present when its read
    /// succeeds and decodes; any failure (including WMI being down) means absent.
    pub fn detect<B: Backend + ?Sized>(b: &B) -> Capabilities {
        let mut c = Capabilities::default();
        for slot in 0..FAN_SLOTS {
            let mode = query(b, Method::GetGamingFanBehavior, 1 << slot, |o| decode_fan_mode(slot, o));
            if let Ok(Some(_)) = mode {
                c.fan_slots |= 1 << slot;
            }
        }
        for s in Sensor::ALL {
            if query(b, Method::GetGamingSysInfo, s.input(), decode_sensor).is_ok() {
                c.sensors.push(s);
                if let Some(input) = s.duty_input() {
                    if query(b, Method::GetGamingSysInfo, input, decode_duty).is_ok() {
                        c.fan_duty.push(s);
                    }
                }
            }
        }
        for z in 0..KB_ZONES {
            if exec(b, Method::GetGamingRgbKb, 1 << z).is_ok() {
                c.zones |= 1 << z;
            }
        }
        let ok = |m, input| exec(b, m, input).is_ok();
        c.turbo_led = ok(Method::GetGamingLed, TURBO_LED_ID);
        c.turbo_flag = ok(Method::GetGamingMiscSetting, MiscSetting::Turbo as u64);
        c.gpu_oc = ok(Method::GetGamingMiscSetting, MiscSetting::GpuOc as u64);
        c.lcd_overdrive = query(b, Method::GetGamingProfile, PROFILE_GET, decode_lcd_overdrive).is_ok();
        c.boot_animation = ok(Method::GetGamingMiscSetting, MiscSetting::BootAnimation as u64);
        c.kb_timeout = query(b, Method::ApgeGetFunction, KB_TIMEOUT_GET, decode_kb_timeout).is_ok();
        c.usb_charge = query(b, Method::ApgeGetFunction, USB_CHARGE_GET, decode_usb_charge).is_ok();
        // Same test as PredatorSense's Get_GPUMode_Support: misc 2 reads 1 or 2.
        c.gpu_mode = query(b, Method::GetGamingMiscSetting, MiscSetting::GpuMode as u64, decode_misc)
            .ok()
            .and_then(GpuMode::from_u8)
            .is_some();
        c.coolboost = query(b, Method::ApgeGetFunction, COOLBOOST_GET, decode_coolboost).is_ok();
        c.win_key = query(b, Method::GetGamingProfile, PROFILE_GET, decode_win_key).is_ok();
        // Lighting effects ride on the same firmware block as the zone colors.
        c.kb_effects = c.zones != 0;
        c
    }

    /// Fan-behavior slot `slot` has a fan.
    #[must_use]
    pub fn has_slot(&self, slot: u32) -> bool {
        self.fan_slots & (1 << slot) != 0
    }

    /// The slots of group `g` that have a fan.
    pub fn group_slots(&self, g: FanGroup) -> impl Iterator<Item = u32> + '_ {
        g.slots().iter().copied().filter(|&slot| self.has_slot(slot))
    }

    /// At least one fan of group `g` exists.
    #[must_use]
    pub fn has_group(&self, g: FanGroup) -> bool {
        self.group_slots(g).next().is_some()
    }

    /// Keyboard zone `idx` (0-based) exists.
    #[must_use]
    pub fn has_zone(&self, idx: usize) -> bool {
        idx < KB_ZONES && self.zones & (1 << idx) != 0
    }
}

/// OS-side limits used by Eco mode (CPU power plan, GPU clock cap). Returns an optional note
/// for the user, e.g. when one of the limits is not available on this machine.
pub trait EcoLimiter {
    /// Turn the limits on (GPU capped at `gpu_mhz`, 0 = no cap) or off. Turning them off when
    /// they are not on must be a cheap no-op: it runs at every start, for crash recovery.
    ///
    /// # Errors
    /// `Error::Os` when Windows refuses the change; the active power plan then stays as it was.
    fn set_eco(&mut self, on: bool, gpu_mhz: u16) -> Result<Option<String>>;
}

/// No OS limits (tests, `--status`).
pub struct NoEcoLimiter;

impl EcoLimiter for NoEcoLimiter {
    fn set_eco(&mut self, _on: bool, _gpu_mhz: u16) -> Result<Option<String>> {
        Ok(None)
    }
}

/// Result of checking for a press of the physical Turbo button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurboButton {
    /// Not pressed since the last poll.
    Unchanged,
    /// Pressed: Turbo is now on.
    On,
    /// Pressed: Turbo is now off.
    Off,
    /// Pressed while overclocking is not allowed (battery): switched back off.
    RefusedOnBattery,
}

/// PredatorSense's AC/battery rule: overclocking (Turbo, GPU Faster) stops on battery or below
/// 30 % and comes back on AC once the battery is at 40 % again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerGate {
    allowed: bool,
}

impl Default for PowerGate {
    fn default() -> Self {
        PowerGate { allowed: true }
    }
}

impl PowerGate {
    /// Battery percent below which overclocking stops (even on AC).
    pub const STOP_BELOW: u8 = 30;
    /// Battery percent at which overclocking comes back (on AC).
    pub const RESUME_AT: u8 = 40;

    /// Overclocking is currently allowed.
    #[must_use]
    pub fn allowed(self) -> bool {
        self.allowed
    }

    /// `battery` is `None` when unknown or there is no battery. Returns true if the state changed.
    pub fn update(&mut self, on_ac: bool, battery: Option<u8>) -> bool {
        let new = if self.allowed {
            on_ac && battery.is_none_or(|p| p >= Self::STOP_BELOW)
        } else {
            on_ac && battery.is_none_or(|p| p >= Self::RESUME_AT)
        };
        let changed = new != self.allowed;
        self.allowed = new;
        changed
    }
}

/// Parts of the hardware state that `apply` writes: bit flags, combined with `|`.
pub mod part {
    /// Turbo LED and firmware turbo flag.
    pub const TURBO: u16 = 1 << 0;
    /// Fan modes and Custom speeds.
    pub const FANS: u16 = 1 << 1;
    /// GPU overclock level.
    pub const GPU: u16 = 1 << 2;
    /// LCD overdrive.
    pub const LCD_OVERDRIVE: u16 = 1 << 3;
    /// Keyboard lighting effect and, for Static, the zone colors.
    pub const KB_LIGHTING: u16 = 1 << 4;
    /// CoolBoost.
    pub const COOLBOOST: u16 = 1 << 5;
    /// Win key lock.
    pub const WIN_KEY: u16 = 1 << 6;
    /// Boot animation and sound.
    pub const BOOT_ANIMATION: u16 = 1 << 7;
    /// Keyboard backlight idle timeout.
    pub const KB_TIMEOUT: u16 = 1 << 8;
    /// Power-off USB charging.
    pub const USB_CHARGE: u16 = 1 << 9;
    /// Every part.
    pub const ALL: u16 = TURBO
        | FANS
        | GPU
        | LCD_OVERDRIVE
        | KB_LIGHTING
        | COOLBOOST
        | WIN_KEY
        | BOOT_ANIMATION
        | KB_TIMEOUT
        | USB_CHARGE;
    /// Kept by the firmware itself (boot animation and LCD overdrive can also be changed in BIOS
    /// setup, USB charging in Acer Care Center), so the firmware is the truth: read at start and
    /// after sleep, written only when the user changes them in the app. PredatorSense does the
    /// same for boot animation and LCD overdrive.
    pub const FIRMWARE_OWNED: u16 = LCD_OVERDRIVE | BOOT_ANIMATION | USB_CHARGE;
    /// Restored from the settings file at start and after sleep.
    pub const RESTORE: u16 = ALL & !FIRMWARE_OWNED;
}

/// The user's settings plus everything needed to put them on the hardware.
pub struct Controller<B: Backend> {
    /// The firmware (WMI in the app, a mock in the tests).
    pub hw: B,
    /// What this laptop supports, detected at start.
    pub caps: Capabilities,
    /// What the user chose (saved to the settings file by the caller).
    pub settings: Settings,
    /// AC and battery state, for the overclocking rule.
    pub power: PowerGate,
    /// Eco's OS-side limits; `NoEcoLimiter` until the app installs the real one.
    pub limiter: Box<dyn EcoLimiter>,
    /// Balanced/Eco fan curve fell back to the firmware's Auto (temperatures unreadable).
    pub curve_fell_back_to_auto: bool,
    /// Curve state per fan group (smoothed temperature, current speed).
    pub tracks: [FanTrack; 2],
}

impl<B: Backend> Controller<B> {
    /// A controller that has written nothing yet (call `apply` to put `settings` on the hardware).
    pub fn new(hw: B, caps: Capabilities, settings: Settings) -> Self {
        Controller {
            hw,
            caps,
            settings,
            power: PowerGate::default(),
            limiter: Box::new(NoEcoLimiter),
            curve_fell_back_to_auto: false,
            tracks: Default::default(),
        }
    }

    /// Fan curve of the active Balanced/Eco mode (Turbo overrides both).
    pub fn active_curve(&self) -> Option<&FanCurve> {
        if self.settings.turbo {
            None
        } else if self.settings.eco {
            Some(&self.settings.eco_curve)
        } else if self.settings.balanced {
            Some(&self.settings.balanced_curve)
        } else {
            None
        }
    }

    /// Fan mode actually sent for group `g`: Max under Turbo, Custom while a curve runs (Auto
    /// if it fell back), otherwise the user's choice.
    pub fn effective_fan_mode(&self, g: FanGroup) -> FanMode {
        if self.settings.turbo {
            FanMode::Max
        } else if self.active_curve().is_some() {
            if self.curve_fell_back_to_auto {
                FanMode::Auto
            } else {
                FanMode::Custom
            }
        } else {
            self.settings.fan_mode[g.index()]
        }
    }

    /// Custom duty (%) for group `g`: the curve's while one runs, otherwise the user's.
    pub fn effective_fan_pct(&self, g: FanGroup) -> u8 {
        if self.active_curve().is_some() {
            self.tracks[g.index()].pct()
        } else {
            self.settings.fan_pct[g.index()]
        }
    }

    /// GPU level actually sent: Normal on battery and in Eco, Turbo under Turbo, otherwise the
    /// user's level capped at Faster.
    pub fn effective_gpu_oc(&self) -> GpuOc {
        if !self.power.allowed() || (self.settings.eco && !self.settings.turbo) {
            GpuOc::Normal
        } else if self.settings.turbo {
            GpuOc::Turbo
        } else {
            // The firmware ties its turbo flag to GPU level Turbo (misc 1 reads 1 exactly when
            // misc 5 reads 0x22), so outside
            // Turbo mode the GPU tops out at Faster; anything higher would switch Turbo back on.
            self.settings.gpu_oc.min(GpuOc::Faster)
        }
    }

    /// Write the selected parts (`part::*`) of the derived state to the firmware. Each value is
    /// read first and written only when it differs. Stops at the first error.
    ///
    /// # Errors
    /// The first firmware or WMI failure (`Error::Hw`, `Error::Wmi`); parts after it are not
    /// written.
    pub fn apply(&self, parts: u16) -> Result<()> {
        let hw = &self.hw;
        if parts & part::TURBO != 0 {
            if self.caps.turbo_led {
                let on = query(hw, Method::GetGamingLed, TURBO_LED_ID, decode_turbo_led)?;
                if on != self.settings.turbo {
                    exec(hw, Method::SetGamingLed, if self.settings.turbo { TURBO_LED_ON } else { TURBO_LED_OFF })?;
                }
            }
            if self.caps.turbo_flag {
                self.set_misc(MiscSetting::Turbo, u8::from(self.settings.turbo), 0xFF)?;
            }
        }
        if parts & part::FANS != 0 && self.caps.fan_slots != 0 {
            let items = FanGroup::ALL.map(|g| (g, self.effective_fan_mode(g)));
            exec(hw, Method::SetGamingFanBehavior, fan_behavior_input(&items, self.caps.fan_slots))?;
            for g in FanGroup::ALL {
                if self.effective_fan_mode(g) == FanMode::Custom {
                    self.send_fan_speed(g, self.effective_fan_pct(g))?;
                }
            }
        }
        if parts & part::GPU != 0 && self.caps.gpu_oc {
            // Read back as 0x2L (L = level), hence the 0x0F mask.
            self.set_misc(MiscSetting::GpuOc, self.effective_gpu_oc() as u8, 0x0F)?;
        }
        if parts & part::COOLBOOST != 0 && self.caps.coolboost {
            let on = query(hw, Method::ApgeGetFunction, COOLBOOST_GET, decode_coolboost)?;
            if on != self.settings.coolboost {
                exec(hw, Method::ApgeSetFunction, coolboost_set_input(self.settings.coolboost))?;
            }
        }
        if parts & part::LCD_OVERDRIVE != 0 && self.caps.lcd_overdrive {
            let on = query(hw, Method::GetGamingProfile, PROFILE_GET, decode_lcd_overdrive)?;
            if on != self.settings.lcd_overdrive {
                exec(hw, Method::SetGamingProfile, lcd_overdrive_set_input(self.settings.lcd_overdrive))?;
            }
        }
        if parts & part::BOOT_ANIMATION != 0 && self.caps.boot_animation {
            self.set_misc(MiscSetting::BootAnimation, u8::from(self.settings.boot_animation), 0xFF)?;
        }
        if parts & part::KB_TIMEOUT != 0 && self.caps.kb_timeout {
            let (brightness, secs) = query(hw, Method::ApgeGetFunction, KB_TIMEOUT_GET, decode_kb_timeout)?;
            if secs != self.settings.kb_timeout {
                // The firmware stores brightness in the same call: send back what it holds.
                exec(hw, Method::ApgeSetFunction, kb_timeout_set_input(brightness, self.settings.kb_timeout))?;
            }
        }
        if parts & part::USB_CHARGE != 0 && self.caps.usb_charge {
            let floor = query(hw, Method::ApgeGetFunction, USB_CHARGE_GET, decode_usb_charge)?;
            if floor != self.settings.usb_charge {
                exec(hw, Method::ApgeSetFunction, usb_charge_set_input(self.settings.usb_charge))?;
            }
        }
        if parts & part::KB_LIGHTING != 0 && self.caps.kb_effects {
            let payload = kb_backlight_payload(&self.settings.kb);
            check(hw.call_bytes(Method::SetGamingKbBacklight, &payload)?)
                .map_err(|e| Error::Hw(Method::SetGamingKbBacklight, e))?;
            if self.settings.kb.effect == KbEffect::Static {
                for z in 0..KB_ZONES {
                    if self.caps.has_zone(z) {
                        self.send_zone(z)?;
                    }
                }
            }
        }
        if parts & part::WIN_KEY != 0 && self.caps.win_key {
            let on = query(hw, Method::GetGamingProfile, PROFILE_GET, decode_win_key)?;
            if on != self.settings.win_key {
                exec(hw, Method::SetGamingProfile, win_key_set_input(self.settings.win_key))?;
            }
        }
        Ok(())
    }

    /// Write a misc setting only if it differs: the firmware treats a write of the value it
    /// already holds like a Turbo-button press (seen on a PT515-51 after a button press).
    /// `mask` selects the bits of the read-back that hold the value.
    fn set_misc(&self, idx: MiscSetting, want: u8, mask: u8) -> Result<()> {
        let cur = byte(exec(&self.hw, Method::GetGamingMiscSetting, idx as u64)?, 0) & mask;
        if cur != want {
            exec(&self.hw, Method::SetGamingMiscSetting, misc_input(idx, want))?;
        }
        Ok(())
    }

    /// Set the Custom speed of every fan in group `g`: each fan has its own speed id (the GPU
    /// group has two fans).
    fn send_fan_speed(&self, g: FanGroup, duty: u8) -> Result<()> {
        for slot in self.caps.group_slots(g) {
            let input = fan_speed_input(slot, duty).ok_or(Error::Range("fan speed"))?;
            exec(&self.hw, Method::SetGamingFanSpeed, input)?;
        }
        Ok(())
    }

    /// Send the color of keyboard zone `z` (0-based).
    fn send_zone(&self, z: usize) -> Result<()> {
        let input = rgb_zone_input(1 << z, self.settings.zones[z]).ok_or(Error::Range("keyboard zone"))?;
        exec(&self.hw, Method::SetGamingRgbKb, input).map(drop)
    }

    /// Change settings and apply `parts`; on failure restore the previous settings and
    /// re-apply them (best effort). Nothing is written when the change is a no-op.
    fn update(&mut self, parts: u16, change: impl FnOnce(&mut Settings)) -> Result<()> {
        let old = self.settings.clone();
        change(&mut self.settings);
        if self.settings == old {
            return Ok(());
        }
        if let Err(e) = self.apply(parts) {
            self.settings = old;
            let _ = self.apply(parts);
            return Err(e);
        }
        Ok(())
    }

    /// Drop Eco's OS limits before another mode takes over. Returns whether Eco was on.
    fn release_eco(&mut self) -> Result<bool> {
        if !self.settings.eco {
            return Ok(false);
        }
        self.limiter.set_eco(false, self.settings.eco_gpu_mhz)?;
        Ok(true)
    }

    /// Undo `release_eco` when the change that needed it failed.
    fn restore_eco(&mut self, released: bool) {
        if released && self.settings.eco {
            let _ = self.limiter.set_eco(true, self.settings.eco_gpu_mhz);
        }
    }

    /// A change that leaves Turbo, Balanced and Eco: release Eco's limits, apply, roll back on
    /// failure.
    fn update_leaving_modes(&mut self, parts: u16, change: impl FnOnce(&mut Settings)) -> Result<()> {
        let modal = self.settings.turbo || self.settings.eco || self.settings.balanced;
        let parts = if modal { parts | part::TURBO | part::FANS | part::GPU } else { parts };
        let released = self.release_eco()?;
        let r = self.update(parts, |s| {
            change(s);
            s.turbo = false;
            s.eco = false;
            s.balanced = false;
        });
        if r.is_err() {
            self.restore_eco(released);
        }
        r
    }

    /// The user's fan mode for group `g`. Picking a fan mode by hand leaves Turbo, Balanced and
    /// Eco, like PredatorSense leaves Turbo.
    ///
    /// # Errors
    /// `Error::NotPresent` when the group has no controllable fan, or a firmware or WMI failure
    /// (`Error::Hw`, `Error::Wmi`); the settings are left as they were.
    pub fn set_fan_mode(&mut self, g: FanGroup, m: FanMode) -> Result<()> {
        if !self.caps.has_group(g) {
            return Err(Error::NotPresent("fan"));
        }
        self.update_leaving_modes(part::FANS, |s| s.fan_mode[g.index()] = m)
    }

    /// The user's Custom duty (%) for group `g`; written only while the group runs on Custom,
    /// otherwise just stored for later.
    ///
    /// # Errors
    /// `Error::Range` above 100 %, `Error::NotPresent` when the group has no controllable fan, or a
    /// firmware or WMI failure (`Error::Hw`, `Error::Wmi`); the settings are left as they were.
    pub fn set_fan_pct(&mut self, g: FanGroup, pct: u8) -> Result<()> {
        if pct > 100 {
            return Err(Error::Range("fan speed"));
        }
        if !self.caps.has_group(g) {
            return Err(Error::NotPresent("fan"));
        }
        let live = self.effective_fan_mode(g) == FanMode::Custom;
        if !live {
            self.settings.fan_pct[g.index()] = pct;
            return Ok(());
        }
        self.update(part::FANS, |s| s.fan_pct[g.index()] = pct)
    }

    /// The user's GPU level. Turbo means Turbo mode; any other choice leaves the modes.
    ///
    /// # Errors
    /// `Error::NotPresent` without GPU overclocking, `Error::NeedsAc` for an overclock on battery,
    /// or a firmware or WMI failure (`Error::Hw`, `Error::Wmi`); the settings are left as they
    /// were.
    pub fn set_gpu_oc(&mut self, level: GpuOc) -> Result<()> {
        if !self.caps.gpu_oc {
            return Err(Error::NotPresent("GPU overclocking"));
        }
        // GPU level Turbo is the firmware's Turbo mode.
        if level == GpuOc::Turbo {
            return self.set_turbo(true);
        }
        if level != GpuOc::Normal && !self.power.allowed() {
            return Err(Error::NeedsAc);
        }
        self.update_leaving_modes(part::GPU, |s| s.gpu_oc = level)
    }

    /// Turbo mode on/off. On ends Balanced and Eco and needs AC power; off returns to the
    /// user's own fan modes and GPU level.
    ///
    /// # Errors
    /// `Error::NotPresent` when the model has nothing Turbo could drive, `Error::NeedsAc` to turn
    /// it on on battery, `Error::Os` when Eco's limits can't be removed, or a firmware or WMI
    /// failure (`Error::Hw`, `Error::Wmi`); the settings are left as they were.
    pub fn set_turbo(&mut self, on: bool) -> Result<()> {
        if !(self.caps.turbo_led || self.caps.gpu_oc || self.caps.fan_slots != 0) {
            return Err(Error::NotPresent("Turbo"));
        }
        if on && !self.power.allowed() {
            return Err(Error::NeedsAc);
        }
        let released = if on { self.release_eco()? } else { false };
        let r = self.update(part::TURBO | part::FANS | part::GPU, |s| {
            s.turbo = on;
            if on {
                s.eco = false;
                s.balanced = false;
            }
        });
        if r.is_err() {
            self.restore_eco(released);
        }
        r
    }

    /// Eco mode on/off. OS limits go first (if Windows refuses, nothing changes), then the
    /// firmware part (fans on the Eco curve, GPU Normal); a firmware failure takes the OS
    /// limits back.
    ///
    /// # Errors
    /// `Error::Os` when Windows refuses the limits (nothing changed), or a firmware or WMI failure
    /// (`Error::Hw`, `Error::Wmi`); the settings are left as they were, with the limits taken back.
    pub fn set_eco(&mut self, on: bool) -> Result<Option<String>> {
        if on == self.settings.eco {
            return Ok(None);
        }
        let mhz = self.settings.eco_gpu_mhz;
        let note = self.limiter.set_eco(on, mhz)?;
        let saved = (self.tracks, self.curve_fell_back_to_auto);
        if on {
            let c = self.settings.eco_curve.clone();
            self.prime(&c);
        }
        let r = self.update(part::TURBO | part::FANS | part::GPU, |s| {
            s.eco = on;
            if on {
                s.turbo = false;
                s.balanced = false;
            }
        });
        if let Err(e) = r {
            (self.tracks, self.curve_fell_back_to_auto) = saved;
            let _ = self.limiter.set_eco(!on, mhz);
            return Err(e);
        }
        Ok(note)
    }

    /// Balanced mode on/off: full performance, fans on the Balanced curve.
    ///
    /// # Errors
    /// `Error::NotPresent` without fan control, `Error::Os` when Eco's limits can't be removed, or
    /// a firmware or WMI failure (`Error::Hw`, `Error::Wmi`); the settings are left as they were.
    pub fn set_balanced(&mut self, on: bool) -> Result<()> {
        if on == self.settings.balanced {
            return Ok(());
        }
        if on && self.caps.fan_slots == 0 {
            return Err(Error::NotPresent("fan control"));
        }
        let released = if on { self.release_eco()? } else { false };
        let saved = (self.tracks, self.curve_fell_back_to_auto);
        if on {
            let c = self.settings.balanced_curve.clone();
            self.prime(&c);
        }
        let r = self.update(part::TURBO | part::FANS | part::GPU, |s| {
            s.balanced = on;
            if on {
                s.eco = false;
                s.turbo = false;
            }
        });
        if r.is_err() {
            (self.tracks, self.curve_fell_back_to_auto) = saved;
            self.restore_eco(released);
        }
        r
    }

    /// Flip Balanced; returns the new state.
    ///
    /// # Errors
    /// As `set_balanced`.
    pub fn toggle_balanced(&mut self) -> Result<bool> {
        let on = !self.settings.balanced;
        self.set_balanced(on).map(|()| on)
    }

    /// Temperature that drives a fan group: CPU fan by CPU, GPU fans by GPU (CPU if no GPU
    /// sensor). `None` when unreadable or implausible.
    fn group_temp(&self, g: FanGroup) -> Option<f32> {
        let sensor = match g {
            FanGroup::Gpu if self.caps.sensors.contains(&Sensor::GpuTemp) => Sensor::GpuTemp,
            _ => Sensor::CpuTemp,
        };
        query(&self.hw, Method::GetGamingSysInfo, sensor.input(), decode_sensor)
            .ok()
            .filter(|t| VALID_TEMP_C.contains(t))
            .map(f32::from)
    }

    /// Start a curve from the current temperatures (no smoothing history yet). Falls back to
    /// Auto (`curve_fell_back_to_auto`) when a temperature is unreadable.
    fn prime(&mut self, curve: &FanCurve) {
        self.tracks = Default::default();
        let mut missing = false;
        for g in FanGroup::ALL {
            if !self.caps.has_group(g) {
                continue;
            }
            match self.group_temp(g) {
                Some(t) => {
                    self.tracks[g.index()].step(curve, t);
                }
                None => missing = true,
            }
        }
        self.curve_fell_back_to_auto = missing;
    }

    /// Status-line summary of the curve, e.g. "Fans: CPU 25% (61 °C), GPU off (55 °C)".
    pub fn curve_status(&self) -> String {
        if self.curve_fell_back_to_auto {
            return "Fans on Auto (temperature unreadable)".into();
        }
        let describe = |g: FanGroup| {
            let track = self.tracks[g.index()];
            let speed = if track.pct() == 0 { "off".to_string() } else { format!("{}%", track.pct()) };
            match track.temp() {
                Some(celsius) => format!("{} {speed} ({celsius:.0} °C)", g.label()),
                None => format!("{} {speed}", g.label()),
            }
        };
        let groups: Vec<String> = FanGroup::ALL.into_iter().filter(|g| self.caps.has_group(*g)).map(describe).collect();
        format!("Fans: {}", groups.join(", "))
    }

    /// Balanced/Eco fan curve; call every 2 s while one of them is on. Only changed speeds are
    /// written. Returns the curve summary when something changed. A failed write leaves the
    /// curve state as it was, so the next tick tries the same change again.
    ///
    /// # Errors
    /// A firmware or WMI failure (`Error::Hw`, `Error::Wmi`); the curve state is left as it was.
    pub fn curve_tick(&mut self) -> Result<Option<String>> {
        let saved = (self.tracks, self.curve_fell_back_to_auto);
        let r = self.advance_curve();
        if r.is_err() {
            (self.tracks, self.curve_fell_back_to_auto) = saved;
        }
        r
    }

    /// One curve step for `curve_tick`: read the temperatures, step the tracks, write changes.
    fn advance_curve(&mut self) -> Result<Option<String>> {
        let Some(curve) = self.active_curve().cloned() else { return Ok(None) };
        let groups: Vec<FanGroup> = FanGroup::ALL.into_iter().filter(|g| self.caps.has_group(*g)).collect();
        let temps: Vec<Option<f32>> = groups.iter().map(|g| self.group_temp(*g)).collect();
        if temps.iter().any(Option::is_none) {
            if self.curve_fell_back_to_auto {
                return Ok(None);
            }
            // Fail safe: hand the fans back to the firmware.
            self.curve_fell_back_to_auto = true;
            self.apply(part::FANS)?;
            return Ok(Some(self.curve_status()));
        }
        let was_auto = self.curve_fell_back_to_auto;
        self.curve_fell_back_to_auto = false;
        let before = self.tracks.map(|t| t.pct());
        for (g, temp) in groups.iter().zip(&temps) {
            self.tracks[g.index()].step(&curve, temp.unwrap_or_default());
        }
        let after = self.tracks.map(|t| t.pct());
        if was_auto {
            // Back from Auto: the fan mode has to be set to Custom again too.
            self.apply(part::FANS)?;
        } else {
            for &g in &groups {
                if before[g.index()] != after[g.index()] {
                    self.send_fan_speed(g, after[g.index()])?;
                }
            }
        }
        Ok((was_auto || before != after).then(|| self.curve_status()))
    }

    /// Flip Eco; returns the new state and the limiter's note.
    ///
    /// # Errors
    /// As `set_eco`.
    pub fn toggle_eco(&mut self) -> Result<(bool, Option<String>)> {
        let on = !self.settings.eco;
        self.set_eco(on).map(|n| (on, n))
    }

    /// Start the active Balanced/Eco curve from current temperatures (start-up from settings).
    pub fn prime_mode_curve(&mut self) {
        if let Some(c) = self.active_curve().cloned() {
            self.prime(&c);
        }
    }

    /// Re-apply (or, when Eco is off, clean up) the OS limits: at start-up, after a crash,
    /// after sleep.
    ///
    /// # Errors
    /// `Error::Os` when Windows refuses the change.
    pub fn sync_limits(&mut self) -> Result<Option<String>> {
        self.limiter.set_eco(self.settings.eco && !self.settings.turbo, self.settings.eco_gpu_mhz)
    }

    /// Leave nothing unguarded on exit: no curve runs without the app, so fans go back to the
    /// firmware's Auto and Eco's OS limits are removed. Settings keep the mode, so it
    /// returns at the next start.
    pub fn release_modes_on_exit(&mut self) {
        if self.active_curve().is_none() {
            return;
        }
        if self.settings.eco {
            let _ = self.limiter.set_eco(false, self.settings.eco_gpu_mhz);
        }
        self.curve_fell_back_to_auto = true;
        let _ = self.apply(part::FANS | part::GPU);
    }

    /// The firmware's GPU mode value, for `--status`. Not the mode for the next start: on the
    /// PT515-51 it reads Discrete after every start, whichever mode is running (see
    /// `protocol::next_gpu_mode`).
    pub fn gpu_mode_setting(&self) -> Option<GpuMode> {
        if !self.caps.gpu_mode {
            return None;
        }
        query(&self.hw, Method::GetGamingMiscSetting, MiscSetting::GpuMode as u64, decode_misc)
            .ok()
            .and_then(GpuMode::from_u8)
    }

    /// Choose the GPU mode for the next start. Stored by the firmware itself, so it is not part
    /// of `Settings` and never re-applied. Always written, as PredatorSense's
    /// `Set_System_GPUMode` does: the value read back can't be trusted to skip the write.
    ///
    /// # Errors
    /// `Error::NotPresent` without a GPU mode switch, or a firmware or WMI failure (`Error::Hw`,
    /// `Error::Wmi`).
    pub fn set_gpu_mode(&mut self, m: GpuMode) -> Result<()> {
        if !self.caps.gpu_mode {
            return Err(Error::NotPresent("GPU mode switch"));
        }
        exec(&self.hw, Method::SetGamingMiscSetting, misc_input(MiscSetting::GpuMode, m as u8)).map(drop)
    }

    /// The Turbo button makes the firmware flip its turbo flag (misc 1) and GPU level (misc 5)
    /// by itself, with no event. Compare the flag with our state and complete or undo Turbo.
    ///
    /// # Errors
    /// `Error::Os` when Eco's limits can't be removed, or a firmware or WMI failure (`Error::Hw`,
    /// `Error::Wmi`); the settings are left as they were, so the next poll tries again.
    pub fn poll_turbo_button(&mut self) -> Result<TurboButton> {
        if !self.caps.turbo_flag {
            return Ok(TurboButton::Unchanged);
        }
        let on = byte(exec(&self.hw, Method::GetGamingMiscSetting, MiscSetting::Turbo as u64)?, 0) == 1;
        if on == self.settings.turbo {
            return Ok(TurboButton::Unchanged);
        }
        // On every failure below the settings stay (or go back to) as they were, so the next
        // poll sees the press again and retries.
        if on && !self.power.allowed() {
            // Take the firmware's Turbo, then switch it off again (`set_turbo` only writes a
            // change). If that fails, Turbo goes back to off in the settings.
            self.settings.turbo = true;
            if let Err(e) = self.set_turbo(false) {
                self.settings.turbo = false;
                return Err(e);
            }
            return Ok(TurboButton::RefusedOnBattery);
        }
        // The button wins over Balanced and Eco. If Windows keeps Eco's limits, nothing changes.
        let released = if on { self.release_eco()? } else { false };
        let saved = self.settings.clone();
        self.settings.turbo = on;
        if on {
            self.settings.eco = false;
            self.settings.balanced = false;
        }
        // The firmware already switched its flag and GPU level; add what it does not do: the
        // button LED, fans, and the user's own GPU level when leaving Turbo. Writes are skipped
        // for values the firmware already holds, so its flag is not touched.
        if let Err(e) = self.apply(part::TURBO | part::FANS | part::GPU) {
            self.settings = saved;
            self.restore_eco(released);
            return Err(e);
        }
        Ok(if on { TurboButton::On } else { TurboButton::Off })
    }

    /// Flip Turbo; returns the new state.
    ///
    /// # Errors
    /// As `set_turbo`.
    pub fn toggle_turbo(&mut self) -> Result<bool> {
        let on = !self.settings.turbo;
        self.set_turbo(on).map(|()| on)
    }

    // Simple settings: check support, change, apply, roll back on failure.

    /// CoolBoost on or off.
    ///
    /// # Errors
    /// `Error::NotPresent` without CoolBoost, or a firmware or WMI failure (`Error::Hw`,
    /// `Error::Wmi`); the settings are left as they were.
    pub fn set_coolboost(&mut self, on: bool) -> Result<()> {
        if !self.caps.coolboost {
            return Err(Error::NotPresent("CoolBoost"));
        }
        self.update(part::COOLBOOST, |s| s.coolboost = on)
    }

    /// LCD overdrive on or off.
    ///
    /// # Errors
    /// `Error::NotPresent` without LCD overdrive, or a firmware or WMI failure (`Error::Hw`,
    /// `Error::Wmi`); the settings are left as they were.
    pub fn set_lcd_overdrive(&mut self, on: bool) -> Result<()> {
        if !self.caps.lcd_overdrive {
            return Err(Error::NotPresent("LCD overdrive"));
        }
        self.update(part::LCD_OVERDRIVE, |s| s.lcd_overdrive = on)
    }

    /// Win key enabled (`true`) or locked.
    ///
    /// # Errors
    /// `Error::NotPresent` without the Win key lock, or a firmware or WMI failure (`Error::Hw`,
    /// `Error::Wmi`); the settings are left as they were.
    pub fn set_win_key(&mut self, on: bool) -> Result<()> {
        if !self.caps.win_key {
            return Err(Error::NotPresent("Win key lock"));
        }
        self.update(part::WIN_KEY, |s| s.win_key = on)
    }

    /// Boot animation and sound on or off.
    ///
    /// # Errors
    /// `Error::NotPresent` without the boot animation setting, or a firmware or WMI failure
    /// (`Error::Hw`, `Error::Wmi`); the settings are left as they were.
    pub fn set_boot_animation(&mut self, on: bool) -> Result<()> {
        if !self.caps.boot_animation {
            return Err(Error::NotPresent("Boot animation"));
        }
        self.update(part::BOOT_ANIMATION, |s| s.boot_animation = on)
    }

    /// `secs` must be one of `KB_TIMEOUTS`.
    ///
    /// # Errors
    /// `Error::Range` for any other value, `Error::NotPresent` without the setting, or a firmware
    /// or WMI failure (`Error::Hw`, `Error::Wmi`); the settings are left as they were.
    pub fn set_kb_timeout(&mut self, secs: u8) -> Result<()> {
        if !KB_TIMEOUTS.contains(&secs) {
            return Err(Error::Range("backlight timeout"));
        }
        if !self.caps.kb_timeout {
            return Err(Error::NotPresent("Backlight timeout"));
        }
        self.update(part::KB_TIMEOUT, |s| s.kb_timeout = secs)
    }

    /// `floor` must be one of `USB_CHARGE_FLOORS` (0 = off).
    ///
    /// # Errors
    /// `Error::Range` for any other value, `Error::NotPresent` without the setting, or a firmware
    /// or WMI failure (`Error::Hw`, `Error::Wmi`); the settings are left as they were.
    pub fn set_usb_charge(&mut self, floor: u8) -> Result<()> {
        if !USB_CHARGE_FLOORS.contains(&floor) {
            return Err(Error::Range("USB charging floor"));
        }
        if !self.caps.usb_charge {
            return Err(Error::NotPresent("Power-off USB charging"));
        }
        self.update(part::USB_CHARGE, |s| s.usb_charge = floor)
    }

    /// Keyboard lighting effect; with Static the zone colors are sent again too.
    ///
    /// # Errors
    /// `Error::NotPresent` without keyboard lighting, `Error::Range` for a speed or brightness
    /// above the maximum, or a firmware or WMI failure (`Error::Hw`, `Error::Wmi`); the settings
    /// are left as they were.
    pub fn set_kb(&mut self, kb: KbBacklight) -> Result<()> {
        if !self.caps.kb_effects {
            return Err(Error::NotPresent("keyboard lighting"));
        }
        if kb.speed > KB_MAX_SPEED {
            return Err(Error::Range("lighting speed"));
        }
        if kb.brightness > KB_MAX_BRIGHTNESS {
            return Err(Error::Range("brightness"));
        }
        self.update(part::KB_LIGHTING, |s| {
            s.kb = kb;
            s.restore_kb_lighting = true;
        })
    }

    /// Color of zone `z` (0-based). Sent only while the effect is Static; otherwise stored
    /// for when it is.
    ///
    /// # Errors
    /// `Error::NotPresent` for a zone the keyboard doesn't have, or a firmware or WMI failure
    /// (`Error::Hw`, `Error::Wmi`); the zone keeps its old color.
    pub fn set_zone_color(&mut self, z: usize, c: Rgb) -> Result<()> {
        if !self.caps.has_zone(z) {
            return Err(Error::NotPresent("keyboard zone"));
        }
        let old = self.settings.zones[z];
        self.settings.zones[z] = c;
        if self.settings.kb.effect == KbEffect::Static && old != c {
            self.send_zone(z).inspect_err(|_| self.settings.zones[z] = old)?;
        }
        self.settings.restore_kb_lighting = true;
        Ok(())
    }

    /// Put the saved settings back on the hardware (start-up, after sleep). Firmware-owned
    /// settings are skipped (see `sync_firmware_owned`), and so is the keyboard lighting until
    /// the user has set it in the app.
    ///
    /// # Errors
    /// As `apply`.
    pub fn restore(&mut self) -> Result<()> {
        let parts = if self.settings.restore_kb_lighting { part::RESTORE } else { part::RESTORE & !part::KB_LIGHTING };
        self.apply(parts)
    }

    /// React to a power change. Returns a user-facing note when overclocking was paused/resumed.
    /// Turbo is switched off for good (the user turns it on again); the GPU level comes back.
    /// If the firmware refuses, the power state is not taken, so the next call retries.
    ///
    /// # Errors
    /// A firmware or WMI failure (`Error::Hw`, `Error::Wmi`); the power change is then not taken.
    pub fn on_power(&mut self, on_ac: bool, battery: Option<u8>) -> Result<Option<&'static str>> {
        let saved = (self.power, self.settings.turbo);
        if !self.power.update(on_ac, battery) {
            return Ok(None);
        }
        let r = self.apply_power_change();
        if r.is_err() {
            (self.power, self.settings.turbo) = saved;
        }
        r
    }

    /// The writes for a power change `on_power` just took.
    fn apply_power_change(&mut self) -> Result<Option<&'static str>> {
        if self.power.allowed() {
            self.apply(part::GPU)?;
            return Ok(Some("Overclocking resumed"));
        }
        let was_turbo = self.settings.turbo;
        self.settings.turbo = false;
        self.apply(if was_turbo { part::TURBO | part::FANS | part::GPU } else { part::GPU })?;
        Ok(Some(if was_turbo {
            "Turbo stopped: on battery or low battery"
        } else {
            "Overclocking paused: on battery or low battery"
        }))
    }

    /// Current value of every detected sensor (`None` = read failed).
    pub fn read_sensors(&self) -> Vec<(Sensor, Option<u16>)> {
        self.caps
            .sensors
            .iter()
            .map(|&s| (s, query(&self.hw, Method::GetGamingSysInfo, s.input(), decode_sensor).ok()))
            .collect()
    }

    /// Duty cycle (%) of a fan sensor, if the firmware reports it.
    pub fn read_fan_duty(&self, s: Sensor) -> Option<u8> {
        if !self.caps.fan_duty.contains(&s) {
            return None;
        }
        let input = s.duty_input()?;
        query(&self.hw, Method::GetGamingSysInfo, input, decode_duty).ok().flatten()
    }

    /// Take the `part::FIRMWARE_OWNED` settings from the firmware (read-only); a failed read keeps
    /// the current value. Returns whether anything changed.
    pub fn sync_firmware_owned(&mut self) -> bool {
        let mut s = self.settings.clone();
        self.read_firmware_owned(&mut s);
        let changed = s != self.settings;
        self.settings = s;
        changed
    }

    /// Overwrite the firmware-owned fields of `s` with what the firmware reports; fields whose
    /// read fails are left alone.
    fn read_firmware_owned(&self, s: &mut Settings) {
        let hw = &self.hw;
        if self.caps.lcd_overdrive {
            if let Ok(on) = query(hw, Method::GetGamingProfile, PROFILE_GET, decode_lcd_overdrive) {
                s.lcd_overdrive = on;
            }
        }
        if self.caps.boot_animation {
            if let Ok(v) = query(hw, Method::GetGamingMiscSetting, MiscSetting::BootAnimation as u64, decode_misc) {
                s.boot_animation = v == 1;
            }
        }
        if self.caps.usb_charge {
            if let Ok(floor) = query(hw, Method::ApgeGetFunction, USB_CHARGE_GET, decode_usb_charge) {
                s.usb_charge = floor;
            }
        }
    }

    /// Build settings from what the firmware reports (read-only). Used when there is no config.
    pub fn read_state(&self) -> Settings {
        let hw = &self.hw;
        let mut s = Settings::default();
        // The firmware turbo flag is what the Turbo button flips; the LED is only a fallback.
        let turbo = if self.caps.turbo_flag {
            query(hw, Method::GetGamingMiscSetting, MiscSetting::Turbo as u64, decode_misc).ok() == Some(1)
        } else {
            self.caps.turbo_led && query(hw, Method::GetGamingLed, TURBO_LED_ID, decode_turbo_led).ok() == Some(true)
        };
        for g in FanGroup::ALL {
            // The first fan of the group that answers stands for the group.
            let mode = self.caps.group_slots(g).find_map(|slot| {
                query(hw, Method::GetGamingFanBehavior, 1 << slot, |o| decode_fan_mode(slot, o)).ok().flatten()
            });
            if let Some(m) = mode {
                // Under Turbo the fans read Max; the user's own choice is unknown, so assume Auto.
                s.fan_mode[g.index()] = if turbo && m == FanMode::Max { FanMode::Auto } else { m };
            }
        }
        s.turbo = turbo;
        // GetGamingMiscSetting(5) payload is 0x2L with L = level (seen: 0x20 Normal, 0x22 Turbo).
        // Under Turbo the user's own level is unknown, so keep Normal.
        if self.caps.gpu_oc && !turbo {
            s.gpu_oc = query(hw, Method::GetGamingMiscSetting, MiscSetting::GpuOc as u64, decode_misc)
                .ok()
                .and_then(|v| GpuOc::from_u8(v & 0x0F))
                .unwrap_or(GpuOc::Normal)
                .min(GpuOc::Faster);
        }
        self.read_firmware_owned(&mut s);
        if self.caps.coolboost {
            s.coolboost = query(hw, Method::ApgeGetFunction, COOLBOOST_GET, decode_coolboost).ok() == Some(true);
        }
        if self.caps.win_key {
            // Unknown reads as working keys, so the window never shows them locked by mistake.
            s.win_key = query(hw, Method::GetGamingProfile, PROFILE_GET, decode_win_key).ok() != Some(false);
        }
        if self.caps.kb_timeout {
            if let Ok((_, secs)) = query(hw, Method::ApgeGetFunction, KB_TIMEOUT_GET, decode_kb_timeout) {
                s.kb_timeout = secs;
            }
        }
        for z in 0..KB_ZONES {
            if self.caps.has_zone(z) {
                if let Ok(c) = query(hw, Method::GetGamingRgbKb, 1 << z, decode_rgb) {
                    s.zones[z] = c;
                }
            }
        }
        // The effect can't be read, so `kb` is only a placeholder: leave the lighting alone until
        // the user sets it.
        s.restore_kb_lighting = false;
        s
    }
}

#[cfg(test)]
pub mod mock {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// Test backend: records calls; answers from `replies` (default: success with payload 0).
    /// Writes of the settings the controller reads back are reflected in `replies`, like the
    /// firmware would.
    #[derive(Default)]
    pub struct Mock {
        /// Every integer call, in order (`call_bytes` appears as its payload length).
        pub calls: RefCell<Vec<(Method, u64)>>,
        /// Payloads of `call_bytes`.
        pub byte_calls: RefCell<Vec<Vec<u8>>>,
        /// Raw output per `(method, input)`.
        pub replies: RefCell<HashMap<(Method, u64), u64>>,
        /// Make every call of this method answer with this raw output (a failing status).
        pub fail: RefCell<Option<(Method, u64)>>,
        /// Every call fails with a WMI error and is not recorded.
        pub wmi_down: RefCell<bool>,
    }

    impl Mock {
        /// Replies captured from the read-only probe of a PT515-51.
        #[must_use]
        pub fn pt515() -> Mock {
            let m = Mock::default();
            let r = [
                // Fans in slots 0, 3 and 4, all on Auto.
                ((Method::GetGamingFanBehavior, 0x1), 0x100),
                ((Method::GetGamingFanBehavior, 0x2), 0x0),
                ((Method::GetGamingFanBehavior, 0x4), 0x0),
                ((Method::GetGamingFanBehavior, 0x8), 0x4000),
                ((Method::GetGamingFanBehavior, 0x10), 0x1_0000),
                ((Method::GetGamingFanBehavior, 0x20), 0x2),
                ((Method::GetGamingFanBehavior, 0x40), 0x2),
                ((Method::GetGamingFanBehavior, 0x80), 0x2),
                // CPU 77 °C, CPU fan 2960 RPM, system 62 °C, GPU fan 4274 RPM, fan 3 4062 RPM,
                // GPU 59 °C.
                ((Method::GetGamingSysInfo, 0x101), 0x4D00),
                ((Method::GetGamingSysInfo, 0x201), 0xB_9000),
                ((Method::GetGamingSysInfo, 0x301), 0x3E00),
                ((Method::GetGamingSysInfo, 0x601), 0x10_B200),
                ((Method::GetGamingSysInfo, 0x901), 0xF_DE00),
                ((Method::GetGamingSysInfo, 0xA01), 0x3B00),
                // Zones 1-3 Acer cyan, no zone 4.
                ((Method::GetGamingRgbKb, 0x1), 0xC7AE_0000),
                ((Method::GetGamingRgbKb, 0x2), 0xC7AE_0000),
                ((Method::GetGamingRgbKb, 0x4), 0xC7AE_0000),
                ((Method::GetGamingRgbKb, 0x8), 0x1),
                // Turbo LED and flag off, GPU Normal (0x20), boot animation off, Discrete mode.
                ((Method::GetGamingLed, 0x1), 0x0),
                ((Method::GetGamingMiscSetting, 0x1), 0x0),
                ((Method::GetGamingMiscSetting, 0x5), 0x2000),
                ((Method::GetGamingMiscSetting, 0x6), 0x0),
                ((Method::GetGamingMiscSetting, 0x2), 0x200),
                // CoolBoost off.
                ((Method::ApgeGetFunction, 0x207), 0x1_0000),
                // Read while the Win key was locked (LCD overdrive on).
                ((Method::GetGamingProfile, 0x0), 0x0001_0000_0000_0100),
                // Keyboard backlight: brightness 100, off after 120 s.
                ((Method::ApgeGetFunction, 0x8_8401), 0x7864_0008_0000),
                // Power-off USB charging on, until 30 %.
                ((Method::ApgeGetFunction, 0x204), 0x1E_0F00),
                // Fan duty: CPU 38 %, GPU 19 %, fan 3 18 %.
                ((Method::GetGamingSysInfo, 0xC01), 0x2600),
                ((Method::GetGamingSysInfo, 0x1001), 0x1300),
                ((Method::GetGamingSysInfo, 0x1101), 0x1200),
            ];
            m.replies.borrow_mut().extend(r);
            m
        }

        /// The calls recorded so far; clears the record.
        pub fn take(&self) -> Vec<(Method, u64)> {
            std::mem::take(&mut self.calls.borrow_mut())
        }

        /// Like `take`, keeping only the Set* calls.
        pub fn sets(&self) -> Vec<(Method, u64)> {
            self.take().into_iter().filter(|(m, _)| m.name().starts_with("Set")).collect()
        }
    }

    impl Backend for Mock {
        fn call(&self, m: Method, input: u64) -> Result<u64> {
            if *self.wmi_down.borrow() {
                return Err(Error::Wmi("0x80041003".into()));
            }
            self.calls.borrow_mut().push((m, input));
            if let Some((fm, out)) = *self.fail.borrow() {
                if fm == m {
                    return Ok(out);
                }
            }
            // Remember writes so later reads see them, like the firmware: misc settings,
            // CoolBoost, the profile word, USB charging, backlight timeout, Turbo LED.
            if m == Method::SetGamingMiscSetting {
                self.replies.borrow_mut().insert((Method::GetGamingMiscSetting, input & 0xFF), input & 0xFF00);
            }
            if m == Method::ApgeSetFunction && input & 0xFF == 7 {
                self.replies.borrow_mut().insert((Method::ApgeGetFunction, 0x207), 0x1_0000 | ((input >> 16) & 1) << 8);
            }
            if m == Method::SetGamingProfile {
                // Byte 0 bit n selects byte n + 2.
                let mut r = self.replies.borrow_mut();
                let mut cur = *r.get(&(Method::GetGamingProfile, 0)).unwrap_or(&0);
                for bit in 0..6 {
                    if input & (1 << bit) != 0 {
                        let mask = 0xFFu64 << ((bit + 2) * 8);
                        cur = (cur & !mask) | (input & mask);
                    }
                }
                r.insert((Method::GetGamingProfile, 0), cur);
            }
            if m == Method::ApgeSetFunction && input & 0xFF == 0x04 {
                self.replies.borrow_mut().insert((Method::ApgeGetFunction, 0x204), input & 0xFF_FF00);
            }
            if m == Method::ApgeSetFunction && input & 0xF_FFFF == 0x8_8402 {
                self.replies
                    .borrow_mut()
                    .insert((Method::ApgeGetFunction, 0x8_8401), (input & 0xFFFF_0000_0000) | 0x8_0000);
            }
            if m == Method::SetGamingLed {
                self.replies.borrow_mut().insert((Method::GetGamingLed, input & 0xFFFF), ((input >> 16) & 3) << 8);
            }
            Ok(*self.replies.borrow().get(&(m, input)).unwrap_or(&0))
        }

        fn call_bytes(&self, m: Method, input: &[u8]) -> Result<u64> {
            self.byte_calls.borrow_mut().push(input.to_vec());
            self.call(m, input.len() as u64)
        }

        fn get_bytes(&self, _m: Method, _input: u64) -> Result<Vec<u8>> {
            Err(Error::Wmi("not mocked".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::Mock;
    use super::*;
    use Method::*;

    fn ctl() -> Controller<Mock> {
        let hw = Mock::pt515();
        let caps = Capabilities::detect(&hw);
        hw.take();
        Controller::new(hw, caps, Settings::default())
    }

    const ALL_AUTO: u64 = 0x19 | (0b01_01_00_00_01 << 16);
    const ALL_MAX: u64 = 0x19 | (0b10_10_00_00_10 << 16);

    #[test]
    fn detects_pt515_capabilities() {
        let c = ctl().caps;
        assert_eq!(c.fan_slots, 0b11001);
        assert_eq!(c.zones, 0b0111);
        assert!(
            c.turbo_led
                && c.turbo_flag
                && c.gpu_oc
                && c.lcd_overdrive
                && c.kb_effects
                && c.gpu_mode
                && c.coolboost
                && c.win_key
        );
        assert!(c.boot_animation && c.kb_timeout && c.usb_charge);
        assert_eq!(c.fan_duty, [Sensor::CpuFan, Sensor::GpuFan, Sensor::Fan3]);
        assert_eq!(c.sensors.len(), 6);
        assert!(c.has_group(FanGroup::Cpu) && c.has_group(FanGroup::Gpu));
        assert!(!c.has_zone(3) && !c.has_zone(99));
    }

    #[test]
    fn detect_is_read_only_and_survives_dead_wmi() {
        let hw = Mock::pt515();
        Capabilities::detect(&hw);
        assert!(hw.take().iter().all(|(m, _)| m.name().starts_with("Get")));
        *hw.wmi_down.borrow_mut() = true;
        assert_eq!(Capabilities::detect(&hw), Capabilities::default());
    }

    #[test]
    fn read_state_from_probe() {
        let c = ctl();
        let s = c.read_state();
        assert_eq!(s.fan_mode, [FanMode::Auto; 2]);
        assert!(!s.turbo && s.lcd_overdrive && !s.boot_animation);
        assert_eq!((s.kb_timeout, s.usb_charge), (120, 30));
        assert_eq!(s.zones[..3], [Rgb::ACER_CYAN; 3]);
        assert!(c.hw.take().iter().all(|(m, _)| m.name().starts_with("Get")));
    }

    #[test]
    fn read_state_turbo_comes_from_firmware_flag() {
        let mut c = ctl();
        // LED on alone does not mean Turbo when the firmware flag exists.
        c.hw.replies.borrow_mut().insert((GetGamingLed, 1), 0x100);
        assert!(!c.read_state().turbo);
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x100);
        assert!(c.read_state().turbo);
        // Without the flag, fall back to the LED.
        c.caps.turbo_flag = false;
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x0);
        assert!(c.read_state().turbo);
    }

    #[test]
    fn read_state_gpu_level_from_hardware() {
        let c = ctl();
        assert_eq!(c.read_state().gpu_oc, GpuOc::Normal); // probe: 0x2000
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 5), 0x2100);
        assert_eq!(c.read_state().gpu_oc, GpuOc::Faster);
        // Turbo level: the saved preference tops out at Faster.
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 5), 0x2200);
        assert_eq!(c.read_state().gpu_oc, GpuOc::Faster);
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 5), 0x2F00);
        assert_eq!(c.read_state().gpu_oc, GpuOc::Normal);
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 5), 0x1);
        assert_eq!(c.read_state().gpu_oc, GpuOc::Normal);
    }

    #[test]
    fn read_state_under_turbo_assumes_auto_preference() {
        let c = ctl();
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x100);
        c.hw.replies.borrow_mut().insert((GetGamingFanBehavior, 1), 0x200);
        c.hw.replies.borrow_mut().insert((GetGamingFanBehavior, 8), 0x8000);
        let s = c.read_state();
        assert!(s.turbo);
        assert_eq!(s.fan_mode, [FanMode::Auto; 2]);
    }

    #[test]
    fn turbo_on_and_off_sequence() {
        let mut c = ctl();
        assert_eq!(c.toggle_turbo(), Ok(true));
        assert_eq!(
            c.hw.sets(),
            vec![
                (SetGamingLed, 0x1_0001),
                (SetGamingMiscSetting, 0x101),
                (SetGamingFanBehavior, ALL_MAX),
                (SetGamingMiscSetting, 0x205),
            ]
        );
        assert_eq!(c.toggle_turbo(), Ok(false));
        assert_eq!(
            c.hw.sets(),
            vec![
                (SetGamingLed, 0x1),
                (SetGamingMiscSetting, 0x001),
                (SetGamingFanBehavior, ALL_AUTO),
                (SetGamingMiscSetting, 0x005),
            ]
        );
    }

    #[test]
    fn turbo_off_restores_custom_fans_and_gpu_level() {
        let mut c = ctl();
        c.set_gpu_oc(GpuOc::Faster).unwrap();
        c.set_fan_pct(FanGroup::Gpu, 70).unwrap();
        c.set_fan_mode(FanGroup::Gpu, FanMode::Custom).unwrap();
        c.hw.take();
        c.set_turbo(true).unwrap();
        c.set_turbo(false).unwrap();
        let sets = c.hw.sets();
        let tail = &sets[sets.len() - 4..];
        assert_eq!(tail[0], (SetGamingFanBehavior, 0x19 | (0b11_11_00_00_01 << 16)));
        // Both GPU-side fans get the custom speed (ids 4 and 5): 70 % duty is firmware 88 %.
        assert_eq!(tail[1], (SetGamingFanSpeed, 0x5804));
        assert_eq!(tail[2], (SetGamingFanSpeed, 0x5805));
        assert_eq!(tail[3], (SetGamingMiscSetting, 0x105));
    }

    #[test]
    fn gpu_custom_speed_reaches_fan_3() {
        // Regression: GPU at 0 % stopped only the first GPU fan; fan 3 (slot 4, id 5) kept spinning.
        let mut c = ctl();
        c.set_fan_pct(FanGroup::Gpu, 0).unwrap();
        c.set_fan_mode(FanGroup::Gpu, FanMode::Custom).unwrap();
        let speeds: Vec<_> = c.hw.sets().into_iter().filter(|(m, _)| *m == SetGamingFanSpeed).collect();
        assert_eq!(speeds, vec![(SetGamingFanSpeed, 0x0004), (SetGamingFanSpeed, 0x0005)]);
        // A machine with only one GPU fan gets one write.
        c.caps.fan_slots = 0b01001;
        c.set_fan_pct(FanGroup::Gpu, 40).unwrap();
        let speeds: Vec<_> = c.hw.sets().into_iter().filter(|(m, _)| *m == SetGamingFanSpeed).collect();
        assert_eq!(speeds, vec![(SetGamingFanSpeed, 0x4504)]);
    }

    #[test]
    fn turbo_button_press_completes_turbo_mode() {
        let mut c = ctl();
        c.set_gpu_oc(GpuOc::Faster).unwrap();
        c.set_fan_mode(FanGroup::Cpu, FanMode::Custom).unwrap();
        c.hw.take();
        // Nothing pressed: one read, no writes.
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::Unchanged));
        assert_eq!(c.hw.take(), vec![(GetGamingMiscSetting, 1)]);
        // Firmware flipped misc 1 (and its GPU level): we add fans Max etc.
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x100);
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::On));
        assert!(c.settings.turbo);
        assert!(c.hw.sets().contains(&(SetGamingFanBehavior, ALL_MAX)));
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::Unchanged));
        c.hw.take();
        // Pressed again: back to the user's custom fans and Faster GPU.
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x0);
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::Off));
        assert!(!c.settings.turbo);
        let sets = c.hw.sets();
        assert!(sets.contains(&(SetGamingFanBehavior, 0x19 | (0b01_01_00_00_11 << 16))));
        assert_eq!(sets.last(), Some(&(SetGamingMiscSetting, 0x105)));
    }

    #[test]
    fn turbo_flag_never_rewritten_with_same_value() {
        // Regression: after a button press turned the flag off, writing 0 again turned Turbo back on.
        let mut c = ctl();
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x100);
        c.poll_turbo_button().unwrap();
        c.hw.take();
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x0); // firmware: button off
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::Off));
        let sets = c.hw.sets();
        assert!(!sets.iter().any(|&(m, i)| m == SetGamingMiscSetting && i & 0xFF == 1), "{sets:?}");
        // And the next poll sees no change.
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::Unchanged));
        // A UI toggle still writes the flag when it differs.
        c.hw.take();
        c.set_turbo(true).unwrap();
        assert!(c.hw.sets().contains(&(SetGamingMiscSetting, 0x101)));
    }

    #[test]
    fn turbo_button_on_battery_is_undone() {
        let mut c = ctl();
        c.on_power(false, Some(80)).unwrap();
        c.hw.take();
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x100);
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::RefusedOnBattery));
        assert!(!c.settings.turbo);
        let sets = c.hw.sets();
        assert!(sets.contains(&(SetGamingMiscSetting, 0x001)));
        assert!(!sets.iter().any(|&(m, i)| m == SetGamingMiscSetting && i & 0xFF == 5 && i >> 8 != 0));
    }

    #[test]
    fn button_off_does_not_rewrite_firmware_values() {
        // Recorded sequence: Turbo on via the app, then the button switches the firmware off
        // (flag 0, GPU 0x20). The app follows with the button LED and fans only; the firmware's
        // own flag and GPU level are left alone.
        let mut c = ctl();
        c.set_turbo(true).unwrap();
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x0);
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 5), 0x2000);
        c.hw.take();
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::Off));
        assert_eq!(c.hw.sets(), vec![(SetGamingLed, TURBO_LED_OFF), (SetGamingFanBehavior, ALL_AUTO)]);
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::Unchanged));
        // Button on again: firmware sets flag 1 and GPU 0x22; app only adds fans Max.
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x100);
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 5), 0x2200);
        c.hw.take();
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::On));
        assert_eq!(c.hw.sets(), vec![(SetGamingLed, TURBO_LED_ON), (SetGamingFanBehavior, ALL_MAX)]);
    }

    #[test]
    fn turbo_button_errors_and_missing_flag() {
        let mut c = ctl();
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x1);
        assert_eq!(c.poll_turbo_button(), Err(Error::Hw(GetGamingMiscSetting, FirmwareError::Unsupported)));
        assert!(!c.settings.turbo);
        c.caps.turbo_flag = false;
        c.hw.take();
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::Unchanged));
        assert_eq!(c.hw.take(), []);
    }

    type Calls = std::rc::Rc<std::cell::RefCell<Vec<(bool, u16)>>>;

    /// Records Eco limiter calls; can be told to fail or to return a note.
    struct RecLimiter {
        calls: Calls,
        fail: bool,
        note: Option<String>,
    }

    impl EcoLimiter for RecLimiter {
        fn set_eco(&mut self, on: bool, gpu_mhz: u16) -> Result<Option<String>> {
            self.calls.borrow_mut().push((on, gpu_mhz));
            if self.fail {
                return Err(Error::Os("power plan refused".into()));
            }
            Ok(self.note.clone())
        }
    }

    fn eco_ctl() -> (Controller<Mock>, Calls) {
        let mut c = ctl();
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        c.limiter = Box::new(RecLimiter { calls: calls.clone(), fail: false, note: None });
        (c, calls)
    }

    fn temp(c: &Controller<Mock>, cpu: u16) {
        c.hw.replies.borrow_mut().insert((GetGamingSysInfo, 0x101), u64::from(cpu) << 8);
    }

    const ALL_CUSTOM: u64 = 0x19 | (0b11_11_00_00_11 << 16);

    #[test]
    fn coolboost_toggle_and_read_back() {
        let mut c = ctl();
        assert!(!c.read_state().coolboost);
        c.set_coolboost(true).unwrap();
        assert_eq!(c.hw.sets(), vec![(ApgeSetFunction, 0x1_0007)]);
        assert!(c.read_state().coolboost);
        // Same value again: no write.
        c.set_coolboost(true).unwrap();
        assert_eq!(c.hw.sets(), []);
        c.set_coolboost(false).unwrap();
        assert_eq!(c.hw.sets(), vec![(ApgeSetFunction, 0x0_0007)]);
    }

    #[test]
    fn coolboost_apply_only_when_different() {
        let mut c = ctl();
        let cool =
            |sets: Vec<(Method, u64)>| sets.iter().filter(|&&(m, i)| m == ApgeSetFunction && i & 0xFF == 7).count();
        c.apply(part::ALL).unwrap();
        assert_eq!(cool(c.hw.sets()), 0);
        // Settings say on (e.g. restored from the settings file): applied once.
        c.settings.coolboost = true;
        c.apply(part::ALL).unwrap();
        assert_eq!(cool(c.hw.sets()), 1);
        c.apply(part::ALL).unwrap();
        assert_eq!(cool(c.hw.sets()), 0);
    }

    #[test]
    fn coolboost_missing_or_refused() {
        let hw = Mock::pt515();
        hw.replies.borrow_mut().insert((ApgeGetFunction, 0x207), 0xE2);
        assert!(!Capabilities::detect(&hw).coolboost);
        let mut c = ctl();
        c.caps.coolboost = false;
        assert_eq!(c.set_coolboost(true), Err(Error::NotPresent("CoolBoost")));
        assert_eq!(c.hw.sets(), []);
        // Firmware rejects the write: setting rolls back.
        let mut c = ctl();
        *c.hw.fail.borrow_mut() = Some((ApgeSetFunction, 0xE1));
        assert!(c.set_coolboost(true).is_err());
        assert!(!c.settings.coolboost);
    }

    #[test]
    fn win_key_toggle_and_read_back() {
        let mut c = ctl();
        // The probe was taken while the keys were locked.
        assert!(!c.read_state().win_key);
        c.settings.win_key = false;
        c.set_win_key(true).unwrap();
        assert_eq!(c.hw.sets(), vec![(SetGamingProfile, 0x0100_0002)]);
        assert!(c.read_state().win_key);
        // Only byte 3 changed; LCD overdrive (byte 6) kept.
        assert_eq!(c.hw.replies.borrow()[&(GetGamingProfile, 0)], 0x0001_0000_0100_0100);
        c.set_win_key(false).unwrap();
        assert_eq!(c.hw.sets(), vec![(SetGamingProfile, 0x0000_0002)]);
    }

    #[test]
    fn win_key_apply_unlocks_only_when_locked() {
        // Default settings (and older settings files without the key) want the keys working.
        let c = ctl();
        c.apply(part::ALL).unwrap();
        assert_eq!(c.hw.sets().iter().filter(|(m, _)| *m == SetGamingProfile).count(), 1);
        c.apply(part::ALL).unwrap();
        assert_eq!(c.hw.sets().iter().filter(|(m, _)| *m == SetGamingProfile).count(), 0);
    }

    #[test]
    fn win_key_missing_or_refused() {
        let hw = Mock::pt515();
        hw.replies.borrow_mut().insert((GetGamingProfile, 0), 0x1);
        assert!(!Capabilities::detect(&hw).win_key);
        let mut c = ctl();
        c.caps.win_key = false;
        assert_eq!(c.set_win_key(false), Err(Error::NotPresent("Win key lock")));
        assert_eq!(c.hw.sets(), []);
        // Unknown state reads as working keys, so a missing feature never shows as locked.
        assert!(c.read_state().win_key);
        let mut c = ctl();
        c.settings.win_key = false;
        *c.hw.fail.borrow_mut() = Some((SetGamingProfile, 0x2));
        assert!(c.set_win_key(true).is_err());
        assert!(!c.settings.win_key);
    }

    #[test]
    fn lcd_overdrive_uses_profile_word() {
        let mut c = ctl();
        assert!(c.read_state().lcd_overdrive);
        c.set_lcd_overdrive(false).unwrap();
        assert_eq!(c.hw.sets(), vec![(SetGamingProfile, 0x10)]);
        // Byte 6 cleared, Win key (byte 3) untouched; boot animation (misc 6) never written.
        assert_eq!(c.hw.replies.borrow()[&(GetGamingProfile, 0)], 0x0000_0000_0000_0100);
        assert!(!c.read_state().lcd_overdrive);
        c.set_lcd_overdrive(true).unwrap();
        assert_eq!(c.hw.sets(), vec![(SetGamingProfile, 0x1_0000_0000_0010)]);
    }

    #[test]
    fn boot_animation_toggle() {
        let mut c = ctl();
        c.settings.boot_animation = false;
        c.set_boot_animation(true).unwrap();
        assert_eq!(c.hw.sets(), vec![(SetGamingMiscSetting, 0x106)]);
        assert!(c.read_state().boot_animation);
        c.set_boot_animation(false).unwrap();
        assert_eq!(c.hw.sets(), vec![(SetGamingMiscSetting, 0x006)]);
        c.caps.boot_animation = false;
        assert_eq!(c.set_boot_animation(true), Err(Error::NotPresent("Boot animation")));
    }

    #[test]
    fn startup_takes_bios_changes_instead_of_overwriting_them() {
        // Saved: boot animation on, overdrive off. BIOS setup then left boot animation off and
        // overdrive on (the probe).
        let mut c = ctl();
        c.settings.boot_animation = true;
        c.settings.lcd_overdrive = false;
        assert!(c.sync_firmware_owned());
        assert!(!c.settings.boot_animation && c.settings.lcd_overdrive);
        assert!(!c.sync_firmware_owned());
        c.hw.take();
        c.apply(part::RESTORE).unwrap();
        let sets = c.hw.sets();
        assert!(!sets.iter().any(|&(m, i)| m == SetGamingMiscSetting && i & 0xFF == 6), "{sets:?}");
        assert!(!sets.iter().any(|&(m, i)| m == SetGamingProfile && i & 0x10 != 0), "{sets:?}");
        // Reads are only taken when they succeed.
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 6), 0x1);
        c.settings.boot_animation = true;
        assert!(!c.sync_firmware_owned());
        assert!(c.settings.boot_animation);
    }

    #[test]
    fn kb_timeout_keeps_brightness_and_writes_only_changes() {
        let mut c = ctl();
        c.set_kb_timeout(30).unwrap();
        assert_eq!(c.hw.sets(), vec![(ApgeSetFunction, 0x1E64_0008_8402)]);
        assert_eq!(c.read_state().kb_timeout, 30);
        c.apply(part::ALL).unwrap();
        assert!(!c.hw.sets().iter().any(|&(m, i)| m == ApgeSetFunction && i & 0xF_FFFF == 0x8_8402));
        c.set_kb_timeout(0).unwrap();
        assert_eq!(c.hw.sets(), vec![(ApgeSetFunction, 0x0064_0008_8402)]);
        assert_eq!(c.set_kb_timeout(45), Err(Error::Range("backlight timeout")));
        c.caps.kb_timeout = false;
        assert_eq!(c.set_kb_timeout(30), Err(Error::NotPresent("Backlight timeout")));
    }

    #[test]
    fn kb_timeout_missing_or_refused() {
        let hw = Mock::pt515();
        hw.replies.borrow_mut().insert((ApgeGetFunction, 0x8_8401), 0xE2);
        assert!(!Capabilities::detect(&hw).kb_timeout);
        let mut c = ctl();
        *c.hw.fail.borrow_mut() = Some((ApgeSetFunction, 0xE1));
        assert!(c.set_kb_timeout(30).is_err());
        assert_eq!(c.settings.kb_timeout, 120);
    }

    #[test]
    fn gpu_mode_read_and_switch() {
        let mut c = ctl();
        assert_eq!(c.gpu_mode_setting(), Some(GpuMode::Discrete));
        // Written even when the firmware already reads Discrete: on the PT515-51 it reads
        // Discrete after every start, also while Hybrid runs and stays chosen.
        c.set_gpu_mode(GpuMode::Discrete).unwrap();
        assert_eq!(c.hw.sets(), vec![(SetGamingMiscSetting, 0x202)]);
        c.set_gpu_mode(GpuMode::Hybrid).unwrap();
        assert_eq!(c.hw.sets(), vec![(SetGamingMiscSetting, 0x102)]);
        assert_eq!(c.gpu_mode_setting(), Some(GpuMode::Hybrid));
        c.set_gpu_mode(GpuMode::Discrete).unwrap();
        assert_eq!(c.hw.sets(), vec![(SetGamingMiscSetting, 0x202)]);
    }

    #[test]
    fn gpu_mode_never_part_of_apply() {
        let c = ctl();
        c.apply(part::ALL).unwrap();
        assert!(!c.hw.sets().iter().any(|&(m, i)| m == SetGamingMiscSetting && i & 0xFF == 2));
    }

    #[test]
    fn gpu_mode_support_detection() {
        for (reply, supported) in [(0x100u64, true), (0x200, true), (0x000, false), (0x300, false), (0x1, false)] {
            let hw = Mock::pt515();
            hw.replies.borrow_mut().insert((GetGamingMiscSetting, 2), reply);
            let caps = Capabilities::detect(&hw);
            assert_eq!(caps.gpu_mode, supported, "reply {reply:#x}");
        }
        let mut c = ctl();
        c.caps.gpu_mode = false;
        assert_eq!(c.set_gpu_mode(GpuMode::Hybrid), Err(Error::NotPresent("GPU mode switch")));
        assert_eq!(c.gpu_mode_setting(), None);
        assert_eq!(c.hw.sets(), []);
    }

    fn gpu_temp(c: &Controller<Mock>, gpu: u16) {
        c.hw.replies.borrow_mut().insert((GetGamingSysInfo, 0xA01), u64::from(gpu) << 8);
    }

    fn speeds(c: &Controller<Mock>) -> Vec<(Method, u64)> {
        c.hw.sets().into_iter().filter(|(m, _)| *m == SetGamingFanSpeed).collect()
    }

    #[test]
    fn eco_on_limits_then_curve_speeds() {
        // Probe temperatures: CPU 77 °C (Eco curve: 20 % duty = firmware 41 %), GPU 59 °C
        // (below 65: off).
        let (mut c, calls) = eco_ctl();
        assert_eq!(c.set_eco(true), Ok(None));
        assert!(c.settings.eco && !c.curve_fell_back_to_auto);
        assert_eq!(*calls.borrow(), vec![(true, 1000)]);
        assert_eq!(
            c.hw.sets(),
            vec![
                (SetGamingFanBehavior, ALL_CUSTOM),
                (SetGamingFanSpeed, 0x2901),
                (SetGamingFanSpeed, 0x0004),
                (SetGamingFanSpeed, 0x0005),
            ]
        );
        assert_eq!(c.effective_gpu_oc(), GpuOc::Normal);
        assert_eq!(c.curve_status(), "Fans: CPU 20% (77 °C), GPU off (59 °C)");
        assert_eq!(c.set_eco(true), Ok(None));
        assert_eq!(calls.borrow().len(), 1);
    }

    #[test]
    fn eco_off_restores_user_fans_and_gpu() {
        let (mut c, calls) = eco_ctl();
        c.set_gpu_oc(GpuOc::Faster).unwrap();
        c.set_eco(true).unwrap();
        assert!(c.hw.sets().contains(&(SetGamingMiscSetting, 0x005)));
        c.set_eco(false).unwrap();
        assert_eq!(*calls.borrow(), vec![(true, 1000), (false, 1000)]);
        let sets = c.hw.sets();
        assert!(sets.contains(&(SetGamingFanBehavior, ALL_AUTO)));
        assert_eq!(sets.last(), Some(&(SetGamingMiscSetting, 0x105)));
    }

    #[test]
    fn balanced_keeps_performance_and_fans_off_when_cool() {
        let (mut c, calls) = eco_ctl();
        c.set_gpu_oc(GpuOc::Faster).unwrap();
        c.hw.take();
        temp(&c, 50);
        gpu_temp(&c, 48);
        c.set_balanced(true).unwrap();
        assert!(c.settings.balanced && !c.settings.eco && !c.settings.turbo);
        assert!(calls.borrow().is_empty(), "Balanced sets no power limits");
        assert_eq!(c.effective_gpu_oc(), GpuOc::Faster);
        assert_eq!(
            c.hw.sets(),
            vec![
                (SetGamingFanBehavior, ALL_CUSTOM),
                (SetGamingFanSpeed, 0x0001),
                (SetGamingFanSpeed, 0x0004),
                (SetGamingFanSpeed, 0x0005),
            ]
        );
        assert_eq!(c.curve_status(), "Fans: CPU off (50 °C), GPU off (48 °C)");
    }

    #[test]
    fn balanced_and_eco_exclude_each_other() {
        let (mut c, calls) = eco_ctl();
        c.set_balanced(true).unwrap();
        c.set_eco(true).unwrap();
        assert!(c.settings.eco && !c.settings.balanced);
        c.set_balanced(true).unwrap();
        assert!(c.settings.balanced && !c.settings.eco);
        assert_eq!(*calls.borrow(), vec![(true, 1000), (false, 1000)]);
        c.set_turbo(true).unwrap();
        assert!(c.settings.turbo && !c.settings.balanced);
        assert!(c.hw.sets().contains(&(SetGamingFanBehavior, ALL_MAX)));
        // Manual fan choice leaves Balanced too.
        c.set_balanced(true).unwrap();
        c.set_fan_mode(FanGroup::Cpu, FanMode::Auto).unwrap();
        assert!(!c.settings.balanced);
    }

    #[test]
    fn curve_tick_writes_only_changed_speeds() {
        let (mut c, _) = eco_ctl();
        temp(&c, 80);
        gpu_temp(&c, 50);
        c.set_balanced(true).unwrap();
        assert_eq!(c.tracks[0].pct(), 32);
        c.hw.take();
        // Steady: nothing written, nothing reported.
        assert_eq!(c.curve_tick(), Ok(None));
        assert!(c.hw.take().iter().all(|(m, _)| m.name().starts_with("Get")));
        // Cooling down: CPU speed steps down (32 -> 22 % duty, firmware 43 %); only the CPU fan
        // is written, no mode change.
        temp(&c, 50);
        let note = c.curve_tick().unwrap();
        assert!(note.unwrap().starts_with("Fans: CPU 22%"));
        assert_eq!(speeds(&c), vec![(SetGamingFanSpeed, 0x2B01)]);
        // GPU heats up for real: GPU fans start, both ids written.
        for _ in 0..10 {
            gpu_temp(&c, 75);
            c.curve_tick().unwrap();
        }
        assert!(c.tracks[1].pct() >= 20, "{:?}", c.tracks[1]);
        let gpu_writes: Vec<_> = c.hw.sets().into_iter().filter(|(_, i)| i & 0xFF == 4 || i & 0xFF == 5).collect();
        assert!(gpu_writes.is_empty() || gpu_writes.len() % 2 == 0);
    }

    #[test]
    fn curve_falls_back_to_auto_when_temperature_unreadable() {
        let (mut c, _) = eco_ctl();
        c.set_balanced(true).unwrap();
        c.hw.take();
        c.hw.replies.borrow_mut().insert((GetGamingSysInfo, 0x101), 0x1);
        assert_eq!(c.curve_tick(), Ok(Some("Fans on Auto (temperature unreadable)".into())));
        assert_eq!(c.hw.sets(), vec![(SetGamingFanBehavior, ALL_AUTO)]);
        assert_eq!(c.curve_tick(), Ok(None));
        // Readable again: back on the curve with a mode write.
        temp(&c, 70);
        assert!(c.curve_tick().unwrap().is_some());
        assert_eq!(c.hw.sets()[0], (SetGamingFanBehavior, ALL_CUSTOM));
        // Bad values (0 °C, 255 °C) count as unreadable.
        c.hw.replies.borrow_mut().insert((GetGamingSysInfo, 0x101), 0xFF00);
        c.curve_tick().unwrap();
        assert!(c.curve_fell_back_to_auto);
    }

    #[test]
    fn exit_from_a_curve_mode_leaves_fans_on_auto_but_remembers_it() {
        let (mut c, calls) = eco_ctl();
        c.set_eco(true).unwrap();
        c.hw.take();
        c.release_modes_on_exit();
        assert!(c.settings.eco, "Eco comes back at the next start");
        assert_eq!(calls.borrow().last(), Some(&(false, 1000)));
        assert_eq!(c.hw.sets(), vec![(SetGamingFanBehavior, ALL_AUTO)]);
        let (mut c, calls) = eco_ctl();
        c.set_balanced(true).unwrap();
        c.hw.take();
        c.release_modes_on_exit();
        assert!(c.settings.balanced && calls.borrow().is_empty());
        assert_eq!(c.hw.sets(), vec![(SetGamingFanBehavior, ALL_AUTO)]);
        // No curve mode: exit touches nothing.
        let (mut c, calls) = eco_ctl();
        c.release_modes_on_exit();
        assert!(calls.borrow().is_empty() && c.hw.take().is_empty());
    }

    #[test]
    fn start_up_primes_curve_and_syncs_limits() {
        let (mut c, calls) = eco_ctl();
        c.sync_limits().unwrap();
        c.settings.eco = true;
        c.prime_mode_curve();
        assert_eq!(c.tracks[0].pct(), 20);
        c.sync_limits().unwrap();
        c.settings.turbo = true;
        c.sync_limits().unwrap();
        // Off (crash clean-up), on (restored Eco), off (Turbo wins).
        assert_eq!(*calls.borrow(), vec![(false, 1000), (true, 1000), (false, 1000)]);
    }

    #[test]
    fn turbo_button_ends_balanced() {
        let (mut c, _) = eco_ctl();
        c.set_balanced(true).unwrap();
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x100);
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::On));
        assert!(c.settings.turbo && !c.settings.balanced);
        assert!(c.hw.sets().contains(&(SetGamingFanBehavior, ALL_MAX)));
    }

    #[test]
    fn turbo_ends_eco_and_releases_limits() {
        let (mut c, calls) = eco_ctl();
        c.set_eco(true).unwrap();
        c.hw.take();
        c.set_turbo(true).unwrap();
        assert!(c.settings.turbo && !c.settings.eco);
        assert_eq!(*calls.borrow(), vec![(true, 1000), (false, 1000)]);
        assert!(c.hw.sets().contains(&(SetGamingFanBehavior, ALL_MAX)));
        // And Eco ends Turbo.
        c.set_eco(true).unwrap();
        assert!(c.settings.eco && !c.settings.turbo);
        assert!(c.hw.sets().contains(&(SetGamingMiscSetting, 0x001)));
    }

    #[test]
    fn turbo_button_ends_eco() {
        let (mut c, calls) = eco_ctl();
        c.set_eco(true).unwrap();
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x100);
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::On));
        assert!(c.settings.turbo && !c.settings.eco);
        assert_eq!(calls.borrow().last(), Some(&(false, 1000)));
    }

    #[test]
    fn manual_fan_or_gpu_choice_ends_eco() {
        let (mut c, calls) = eco_ctl();
        c.set_eco(true).unwrap();
        c.set_fan_mode(FanGroup::Cpu, FanMode::Max).unwrap();
        assert!(!c.settings.eco);
        assert_eq!(calls.borrow().len(), 2);
        c.set_eco(true).unwrap();
        c.set_gpu_oc(GpuOc::Faster).unwrap();
        assert!(!c.settings.eco);
        assert_eq!(calls.borrow().last(), Some(&(false, 1000)));
        // Changing a fan percentage outside Eco does not touch the limiter.
        let n = calls.borrow().len();
        c.set_fan_pct(FanGroup::Cpu, 40).unwrap();
        assert_eq!(calls.borrow().len(), n);
    }

    #[test]
    fn eco_refused_by_windows_changes_nothing() {
        let mut c = ctl();
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        c.limiter = Box::new(RecLimiter { calls: calls.clone(), fail: true, note: None });
        assert!(matches!(c.set_eco(true), Err(Error::Os(_))));
        assert!(!c.settings.eco);
        assert_eq!(c.hw.sets(), []);
    }

    #[test]
    fn eco_firmware_failure_takes_limits_back() {
        let (mut c, calls) = eco_ctl();
        *c.hw.fail.borrow_mut() = Some((SetGamingFanBehavior, 0x2));
        assert!(c.set_eco(true).is_err());
        assert!(!c.settings.eco);
        assert_eq!(*calls.borrow(), vec![(true, 1000), (false, 1000)]);
    }

    #[test]
    fn turbo_twice_is_a_no_op() {
        let mut c = ctl();
        c.set_turbo(true).unwrap();
        c.hw.take();
        c.set_turbo(true).unwrap();
        assert_eq!(c.hw.take(), []);
    }

    #[test]
    fn turbo_refused_on_battery_without_side_effects() {
        let mut c = ctl();
        c.on_power(false, Some(80)).unwrap();
        c.hw.take();
        assert_eq!(c.set_turbo(true), Err(Error::NeedsAc));
        assert_eq!(c.set_gpu_oc(GpuOc::Turbo), Err(Error::NeedsAc));
        assert!(!c.settings.turbo);
        assert_eq!(c.hw.take(), []);
        // Going back to Normal is always allowed.
        assert_eq!(c.set_gpu_oc(GpuOc::Normal), Ok(()));
    }

    #[test]
    fn power_loss_stops_turbo_and_oc() {
        let mut c = ctl();
        c.set_turbo(true).unwrap();
        c.hw.take();
        assert_eq!(c.on_power(false, Some(90)), Ok(Some("Turbo stopped: on battery or low battery")));
        assert!(!c.settings.turbo);
        let sets = c.hw.sets();
        assert!(sets.contains(&(SetGamingLed, 0x1)));
        assert!(sets.contains(&(SetGamingFanBehavior, ALL_AUTO)));
        assert_eq!(sets.last(), Some(&(SetGamingMiscSetting, 0x005)));
        // Same state again: nothing to do.
        assert_eq!(c.on_power(false, Some(90)), Ok(None));
        assert_eq!(c.hw.take(), []);
    }

    #[test]
    fn gpu_level_kept_while_paused_and_restored_on_ac() {
        let mut c = ctl();
        c.set_gpu_oc(GpuOc::Faster).unwrap();
        c.on_power(false, Some(50)).unwrap();
        assert_eq!(c.settings.gpu_oc, GpuOc::Faster);
        assert_eq!(c.effective_gpu_oc(), GpuOc::Normal);
        c.hw.take();
        assert_eq!(c.on_power(true, Some(50)), Ok(Some("Overclocking resumed")));
        assert_eq!(c.hw.sets(), vec![(SetGamingMiscSetting, 0x105)]);
    }

    #[test]
    fn gpu_turbo_is_turbo_mode() {
        let mut c = ctl();
        c.set_gpu_oc(GpuOc::Turbo).unwrap();
        assert!(c.settings.turbo);
        assert_eq!(c.settings.gpu_oc, GpuOc::Normal);
        assert!(c.hw.sets().contains(&(SetGamingFanBehavior, ALL_MAX)));
    }

    #[test]
    fn saved_gpu_turbo_preference_does_not_re_enable_turbo() {
        // Regression: settings had gpu_oc=Turbo; after the button turned Turbo off the app wrote
        // GPU level 2 back, which the firmware reads as Turbo on again.
        let mut c = ctl();
        c.settings.gpu_oc = GpuOc::Turbo;
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x100);
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 5), 0x2200);
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::On));
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x0);
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 5), 0x2000);
        c.hw.take();
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::Off));
        let sets = c.hw.sets();
        assert!(!sets.contains(&(SetGamingMiscSetting, 0x205)), "{sets:?}");
        assert!(sets.contains(&(SetGamingMiscSetting, 0x105)));
        assert_eq!(c.effective_gpu_oc(), GpuOc::Faster);
    }

    #[test]
    fn power_gate_hysteresis() {
        let mut g = PowerGate::default();
        assert!(!g.update(true, Some(35)));
        assert!(g.allowed());
        assert!(g.update(true, Some(29)));
        assert!(!g.allowed());
        assert!(!g.update(true, Some(35)));
        assert!(!g.allowed());
        assert!(g.update(true, Some(40)));
        assert!(g.allowed());
        assert!(g.update(false, None));
        assert!(!g.allowed());
        // Desktop / unknown battery on AC is fine.
        assert!(g.update(true, None));
        assert!(g.allowed());
        assert!(!g.update(true, Some(30)));
        assert!(g.allowed());
    }

    #[test]
    fn custom_fan_speed_only_sent_in_custom_mode() {
        let mut c = ctl();
        c.set_fan_pct(FanGroup::Cpu, 80).unwrap();
        assert_eq!(c.hw.take(), []);
        c.set_fan_mode(FanGroup::Cpu, FanMode::Custom).unwrap();
        assert_eq!(
            c.hw.sets(),
            // 80 % duty on the CPU fan is firmware 91 %.
            vec![(SetGamingFanBehavior, 0x19 | (0b01_01_00_00_11 << 16)), (SetGamingFanSpeed, 0x5B01)]
        );
        c.set_fan_pct(FanGroup::Cpu, 30).unwrap();
        assert_eq!(c.hw.sets().last(), Some(&(SetGamingFanSpeed, 0x3501)));
        assert_eq!(c.set_fan_pct(FanGroup::Cpu, 101), Err(Error::Range("fan speed")));
        assert_eq!(c.settings.fan_pct[0], 30);
    }

    #[test]
    fn manual_fan_mode_leaves_turbo() {
        let mut c = ctl();
        c.set_turbo(true).unwrap();
        c.hw.take();
        c.set_fan_mode(FanGroup::Cpu, FanMode::Max).unwrap();
        assert!(!c.settings.turbo);
        let sets = c.hw.sets();
        assert_eq!(sets[0], (SetGamingLed, 0x1));
        assert!(sets.contains(&(SetGamingFanBehavior, 0x19 | (0b01_01_00_00_10 << 16))));
        assert_eq!(sets.last(), Some(&(SetGamingMiscSetting, 0x005)));
    }

    #[test]
    fn firmware_rejection_rolls_back() {
        let mut c = ctl();
        *c.hw.fail.borrow_mut() = Some((SetGamingFanBehavior, 0x2));
        let before = c.settings.clone();
        assert_eq!(c.set_turbo(true), Err(Error::Hw(SetGamingFanBehavior, FirmwareError::InvalidParam)));
        assert_eq!(c.settings, before);
        // The rollback re-sent the old LED state after the failure.
        let sets = c.hw.sets();
        assert_eq!(sets[0], (SetGamingLed, 0x1_0001));
        assert!(sets[2..].contains(&(SetGamingLed, 0x1)));
    }

    #[test]
    fn wmi_failure_surfaces_and_keeps_state() {
        let mut c = ctl();
        *c.hw.wmi_down.borrow_mut() = true;
        assert!(matches!(c.set_lcd_overdrive(false), Err(Error::Wmi(_))));
        assert!(c.settings.lcd_overdrive);
        assert!(c.read_sensors().iter().all(|(_, v)| v.is_none()));
    }

    #[test]
    fn missing_features_are_reported() {
        let hw = Mock::default();
        let mut c = Controller::new(hw, Capabilities::default(), Settings::default());
        assert_eq!(c.set_fan_mode(FanGroup::Cpu, FanMode::Max), Err(Error::NotPresent("fan")));
        assert_eq!(c.set_gpu_oc(GpuOc::Turbo), Err(Error::NotPresent("GPU overclocking")));
        assert_eq!(c.set_turbo(true), Err(Error::NotPresent("Turbo")));
        assert_eq!(c.set_zone_color(0, Rgb::default()), Err(Error::NotPresent("keyboard zone")));
        assert_eq!(c.set_kb(KbBacklight::default()), Err(Error::NotPresent("keyboard lighting")));
        assert_eq!(c.set_lcd_overdrive(true), Err(Error::NotPresent("LCD overdrive")));
        assert_eq!(c.hw.take(), []);
        assert_eq!(c.apply(part::ALL), Ok(()));
        assert_eq!(c.hw.take(), []);
    }

    #[test]
    fn keyboard_static_sends_payload_then_zones() {
        let mut c = ctl();
        c.set_zone_color(1, Rgb::new(0xFF, 0, 0)).unwrap();
        assert_eq!(c.hw.sets(), vec![(SetGamingRgbKb, 0x0000_FF02)]);
        let kb = KbBacklight { brightness: 40, ..KbBacklight::default() };
        c.set_kb(kb).unwrap();
        assert_eq!(c.hw.byte_calls.borrow().last().unwrap()[..3], [0, 0, 40]);
        let sets = c.hw.sets();
        assert_eq!(sets[0], (SetGamingKbBacklight, 16));
        assert_eq!(
            &sets[1..],
            &[(SetGamingRgbKb, 0xC7AE_0001), (SetGamingRgbKb, 0x0000_FF02), (SetGamingRgbKb, 0xC7AE_0004)]
        );
    }

    #[test]
    fn keyboard_effect_skips_zones_and_validates() {
        let mut c = ctl();
        let kb = KbBacklight { effect: KbEffect::Wave, speed: 9, ..KbBacklight::default() };
        c.set_kb(kb).unwrap();
        assert_eq!(c.hw.sets(), vec![(SetGamingKbBacklight, 16)]);
        // Zone colors are stored but not sent while an effect runs.
        c.set_zone_color(0, Rgb::new(1, 2, 3)).unwrap();
        assert_eq!(c.hw.take(), []);
        assert_eq!(c.settings.zones[0], Rgb::new(1, 2, 3));
        assert_eq!(c.set_kb(KbBacklight { speed: 10, ..kb }), Err(Error::Range("lighting speed")));
        assert_eq!(c.set_kb(KbBacklight { brightness: 101, ..kb }), Err(Error::Range("brightness")));
        assert_eq!(c.set_zone_color(3, Rgb::default()), Err(Error::NotPresent("keyboard zone")));
    }

    #[test]
    fn zone_failure_restores_color() {
        let mut c = ctl();
        *c.hw.fail.borrow_mut() = Some((SetGamingRgbKb, 0x1));
        assert!(c.set_zone_color(0, Rgb::new(9, 9, 9)).is_err());
        assert_eq!(c.settings.zones[0], Rgb::ACER_CYAN);
    }

    #[test]
    fn apply_all_matches_settings() {
        let mut c = ctl();
        c.settings.fan_mode = [FanMode::Custom, FanMode::Max];
        c.settings.fan_pct = [0, 100];
        c.settings.boot_animation = true;
        c.apply(part::ALL).unwrap();
        let sets = c.hw.sets();
        // LED off, turbo flag 0, GPU Normal and LCD overdrive on are already what the firmware
        // holds: not rewritten.
        assert_eq!(
            &sets[..3],
            &[
                (SetGamingFanBehavior, 0x19 | (0b10_10_00_00_11 << 16)),
                (SetGamingFanSpeed, 0x0001),
                (SetGamingMiscSetting, 0x106),
            ]
        );
    }

    #[test]
    fn sensors_read_and_bad_values() {
        let c = ctl();
        let r = c.read_sensors();
        assert_eq!(r[0], (Sensor::CpuTemp, Some(77)));
        assert_eq!(r[3], (Sensor::CpuFan, Some(2960)));
        c.hw.replies.borrow_mut().insert((GetGamingSysInfo, 0x101), 0x1);
        assert_eq!(c.read_sensors()[0], (Sensor::CpuTemp, None));
    }

    #[test]
    fn fan_duty_read_and_bad_values() {
        let c = ctl();
        assert_eq!(c.read_fan_duty(Sensor::CpuFan), Some(38));
        assert_eq!(c.read_fan_duty(Sensor::GpuFan), Some(19));
        assert_eq!(c.read_fan_duty(Sensor::CpuTemp), None);
        c.hw.replies.borrow_mut().insert((GetGamingSysInfo, 0xC01), 0x6500);
        assert_eq!(c.read_fan_duty(Sensor::CpuFan), None);
        // A model without duty sensors: nothing detected, nothing read.
        let hw = Mock::pt515();
        hw.replies.borrow_mut().insert((GetGamingSysInfo, 0x1101), 0x1);
        assert_eq!(Capabilities::detect(&hw).fan_duty, [Sensor::CpuFan, Sensor::GpuFan]);
    }

    #[test]
    fn untouched_keyboard_lighting_is_not_restored() {
        let sends_kb =
            |sets: &[(Method, u64)]| sets.iter().any(|(m, _)| matches!(m, SetGamingKbBacklight | SetGamingRgbKb));
        // First start: the effect can't be read, so the snapshot must not be restored over it.
        let mut c = ctl();
        c.settings = c.read_state();
        assert!(!c.settings.restore_kb_lighting);
        c.hw.take();
        c.restore().unwrap();
        assert!(!sends_kb(&c.hw.sets()), "{:?}", c.hw.sets());
        // Once the user sets the lighting, it is restored.
        c.set_kb(KbBacklight { effect: KbEffect::Wave, ..c.settings.kb }).unwrap();
        assert!(c.settings.restore_kb_lighting);
        c.hw.take();
        c.restore().unwrap();
        assert!(sends_kb(&c.hw.sets()));
        // A zone color counts as setting it too.
        let mut c = ctl();
        c.settings = c.read_state();
        c.set_zone_color(1, Rgb::new(1, 2, 3)).unwrap();
        assert!(c.settings.restore_kb_lighting);
    }

    #[test]
    fn rejected_keyboard_change_does_not_mark_it_set() {
        let mut c = ctl();
        c.settings = c.read_state();
        *c.hw.fail.borrow_mut() = Some((SetGamingRgbKb, 0x2));
        assert!(c.set_zone_color(0, Rgb::new(1, 2, 3)).is_err());
        assert!(!c.settings.restore_kb_lighting);
    }

    #[test]
    fn usb_charge_set_and_firmware_owned() {
        let mut c = ctl();
        c.settings.usb_charge = 30;
        c.set_usb_charge(20).unwrap();
        assert_eq!(c.hw.sets(), vec![(ApgeSetFunction, 0x14_0F04)]);
        assert_eq!(c.read_state().usb_charge, 20);
        c.set_usb_charge(0).unwrap();
        assert_eq!(c.hw.sets(), vec![(ApgeSetFunction, 0x0A_1F04)]);
        assert_eq!(c.set_usb_charge(15), Err(Error::Range("USB charging floor")));
        // Changed elsewhere (Care Center): taken at start, never restored over.
        c.hw.replies.borrow_mut().insert((ApgeGetFunction, 0x204), 0x1E_0F00);
        assert!(c.sync_firmware_owned());
        assert_eq!(c.settings.usb_charge, 30);
        c.settings.usb_charge = 10;
        c.hw.take();
        c.apply(part::RESTORE).unwrap();
        assert!(!c.hw.sets().iter().any(|&(m, i)| m == ApgeSetFunction && i & 0xFF == 4));
        c.caps.usb_charge = false;
        assert_eq!(c.set_usb_charge(30), Err(Error::NotPresent("Power-off USB charging")));
    }

    /// A firmware that answers "unsupported" (status 1) to every probe.
    fn bare_hw() -> Mock {
        let hw = Mock::default();
        Capabilities::detect(&hw);
        for call in hw.take() {
            hw.replies.borrow_mut().insert(call, 0x1);
        }
        hw
    }

    #[test]
    fn empty_machine_refuses_every_feature_and_writes_nothing() {
        let hw = bare_hw();
        let caps = Capabilities::detect(&hw);
        assert_eq!(caps, Capabilities::default());
        hw.take();
        let mut c = Controller::new(hw, caps, Settings::default());
        assert_eq!(c.set_fan_pct(FanGroup::Gpu, 50), Err(Error::NotPresent("fan")));
        assert_eq!(c.set_balanced(true), Err(Error::NotPresent("fan control")));
        assert_eq!(c.set_gpu_oc(GpuOc::Faster), Err(Error::NotPresent("GPU overclocking")));
        assert_eq!(c.set_coolboost(true), Err(Error::NotPresent("CoolBoost")));
        assert_eq!(c.set_win_key(false), Err(Error::NotPresent("Win key lock")));
        assert_eq!(c.set_boot_animation(false), Err(Error::NotPresent("Boot animation")));
        assert_eq!(c.set_kb_timeout(30), Err(Error::NotPresent("Backlight timeout")));
        assert_eq!(c.set_usb_charge(10), Err(Error::NotPresent("Power-off USB charging")));
        assert_eq!(c.set_gpu_mode(GpuMode::Hybrid), Err(Error::NotPresent("GPU mode switch")));
        // Range is checked before presence.
        assert_eq!(c.set_fan_pct(FanGroup::Cpu, 101), Err(Error::Range("fan speed")));
        assert_eq!(c.set_kb_timeout(45), Err(Error::Range("backlight timeout")));
        assert_eq!(c.set_usb_charge(15), Err(Error::Range("USB charging floor")));
        assert_eq!(c.settings, Settings::default());
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::Unchanged));
        assert_eq!(c.curve_tick(), Ok(None));
        assert_eq!(c.apply(0), Ok(()));
        assert_eq!(c.restore(), Ok(()));
        assert!(!c.sync_firmware_owned());
        assert_eq!(c.read_state(), Settings { restore_kb_lighting: false, ..Settings::default() });
        assert!(c.read_sensors().is_empty() && c.read_fan_duty(Sensor::CpuFan).is_none());
        assert_eq!(c.gpu_mode_setting(), None);
        assert_eq!(c.hw.take(), []);
    }

    #[test]
    fn detect_machine_without_gpu_fans_or_lighting() {
        let hw = Mock::pt515();
        hw.replies.borrow_mut().insert((GetGamingFanBehavior, 0x8), 0x0);
        hw.replies.borrow_mut().insert((GetGamingFanBehavior, 0x10), 0x0);
        for z in 0..KB_ZONES {
            hw.replies.borrow_mut().insert((GetGamingRgbKb, 1 << z), 0x1);
        }
        let caps = Capabilities::detect(&hw);
        assert_eq!((caps.fan_slots, caps.zones), (0b1, 0));
        assert!(caps.has_group(FanGroup::Cpu) && !caps.has_group(FanGroup::Gpu));
        assert!(!caps.kb_effects);
        assert!(caps.turbo_led && caps.turbo_flag && caps.gpu_oc && caps.coolboost);
        hw.take();
        let mut c = Controller::new(hw, caps, Settings::default());
        assert_eq!(c.set_fan_mode(FanGroup::Gpu, FanMode::Max), Err(Error::NotPresent("fan")));
        assert_eq!(c.set_fan_pct(FanGroup::Gpu, 10), Err(Error::NotPresent("fan")));
        assert_eq!(c.set_kb(KbBacklight::default()), Err(Error::NotPresent("keyboard lighting")));
        assert_eq!(c.set_zone_color(0, Rgb::default()), Err(Error::NotPresent("keyboard zone")));
        // Turbo and Balanced drive the CPU fan alone.
        c.set_turbo(true).unwrap();
        assert!(c.hw.sets().contains(&(SetGamingFanBehavior, 0x1 | (0b10 << 16))));
        c.set_balanced(true).unwrap();
        let sets = c.hw.sets();
        assert!(sets.contains(&(SetGamingFanBehavior, 0x1 | (0b11 << 16))));
        let speeds: Vec<_> = sets.into_iter().filter(|(m, _)| *m == SetGamingFanSpeed).collect();
        assert_eq!(speeds, vec![(SetGamingFanSpeed, fan_speed_input(0, 27).unwrap())]);
        assert_eq!(c.curve_status(), "Fans: CPU 27% (77 °C)");
        // Restoring skips the missing lighting.
        c.restore().unwrap();
        assert!(c.hw.byte_calls.borrow().is_empty());
    }

    #[test]
    fn gpu_fans_follow_cpu_temperature_without_gpu_sensor() {
        let mut c = ctl();
        c.caps.sensors.retain(|&s| s != Sensor::GpuTemp);
        // Would force Auto if it were read.
        c.hw.replies.borrow_mut().insert((GetGamingSysInfo, 0xA01), 0x1);
        temp(&c, 80);
        c.set_balanced(true).unwrap();
        assert!(!c.curve_fell_back_to_auto);
        assert_eq!(c.tracks.map(|t| t.pct()), [32, 32]);
    }

    #[test]
    fn turbo_on_a_machine_with_fans_only() {
        let mut c = ctl();
        (c.caps.turbo_led, c.caps.turbo_flag, c.caps.gpu_oc) = (false, false, false);
        c.set_turbo(true).unwrap();
        assert_eq!(c.hw.sets(), vec![(SetGamingFanBehavior, ALL_MAX)]);
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::Unchanged));
        assert_eq!(c.hw.take(), []);
        c.set_turbo(false).unwrap();
        assert_eq!(c.hw.sets(), vec![(SetGamingFanBehavior, ALL_AUTO)]);
    }

    #[test]
    fn setting_the_current_value_makes_no_calls() {
        let (mut c, calls) = eco_ctl();
        c.set_fan_mode(FanGroup::Cpu, FanMode::Auto).unwrap();
        c.set_fan_pct(FanGroup::Gpu, 50).unwrap();
        c.set_gpu_oc(GpuOc::Normal).unwrap();
        c.set_turbo(false).unwrap();
        c.set_balanced(false).unwrap();
        assert_eq!(c.set_eco(false), Ok(None));
        c.set_coolboost(false).unwrap();
        c.set_lcd_overdrive(true).unwrap();
        c.set_win_key(true).unwrap();
        c.set_boot_animation(true).unwrap();
        c.set_kb_timeout(120).unwrap();
        c.set_usb_charge(0).unwrap();
        c.set_kb(KbBacklight::default()).unwrap();
        c.set_zone_color(2, Rgb::ACER_CYAN).unwrap();
        assert_eq!(c.settings, Settings::default());
        // Same Custom speed while it is live.
        c.settings.fan_mode[0] = FanMode::Custom;
        c.set_fan_pct(FanGroup::Cpu, 50).unwrap();
        assert_eq!(c.hw.take(), []);
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn range_limits_accept_the_maximum_and_refuse_beyond() {
        let mut c = ctl();
        c.set_fan_mode(FanGroup::Cpu, FanMode::Custom).unwrap();
        c.hw.take();
        c.set_fan_pct(FanGroup::Cpu, 100).unwrap();
        assert_eq!(c.hw.sets(), [(SetGamingFanBehavior, 0x19 | (0b01_01_00_00_11 << 16)), (SetGamingFanSpeed, 0x6401)]);
        let kb = KbBacklight {
            effect: KbEffect::Wave,
            speed: KB_MAX_SPEED,
            brightness: KB_MAX_BRIGHTNESS,
            ..KbBacklight::default()
        };
        c.set_kb(kb).unwrap();
        assert_eq!(c.hw.byte_calls.borrow().last().unwrap()[..3], [3, 9, 100]);
        c.hw.take();
        assert_eq!(c.set_fan_pct(FanGroup::Cpu, u8::MAX), Err(Error::Range("fan speed")));
        for secs in [1, 29, 31, 121, u8::MAX] {
            assert_eq!(c.set_kb_timeout(secs), Err(Error::Range("backlight timeout")), "{secs}");
        }
        for floor in [5, 31, 40, u8::MAX] {
            assert_eq!(c.set_usb_charge(floor), Err(Error::Range("USB charging floor")), "{floor}");
        }
        assert_eq!(c.set_kb(KbBacklight { speed: u8::MAX, ..kb }), Err(Error::Range("lighting speed")));
        assert_eq!((c.settings.fan_pct[0], c.settings.kb), (100, kb));
        assert_eq!((c.settings.kb_timeout, c.settings.usb_charge), (120, 0));
        assert_eq!(c.hw.take(), []);
    }

    #[test]
    fn zone_index_limits_and_failures() {
        let mut c = ctl();
        c.caps.zones = 0b1111;
        c.set_zone_color(3, Rgb::new(0xFF, 0, 0)).unwrap();
        assert_eq!(c.hw.sets(), vec![(SetGamingRgbKb, 0xFF08)]);
        c.set_zone_color(3, Rgb::new(0xFF, 0, 0)).unwrap();
        assert_eq!(c.hw.take(), []);
        assert_eq!(c.set_zone_color(KB_ZONES, Rgb::default()), Err(Error::NotPresent("keyboard zone")));
        assert_eq!(c.set_zone_color(usize::MAX, Rgb::default()), Err(Error::NotPresent("keyboard zone")));
        *c.hw.wmi_down.borrow_mut() = true;
        assert!(matches!(c.set_zone_color(0, Rgb::new(1, 2, 3)), Err(Error::Wmi(_))));
        assert_eq!(c.settings.zones[0], Rgb::ACER_CYAN);
    }

    #[test]
    fn every_setter_rolls_back_when_the_firmware_refuses() {
        type Setter = fn(&mut Controller<Mock>) -> Result<()>;
        // Each changes a value the firmware does not hold yet, so it has to write.
        let cases: [(Method, Setter); 12] = [
            (ApgeSetFunction, |c| c.set_coolboost(true)),
            (SetGamingProfile, |c| c.set_lcd_overdrive(false)),
            (SetGamingProfile, |c| c.set_win_key(true)),
            (SetGamingMiscSetting, |c| c.set_boot_animation(true)),
            (ApgeSetFunction, |c| c.set_kb_timeout(30)),
            (ApgeSetFunction, |c| c.set_usb_charge(20)),
            (SetGamingMiscSetting, |c| c.set_gpu_oc(GpuOc::Faster)),
            (SetGamingFanBehavior, |c| c.set_fan_mode(FanGroup::Gpu, FanMode::Max)),
            (SetGamingLed, |c| c.set_turbo(true)),
            (SetGamingFanBehavior, |c| c.set_balanced(true)),
            (SetGamingFanBehavior, |c| c.set_eco(true).map(drop)),
            (SetGamingKbBacklight, |c| c.set_kb(KbBacklight { effect: KbEffect::Neon, ..KbBacklight::default() })),
        ];
        let refusals = [
            (0x1, FirmwareError::Unsupported),
            (0x2, FirmwareError::InvalidParam),
            (0xE2, FirmwareError::Status(0xE2)),
        ];
        for (i, (method, change)) in cases.into_iter().enumerate() {
            for (status, err) in refusals {
                let mut c = ctl();
                c.settings = c.read_state();
                let (settings, firmware) = (c.settings.clone(), c.hw.replies.borrow().clone());
                *c.hw.fail.borrow_mut() = Some((method, status));
                assert_eq!(change(&mut c), Err(Error::Hw(method, err)), "case {i}, status {status:#x}");
                assert_eq!(c.settings, settings, "case {i}, status {status:#x}");
                assert_eq!(*c.hw.replies.borrow(), firmware, "case {i}, status {status:#x}");
            }
            let mut c = ctl();
            c.settings = c.read_state();
            let settings = c.settings.clone();
            *c.hw.wmi_down.borrow_mut() = true;
            assert!(matches!(change(&mut c), Err(Error::Wmi(_))), "case {i}");
            assert_eq!(c.settings, settings, "case {i}");
        }
    }

    #[test]
    fn turbo_rejected_midway_leaves_firmware_as_it_was() {
        let mut c = ctl();
        let firmware = c.hw.replies.borrow().clone();
        *c.hw.fail.borrow_mut() = Some((SetGamingFanBehavior, 0x2));
        assert!(c.set_turbo(true).is_err());
        // LED and turbo flag were switched on, then back off; the GPU level was never raised.
        assert_eq!(*c.hw.replies.borrow(), firmware);
        assert!(!c.hw.sets().iter().any(|&(m, i)| m == SetGamingMiscSetting && i & 0xFF == 5));
    }

    #[test]
    fn keyboard_change_refused_at_a_zone_is_rolled_back() {
        let mut c = ctl();
        c.settings.restore_kb_lighting = false;
        let before = c.settings.clone();
        c.hw.replies.borrow_mut().insert((SetGamingRgbKb, 0xC7AE_0002), 0x2);
        let kb = KbBacklight { brightness: 50, ..KbBacklight::default() };
        assert_eq!(c.set_kb(kb), Err(Error::Hw(SetGamingRgbKb, FirmwareError::InvalidParam)));
        assert_eq!(c.settings, before);
        // The rollback sent the old payload again.
        assert_eq!(c.hw.byte_calls.borrow().last().unwrap()[2], 100);
    }

    #[test]
    fn power_gate_boundaries_and_repeats() {
        let mut g = PowerGate::default();
        // (on AC, battery, changed, allowed afterwards)
        let steps = [
            (true, Some(30), false, true),
            (true, Some(30), false, true),
            (true, Some(29), true, false),
            (true, Some(29), false, false),
            (true, Some(30), false, false),
            (true, Some(39), false, false),
            (true, Some(40), true, true),
            (true, Some(40), false, true),
            (false, Some(100), true, false),
            (false, Some(100), false, false),
            (false, None, false, false),
            (true, Some(100), true, true),
            (true, Some(0), true, false),
            (true, None, true, true),
            (false, None, true, false),
            (true, Some(40), true, true),
        ];
        for (i, (on_ac, battery, changed, allowed)) in steps.into_iter().enumerate() {
            assert_eq!(g.update(on_ac, battery), changed, "step {i}");
            assert_eq!(g.allowed(), allowed, "step {i}");
        }
    }

    #[test]
    fn turbo_kept_down_to_30_percent_and_not_resumed_by_charging() {
        let mut c = ctl();
        c.set_turbo(true).unwrap();
        assert_eq!(c.on_power(true, Some(30)), Ok(None));
        assert!(c.settings.turbo);
        assert_eq!(c.on_power(true, Some(29)), Ok(Some("Turbo stopped: on battery or low battery")));
        assert!(!c.settings.turbo);
        assert_eq!(c.set_turbo(true), Err(Error::NeedsAc));
        assert_eq!(c.on_power(true, Some(39)), Ok(None));
        c.hw.take();
        assert_eq!(c.on_power(true, Some(40)), Ok(Some("Overclocking resumed")));
        assert!(!c.settings.turbo);
        assert_eq!(c.hw.sets(), []);
        assert_eq!(c.set_turbo(true), Ok(()));
    }

    #[test]
    fn battery_change_during_eco_keeps_eco_and_its_limits() {
        let (mut c, calls) = eco_ctl();
        c.set_gpu_oc(GpuOc::Faster).unwrap();
        c.set_eco(true).unwrap();
        c.hw.take();
        assert_eq!(c.on_power(false, Some(80)), Ok(Some("Overclocking paused: on battery or low battery")));
        assert_eq!(c.on_power(true, Some(80)), Ok(Some("Overclocking resumed")));
        assert!(c.settings.eco);
        assert_eq!(c.settings.gpu_oc, GpuOc::Faster);
        // The GPU stays Normal under Eco either way; fans and limits are untouched.
        assert_eq!(c.hw.sets(), []);
        assert_eq!(*calls.borrow(), vec![(true, 1000)]);
    }

    #[test]
    fn battery_change_during_balanced_pauses_only_the_gpu() {
        let mut c = ctl();
        c.set_gpu_oc(GpuOc::Faster).unwrap();
        c.set_balanced(true).unwrap();
        c.hw.take();
        c.on_power(false, Some(80)).unwrap();
        assert_eq!(c.hw.sets(), vec![(SetGamingMiscSetting, 0x005)]);
        assert!(c.settings.balanced);
        c.on_power(true, Some(80)).unwrap();
        assert_eq!(c.hw.sets(), vec![(SetGamingMiscSetting, 0x105)]);
    }

    #[test]
    fn turbo_button_on_battery_keeps_eco() {
        let (mut c, calls) = eco_ctl();
        c.set_eco(true).unwrap();
        c.on_power(false, Some(80)).unwrap();
        c.hw.take();
        // The button flips the firmware flag and GPU level anyway.
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x100);
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 5), 0x2200);
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::RefusedOnBattery));
        assert!(c.settings.eco && !c.settings.turbo);
        let sets = c.hw.sets();
        assert!(sets.contains(&(SetGamingMiscSetting, 0x001)) && sets.contains(&(SetGamingMiscSetting, 0x005)));
        assert!(sets.contains(&(SetGamingFanBehavior, ALL_CUSTOM)));
        assert_eq!(calls.borrow().len(), 1);
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::Unchanged));
    }

    #[test]
    fn unreadable_turbo_flag_leaves_the_active_mode_alone() {
        let (mut c, calls) = eco_ctl();
        c.set_eco(true).unwrap();
        c.hw.take();
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x2);
        assert_eq!(c.poll_turbo_button(), Err(Error::Hw(GetGamingMiscSetting, FirmwareError::InvalidParam)));
        *c.hw.wmi_down.borrow_mut() = true;
        assert!(matches!(c.poll_turbo_button(), Err(Error::Wmi(_))));
        *c.hw.wmi_down.borrow_mut() = false;
        assert!(c.settings.eco && !c.settings.turbo);
        assert_eq!(calls.borrow().len(), 1);
        assert_eq!(c.hw.sets(), []);
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Mode {
        Turbo,
        Balanced,
        Eco,
    }

    fn set_mode(c: &mut Controller<Mock>, m: Mode, on: bool) -> Result<()> {
        match m {
            Mode::Turbo => c.set_turbo(on),
            Mode::Balanced => c.set_balanced(on),
            Mode::Eco => c.set_eco(on).map(drop),
        }
    }

    /// Turbo LED, turbo flag and GPU level bits the firmware holds.
    fn turbo_firmware(c: &Controller<Mock>) -> [u64; 3] {
        let r = c.hw.replies.borrow();
        [r[&(GetGamingLed, 1)], r[&(GetGamingMiscSetting, 1)], r[&(GetGamingMiscSetting, 5)] & 0xF00]
    }

    #[test]
    fn every_mode_switch_leaves_exactly_one_mode_on() {
        use Mode::*;
        let last_fans =
            |sets: &[(Method, u64)]| sets.iter().rev().find(|(m, _)| *m == SetGamingFanBehavior).map(|s| s.1);
        for from in [Turbo, Balanced, Eco] {
            for to in [Turbo, Balanced, Eco] {
                let case = format!("{from:?} -> {to:?}");
                let (mut c, calls) = eco_ctl();
                let limits_on = || calls.borrow().last().is_some_and(|&(on, _)| on);
                set_mode(&mut c, from, true).unwrap();
                set_mode(&mut c, to, true).unwrap();
                let modes = |c: &Controller<Mock>| [c.settings.turbo, c.settings.balanced, c.settings.eco];
                assert_eq!(modes(&c), [to == Turbo, to == Balanced, to == Eco], "{case}");
                assert_eq!(limits_on(), to == Eco, "{case}");
                assert_eq!(last_fans(&c.hw.sets()), Some(if to == Turbo { ALL_MAX } else { ALL_CUSTOM }), "{case}");
                let turbo = if to == Turbo { [0x100, 0x100, 0x200] } else { [0; 3] };
                assert_eq!(turbo_firmware(&c), turbo, "{case}");
                // Off again: back to the user's own fans and GPU level.
                set_mode(&mut c, to, false).unwrap();
                assert_eq!(modes(&c), [false; 3], "{case}");
                assert!(!limits_on(), "{case}");
                assert_eq!(last_fans(&c.hw.sets()), Some(ALL_AUTO), "{case}");
                assert_eq!(turbo_firmware(&c), [0; 3], "{case}");
            }
        }
    }

    #[test]
    fn turning_an_inactive_mode_off_does_not_end_the_active_one() {
        let (mut c, calls) = eco_ctl();
        c.set_eco(true).unwrap();
        c.hw.take();
        c.set_turbo(false).unwrap();
        c.set_balanced(false).unwrap();
        assert!(c.settings.eco);
        assert_eq!(calls.borrow().len(), 1);
        assert_eq!(c.hw.take(), []);
    }

    #[test]
    fn eco_stays_when_windows_refuses_to_lift_its_limits() {
        let (mut c, _) = eco_ctl();
        c.set_eco(true).unwrap();
        let calls = Calls::default();
        c.limiter = Box::new(RecLimiter { calls: calls.clone(), fail: true, note: None });
        let (settings, tracks) = (c.settings.clone(), c.tracks);
        c.hw.take();
        assert!(matches!(c.set_eco(false), Err(Error::Os(_))));
        assert!(matches!(c.toggle_eco(), Err(Error::Os(_))));
        assert!(matches!(c.set_turbo(true), Err(Error::Os(_))));
        assert!(matches!(c.set_balanced(true), Err(Error::Os(_))));
        assert!(matches!(c.set_fan_mode(FanGroup::Cpu, FanMode::Max), Err(Error::Os(_))));
        assert!(matches!(c.set_gpu_oc(GpuOc::Faster), Err(Error::Os(_))));
        assert_eq!(c.settings, settings);
        assert_eq!(c.tracks, tracks);
        assert_eq!(c.hw.sets(), []);
        assert_eq!(*calls.borrow(), vec![(false, 1000); 6]);
    }

    #[test]
    fn firmware_failure_while_leaving_eco_puts_its_limits_back() {
        type Leave = fn(&mut Controller<Mock>) -> Result<()>;
        let cases: [Leave; 4] = [
            |c| c.set_eco(false).map(drop),
            |c| c.set_turbo(true),
            |c| c.set_balanced(true),
            |c| c.set_fan_mode(FanGroup::Cpu, FanMode::Max),
        ];
        for (i, leave) in cases.into_iter().enumerate() {
            let (mut c, calls) = eco_ctl();
            c.set_eco(true).unwrap();
            let (settings, tracks) = (c.settings.clone(), c.tracks);
            *c.hw.fail.borrow_mut() = Some((SetGamingFanBehavior, 0x2));
            assert_eq!(leave(&mut c), Err(Error::Hw(SetGamingFanBehavior, FirmwareError::InvalidParam)), "case {i}");
            assert_eq!(c.settings, settings, "case {i}");
            assert_eq!(c.tracks, tracks, "case {i}");
            assert_eq!(*calls.borrow(), vec![(true, 1000), (false, 1000), (true, 1000)], "case {i}");
        }
    }

    #[test]
    fn gpu_level_choice_leaves_turbo_and_balanced() {
        let mut c = ctl();
        c.set_turbo(true).unwrap();
        c.hw.take();
        c.set_gpu_oc(GpuOc::Faster).unwrap();
        assert!(!c.settings.turbo);
        let sets = c.hw.sets();
        assert_eq!(sets.first(), Some(&(SetGamingLed, TURBO_LED_OFF)));
        assert_eq!(sets.last(), Some(&(SetGamingMiscSetting, 0x105)));
        c.set_balanced(true).unwrap();
        c.hw.take();
        // Even the level it already has.
        c.set_gpu_oc(GpuOc::Faster).unwrap();
        assert!(!c.settings.balanced);
        assert_eq!(c.hw.sets(), vec![(SetGamingFanBehavior, ALL_AUTO)]);
    }

    #[test]
    fn fan_percentage_during_a_curve_mode_is_stored_not_sent() {
        let mut c = ctl();
        c.settings.fan_mode[0] = FanMode::Custom;
        c.set_balanced(true).unwrap();
        c.hw.take();
        c.set_fan_pct(FanGroup::Cpu, 90).unwrap();
        assert!(c.settings.balanced);
        assert_eq!(c.settings.fan_pct[0], 90);
        let user = (SetGamingFanSpeed, fan_speed_input(0, 90).unwrap());
        assert!(!speeds(&c).contains(&user));
        c.set_balanced(false).unwrap();
        assert!(speeds(&c).contains(&user));
    }

    #[test]
    fn curve_starts_fans_exactly_at_the_start_temperature() {
        let mut c = ctl();
        temp(&c, 60);
        gpu_temp(&c, 60);
        c.set_balanced(true).unwrap();
        assert_eq!(c.tracks.map(|t| t.pct()), [10, 10]);
        let want: Vec<_> = [0, 3, 4].map(|slot| (SetGamingFanSpeed, fan_speed_input(slot, 10).unwrap())).into();
        assert_eq!(speeds(&c), want);
        let mut c = ctl();
        temp(&c, 59);
        gpu_temp(&c, 59);
        c.set_balanced(true).unwrap();
        assert_eq!(c.tracks.map(|t| t.pct()), [0, 0]);
    }

    #[test]
    fn curve_stops_fans_only_below_the_hysteresis_edge() {
        let mut c = ctl();
        temp(&c, 60);
        gpu_temp(&c, 40);
        c.set_balanced(true).unwrap();
        c.hw.take();
        // Start minus `STOP_HYSTERESIS`: the smoothed temperature never gets below it.
        temp(&c, 57);
        for _ in 0..30 {
            assert_eq!(c.curve_tick(), Ok(None));
        }
        assert_eq!(c.tracks[0].pct(), 10);
        assert_eq!(c.hw.sets(), []);
        temp(&c, 56);
        for _ in 0..10 {
            c.curve_tick().unwrap();
        }
        assert_eq!(c.tracks[0].pct(), 0);
        assert_eq!(speeds(&c), vec![(SetGamingFanSpeed, 0x0001)]);
    }

    #[test]
    fn curve_reacts_at_once_from_the_fast_threshold() {
        let mut c = ctl();
        temp(&c, 50);
        gpu_temp(&c, 50);
        c.set_balanced(true).unwrap();
        c.hw.take();
        // Just below: smoothed, still under the start temperature.
        temp(&c, 89);
        assert_eq!(c.curve_tick(), Ok(None));
        temp(&c, 90);
        assert!(c.curve_tick().unwrap().is_some());
        assert_eq!(c.tracks[0].pct(), 85);
        // Hottest plausible reading.
        temp(&c, 125);
        c.curve_tick().unwrap();
        assert_eq!(
            speeds(&c),
            vec![
                (SetGamingFanSpeed, fan_speed_input(0, 85).unwrap()),
                (SetGamingFanSpeed, fan_speed_input(0, 100).unwrap())
            ]
        );
        temp(&c, 126);
        c.curve_tick().unwrap();
        assert!(c.curve_fell_back_to_auto);
    }

    #[test]
    fn one_unreadable_group_sends_all_fans_to_auto() {
        let mut c = ctl();
        c.set_balanced(true).unwrap();
        c.hw.take();
        c.hw.replies.borrow_mut().insert((GetGamingSysInfo, 0xA01), 0x1);
        assert_eq!(c.curve_tick(), Ok(Some("Fans on Auto (temperature unreadable)".into())));
        assert_eq!(c.hw.sets(), vec![(SetGamingFanBehavior, ALL_AUTO)]);
        assert_eq!(c.effective_fan_mode(FanGroup::Cpu), FanMode::Auto);
    }

    #[test]
    fn curve_resumes_all_fans_when_both_temperatures_return() {
        let mut c = ctl();
        c.set_balanced(true).unwrap();
        c.hw.take();
        temp(&c, 0);
        gpu_temp(&c, 0);
        c.curve_tick().unwrap();
        for _ in 0..3 {
            assert_eq!(c.curve_tick(), Ok(None));
        }
        assert_eq!(c.hw.sets(), vec![(SetGamingFanBehavior, ALL_AUTO)]);
        temp(&c, 80);
        gpu_temp(&c, 80);
        assert!(c.curve_tick().unwrap().is_some());
        let sets = c.hw.sets();
        assert_eq!(sets[0], (SetGamingFanBehavior, ALL_CUSTOM));
        assert_eq!(sets[1..].iter().filter(|(m, _)| *m == SetGamingFanSpeed).count(), 3);
        assert!(!c.curve_fell_back_to_auto);
    }

    #[test]
    fn curve_mode_started_without_temperatures_runs_on_auto() {
        let mut c = ctl();
        temp(&c, 0);
        c.set_balanced(true).unwrap();
        assert!(c.settings.balanced && c.curve_fell_back_to_auto);
        assert_eq!(c.hw.sets(), vec![(SetGamingFanBehavior, ALL_AUTO)]);
        assert_eq!(c.curve_status(), "Fans on Auto (temperature unreadable)");
        temp(&c, 70);
        assert!(c.curve_tick().unwrap().is_some());
        assert_eq!(c.hw.sets()[0], (SetGamingFanBehavior, ALL_CUSTOM));
    }

    #[test]
    fn curve_tick_without_a_curve_mode_reads_nothing() {
        let mut c = ctl();
        assert_eq!(c.curve_tick(), Ok(None));
        c.set_turbo(true).unwrap();
        c.hw.take();
        assert_eq!(c.curve_tick(), Ok(None));
        assert_eq!(c.hw.take(), []);
    }

    #[test]
    fn read_state_with_dead_wmi_gives_safe_defaults() {
        let mut c = ctl();
        c.settings.usb_charge = 20;
        let settings = c.settings.clone();
        *c.hw.wmi_down.borrow_mut() = true;
        assert_eq!(c.read_state(), Settings { restore_kb_lighting: false, ..Settings::default() });
        assert!(!c.sync_firmware_owned());
        assert_eq!(c.settings, settings);
    }

    #[test]
    fn read_state_keeps_defaults_for_single_failed_reads() {
        let c = ctl();
        {
            let mut r = c.hw.replies.borrow_mut();
            // First GPU fan does not answer: the second (Max) stands for the group.
            r.insert((GetGamingFanBehavior, 0x8), 0x1);
            r.insert((GetGamingFanBehavior, 0x10), 0x2_0000);
            r.insert((GetGamingRgbKb, 0x1), 0xE1);
            r.insert((GetGamingRgbKb, 0x2), 0xFF00);
            r.insert((GetGamingMiscSetting, 5), 0x1);
            r.insert((ApgeGetFunction, USB_CHARGE_GET), 0xE2);
            // Flag unreadable: not Turbo, even with the LED on.
            r.insert((GetGamingMiscSetting, 1), 0x1);
            r.insert((GetGamingLed, 1), 0x100);
        }
        let s = c.read_state();
        assert!(!s.turbo);
        assert_eq!(s.fan_mode, [FanMode::Auto, FanMode::Max]);
        assert_eq!(s.zones[..3], [Rgb::ACER_CYAN, Rgb::new(0xFF, 0, 0), Rgb::ACER_CYAN]);
        assert_eq!((s.gpu_oc, s.usb_charge), (GpuOc::Normal, 0));
        assert!(s.lcd_overdrive && !s.restore_kb_lighting);
    }

    #[test]
    fn apply_nothing_makes_no_calls() {
        let mut c = ctl();
        c.settings.turbo = true;
        c.settings.coolboost = true;
        c.apply(0).unwrap();
        assert_eq!(c.hw.take(), []);
    }

    #[test]
    fn restore_stops_at_the_first_refused_part() {
        let mut c = ctl();
        c.settings.turbo = true;
        c.settings.coolboost = true;
        *c.hw.fail.borrow_mut() = Some((SetGamingFanBehavior, 0x1));
        assert_eq!(c.restore(), Err(Error::Hw(SetGamingFanBehavior, FirmwareError::Unsupported)));
        assert_eq!(
            c.hw.sets(),
            vec![(SetGamingLed, TURBO_LED_ON), (SetGamingMiscSetting, 0x101), (SetGamingFanBehavior, ALL_MAX)]
        );
    }

    #[test]
    fn restore_completes_turbo_without_touching_a_flag_already_on() {
        let mut c = ctl();
        c.settings.turbo = true;
        c.hw.replies.borrow_mut().insert((GetGamingLed, 1), 0x100);
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x100);
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 5), 0x2200);
        c.restore().unwrap();
        let sets = c.hw.sets();
        assert_eq!(sets[0], (SetGamingFanBehavior, ALL_MAX));
        assert!(!sets.iter().any(|(m, _)| matches!(m, SetGamingLed | SetGamingMiscSetting)), "{sets:?}");
    }

    #[test]
    fn exit_under_turbo_touches_nothing_and_refused_limits_still_free_the_fans() {
        let mut c = ctl();
        c.set_turbo(true).unwrap();
        c.hw.take();
        c.release_modes_on_exit();
        assert!(c.settings.turbo);
        assert_eq!(c.hw.take(), []);
        let (mut c, _) = eco_ctl();
        c.set_eco(true).unwrap();
        c.limiter = Box::new(RecLimiter { calls: Calls::default(), fail: true, note: None });
        c.hw.take();
        c.release_modes_on_exit();
        assert_eq!(c.hw.sets(), vec![(SetGamingFanBehavior, ALL_AUTO)]);
    }

    // Suspected bugs: state is committed before a write that can fail, and the next call compares
    // against that state, so the failed write is never retried.

    #[test]
    fn failed_fall_back_to_auto_is_retried() {
        let mut c = ctl();
        temp(&c, 50);
        gpu_temp(&c, 48);
        c.set_balanced(true).unwrap();
        temp(&c, 0);
        *c.hw.fail.borrow_mut() = Some((SetGamingFanBehavior, 0x2));
        assert!(c.curve_tick().is_err());
        *c.hw.fail.borrow_mut() = None;
        c.hw.take();
        c.curve_tick().unwrap();
        assert_eq!(c.hw.sets(), vec![(SetGamingFanBehavior, ALL_AUTO)]);
    }

    #[test]
    fn failed_return_from_auto_is_retried() {
        let mut c = ctl();
        c.set_balanced(true).unwrap();
        temp(&c, 0);
        c.curve_tick().unwrap();
        temp(&c, 80);
        *c.hw.fail.borrow_mut() = Some((SetGamingFanBehavior, 0x2));
        assert!(c.curve_tick().is_err());
        *c.hw.fail.borrow_mut() = None;
        c.hw.take();
        c.curve_tick().unwrap();
        assert!(c.hw.sets().contains(&(SetGamingFanBehavior, ALL_CUSTOM)));
    }

    #[test]
    fn failed_curve_speed_write_is_retried() {
        let mut c = ctl();
        temp(&c, 50);
        gpu_temp(&c, 48);
        c.set_balanced(true).unwrap();
        temp(&c, 95);
        *c.hw.fail.borrow_mut() = Some((SetGamingFanSpeed, 0x2));
        assert!(c.curve_tick().is_err());
        *c.hw.fail.borrow_mut() = None;
        c.hw.take();
        c.curve_tick().unwrap();
        assert_eq!(speeds(&c), vec![(SetGamingFanSpeed, fan_speed_input(0, 100).unwrap())]);
    }

    #[test]
    fn failed_turbo_button_completion_is_retried() {
        let mut c = ctl();
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x100);
        *c.hw.fail.borrow_mut() = Some((SetGamingFanBehavior, 0x2));
        assert!(c.poll_turbo_button().is_err());
        *c.hw.fail.borrow_mut() = None;
        c.hw.take();
        c.poll_turbo_button().unwrap();
        assert!(c.hw.sets().contains(&(SetGamingFanBehavior, ALL_MAX)));
    }

    #[test]
    fn turbo_button_with_stuck_eco_limits_keeps_modes_exclusive() {
        let (mut c, _) = eco_ctl();
        c.set_eco(true).unwrap();
        c.limiter = Box::new(RecLimiter { calls: Calls::default(), fail: true, note: None });
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x100);
        assert!(c.poll_turbo_button().is_err());
        assert!(!(c.settings.turbo && c.settings.eco), "Turbo and Eco both on");
    }

    #[test]
    fn failed_turbo_button_undo_on_battery_is_retried() {
        let mut c = ctl();
        c.on_power(false, Some(80)).unwrap();
        c.hw.replies.borrow_mut().insert((GetGamingMiscSetting, 1), 0x100);
        *c.hw.fail.borrow_mut() = Some((SetGamingMiscSetting, 0x2));
        assert!(c.poll_turbo_button().is_err());
        assert!(!c.settings.turbo);
        *c.hw.fail.borrow_mut() = None;
        c.hw.take();
        assert_eq!(c.poll_turbo_button(), Ok(TurboButton::RefusedOnBattery));
        assert!(c.hw.sets().contains(&(SetGamingMiscSetting, 0x001)));
    }

    #[test]
    fn failed_power_change_write_is_retried() {
        let mut c = ctl();
        c.set_gpu_oc(GpuOc::Faster).unwrap();
        *c.hw.fail.borrow_mut() = Some((SetGamingMiscSetting, 0x2));
        assert!(c.on_power(false, Some(80)).is_err());
        *c.hw.fail.borrow_mut() = None;
        c.hw.take();
        c.on_power(false, Some(80)).unwrap();
        assert_eq!(c.hw.sets(), vec![(SetGamingMiscSetting, 0x005)]);
    }
}
