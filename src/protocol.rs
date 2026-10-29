//! Bit-level encoding of Acer's BIOS WMI interface: the `ROOT\WMI` classes
//! `AcerGamingFunction` (fans, sensors, Turbo, lighting, misc settings) and `APGeAction`
//! (CoolBoost, keyboard backlight timeout, power-off USB charging).
//!
//! Derived from PredatorSense v3.0.3136 (PSSvc.exe / PSAdminAgent.exe) and checked
//! on a Predator Triton 500 (PT515-51). Almost every method takes one packed integer
//! (`gmInput` / `uiInput`) and returns one (`gmOutput` / `uiOutput`) whose low byte is a
//! status code; see `check`. This module is pure: it only builds and decodes the integers,
//! `wmi` sends them and `controller` decides what to send.

use std::fmt;
use std::ops::RangeInclusive;

/// A firmware method. `name` and `class` give its WMI identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    SetGamingProfile,
    GetGamingProfile,
    SetGamingLed,
    GetGamingLed,
    GetGamingSysInfo,
    SetGamingRgbKb,
    GetGamingRgbKb,
    SetGamingFanBehavior,
    GetGamingFanBehavior,
    SetGamingFanSpeed,
    GetGamingFanSpeed,
    SetGamingKbBacklight,
    GetGamingKbBacklight,
    SetGamingMiscSetting,
    GetGamingMiscSetting,
    /// `APGeAction.GetFunction` (CoolBoost, keyboard backlight timeout, ...).
    ApgeGetFunction,
    /// `APGeAction.SetFunction`.
    ApgeSetFunction,
}

impl Method {
    /// Number of variants (size of per-method caches).
    pub const COUNT: usize = 17;

    /// WMI method name, exactly as the class declares it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Method::SetGamingProfile => "SetGamingProfile",
            Method::GetGamingProfile => "GetGamingProfile",
            Method::SetGamingLed => "SetGamingLED",
            Method::GetGamingLed => "GetGamingLED",
            Method::GetGamingSysInfo => "GetGamingSysInfo",
            Method::SetGamingRgbKb => "SetGamingRgbKb",
            Method::GetGamingRgbKb => "GetGamingRgbKb",
            Method::SetGamingFanBehavior => "SetGamingFanBehavior",
            Method::GetGamingFanBehavior => "GetGamingFanBehavior",
            Method::SetGamingFanSpeed => "SetGamingFanSpeed",
            Method::GetGamingFanSpeed => "GetGamingFanSpeed",
            Method::SetGamingKbBacklight => "SetGamingKBBacklight",
            Method::GetGamingKbBacklight => "GetGamingKBBacklight",
            Method::SetGamingMiscSetting => "SetGamingMiscSetting",
            Method::GetGamingMiscSetting => "GetGamingMiscSetting",
            Method::ApgeGetFunction => "GetFunction",
            Method::ApgeSetFunction => "SetFunction",
        }
    }

    /// WMI class (ROOT\WMI) that implements the method.
    #[must_use]
    pub const fn class(self) -> &'static str {
        match self {
            Method::ApgeGetFunction | Method::ApgeSetFunction => "APGeAction",
            _ => "AcerGamingFunction",
        }
    }

    /// Names of the input and output parameters.
    #[must_use]
    pub const fn params(self) -> (&'static str, &'static str) {
        match self {
            Method::ApgeGetFunction | Method::ApgeSetFunction => ("uiInput", "uiOutput"),
            _ => ("gmInput", "gmOutput"),
        }
    }

    /// CIM type of the input (`gmInput` / `uiInput`): uint64 for most setters, uint32 for getters
    /// and `SetGamingRgbKb`.
    #[must_use]
    pub const fn input_is_u64(self) -> bool {
        matches!(
            self,
            Method::SetGamingProfile
                | Method::ApgeSetFunction
                | Method::SetGamingLed
                | Method::SetGamingFanBehavior
                | Method::SetGamingFanSpeed
                | Method::SetGamingMiscSetting
        )
    }

    /// Position in `0..COUNT`, for array-backed caches.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Non-zero status byte returned by the firmware. `APGeAction` methods also use their own codes
/// (e.g. 0xE2), which end up in `Status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirmwareError {
    /// Status 1: the laptop does not have this function.
    Unsupported,
    /// Status 2: the function exists but refused this input.
    InvalidParam,
    /// Any other status code.
    Status(u8),
}

impl fmt::Display for FirmwareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FirmwareError::Unsupported => f.write_str("not supported by this model"),
            FirmwareError::InvalidParam => f.write_str("invalid parameter"),
            FirmwareError::Status(s) => write!(f, "firmware status {s:#04x}"),
        }
    }
}

/// Byte `n` of a firmware value, counting from the least significant byte.
#[must_use]
pub const fn byte(value: u64, n: usize) -> u8 {
    value.to_le_bytes()[n]
}

/// `x` rounded to the nearest integer and clamped to `0..=255`.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped to the u8 range first"
)]
pub fn round_to_u8(x: f32) -> u8 {
    x.round().clamp(0.0, 255.0) as u8
}

/// Split a method's output (`gmOutput` / `uiOutput`) into status and payload (`output >> 8`).
///
/// # Errors
/// `Unsupported` (status 1), `InvalidParam` (status 2) or `Status` for any other non-zero status
/// byte.
pub fn check(output: u64) -> Result<u64, FirmwareError> {
    match byte(output, 0) {
        0 => Ok(output >> 8),
        1 => Err(FirmwareError::Unsupported),
        2 => Err(FirmwareError::InvalidParam),
        s => Err(FirmwareError::Status(s)),
    }
}

// ---------------------------------------------------------------- sensors

/// Temperatures outside this range (°C) are not real readings.
pub const VALID_TEMP_C: RangeInclusive<u16> = 1..=125;
/// Fan speeds above this (RPM) are not real readings.
pub const MAX_FAN_RPM: u16 = 12000;

/// A `GetGamingSysInfo` sensor; the discriminant is its sensor id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sensor {
    CpuTemp = 0x01,
    CpuFan = 0x02,
    SysTemp = 0x03,
    GpuFan = 0x06,
    Fan3 = 0x09,
    GpuTemp = 0x0A,
}

impl Sensor {
    /// Every known sensor, in display order (temperatures first).
    pub const ALL: [Sensor; 6] =
        [Sensor::CpuTemp, Sensor::GpuTemp, Sensor::SysTemp, Sensor::CpuFan, Sensor::GpuFan, Sensor::Fan3];

    /// `GetGamingSysInfo` input: sensor id in bits 8..16, command 1 (read) in bits 0..8.
    #[must_use]
    pub const fn input(self) -> u64 {
        ((self as u64) << 8) | 1
    }

    /// Temperature (°C) rather than fan speed (RPM).
    #[must_use]
    pub const fn is_temp(self) -> bool {
        matches!(self, Sensor::CpuTemp | Sensor::SysTemp | Sensor::GpuTemp)
    }

    /// `GetGamingSysInfo` input of the fan's duty cycle in percent, a separate sensor id that
    /// PredatorSense reads next to each RPM (`MainWindow.get_fan_info_data`, `data_Rate_pool`).
    #[must_use]
    pub const fn duty_input(self) -> Option<u64> {
        let id = match self {
            Sensor::CpuFan => 0x0C,
            Sensor::GpuFan => 0x10,
            Sensor::Fan3 => 0x11,
            _ => return None,
        };
        Some((id << 8) | 1)
    }

    /// Name shown in the window and the status report.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Sensor::CpuTemp => "CPU",
            Sensor::GpuTemp => "GPU",
            Sensor::SysTemp => "System",
            Sensor::CpuFan => "CPU fan",
            Sensor::GpuFan => "GPU fan",
            Sensor::Fan3 => "Fan 3",
        }
    }
}

/// Reading is a 16-bit value (°C or RPM) right above the status byte.
///
/// # Errors
/// The firmware's status when it is not 0 (see `check`).
pub fn decode_sensor(output: u64) -> Result<u16, FirmwareError> {
    check(output).map(|v| (v & 0xFFFF) as u16)
}

/// Fan duty cycle, 0..=100 %.
///
/// # Errors
/// The firmware's status when it is not 0 (see `check`).
pub fn decode_duty(output: u64) -> Result<Option<u8>, FirmwareError> {
    decode_sensor(output).map(|v| u8::try_from(v).ok().filter(|&p| p <= 100))
}

/// Format a reading for display, rejecting values no real sensor produces. `duty` is shown after
/// a fan's RPM.
#[must_use]
pub fn format_sensor(s: Sensor, value: Option<u16>, duty: Option<u8>) -> String {
    match value {
        Some(v) if s.is_temp() && VALID_TEMP_C.contains(&v) => format!("{}: {} °C", s.label(), v),
        Some(v) if !s.is_temp() && v <= MAX_FAN_RPM => match duty {
            Some(d) => format!("{}: {} RPM, {}%", s.label(), v, d),
            None => format!("{}: {} RPM", s.label(), v),
        },
        _ => format!("{}: --", s.label()),
    }
}

// ---------------------------------------------------------------- fans

/// Fan behavior of one slot; the discriminant is the firmware's 2-bit code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FanMode {
    /// The firmware's own curve.
    Auto = 1,
    /// Full speed.
    Max = 2,
    /// Fixed speed set with `SetGamingFanSpeed`.
    Custom = 3,
}

impl FanMode {
    /// Decode the low 2 bits (0 = no fan in that slot).
    #[must_use]
    pub const fn from_bits(b: u64) -> Option<FanMode> {
        match b & 3 {
            1 => Some(FanMode::Auto),
            2 => Some(FanMode::Max),
            3 => Some(FanMode::Custom),
            _ => None,
        }
    }

    /// Parse the settings-file name (`as_str`).
    #[must_use]
    pub fn parse(s: &str) -> Option<FanMode> {
        match s {
            "auto" => Some(FanMode::Auto),
            "max" => Some(FanMode::Max),
            "custom" => Some(FanMode::Custom),
            _ => None,
        }
    }

    /// Lower-case name used in the settings file and status messages.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            FanMode::Auto => "auto",
            FanMode::Max => "max",
            FanMode::Custom => "custom",
        }
    }
}

/// A fan group as PredatorSense exposes it: one CPU group, one GPU group (which may hold two fans).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FanGroup {
    Cpu,
    Gpu,
}

impl FanGroup {
    /// Both groups, in `index` order.
    pub const ALL: [FanGroup; 2] = [FanGroup::Cpu, FanGroup::Gpu];

    /// Fan-behavior slots belonging to this group (slot n = bit n of the bitmap).
    #[must_use]
    pub const fn slots(self) -> &'static [u32] {
        match self {
            FanGroup::Cpu => &[0],
            FanGroup::Gpu => &[3, 4],
        }
    }

    /// Index into per-group arrays (`Settings::fan_mode`, `fan_pct`, ...): CPU 0, GPU 1.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Short name for the window and status messages.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            FanGroup::Cpu => "CPU",
            FanGroup::Gpu => "GPU",
        }
    }
}

/// Fan-behavior slots probed for a fan (slots `0..FAN_SLOTS`).
pub const FAN_SLOTS: u32 = 8;

/// `SetGamingFanBehavior` input: bitmap of slots in bits 0..16, 2-bit mode per slot at bit 16 + 2*slot.
/// `present` masks out slots this machine does not have.
#[must_use]
pub fn fan_behavior_input(items: &[(FanGroup, FanMode)], present: u32) -> u64 {
    let mut bitmap = 0u64;
    let mut modes = 0u64;
    for &(group, mode) in items {
        for &slot in group.slots() {
            if present & (1 << slot) == 0 {
                continue;
            }
            bitmap |= 1 << slot;
            modes |= (mode as u64) << (2 * slot);
        }
    }
    bitmap | (modes << 16)
}

/// GetGamingFanBehavior(1 << slot) -> mode of that slot, `None` when the slot has no fan.
///
/// # Errors
/// The firmware's status when it is not 0 (see `check`).
pub fn decode_fan_mode(slot: u32, output: u64) -> Result<Option<FanMode>, FirmwareError> {
    check(output).map(|v| FanMode::from_bits(v >> (2 * slot)))
}

/// `SetGamingFanSpeed` / `GetGamingFanSpeed` fan id: slot + 1. Probed on a PT515-51: ids 1 (CPU,
/// slot 0), 4 and 5 (the two GPU-side fans, slots 3 and 4) exist; every other id is unsupported.
#[must_use]
pub const fn fan_speed_id(slot: u32) -> u64 {
    slot as u64 + 1
}

/// Firmware Custom % -> duty % the fan really runs at (duty sensors, settled), measured on the
/// PT515-51 by stepping every slot through the Custom values. The firmware value is not a duty
/// cycle: the response is roughly quadratic, and the GPU fans run a few points lower than the CPU
/// fan. The GPU's 20 % point is estimated from the CPU fan's shape (the run stopped at 30 % on
/// temperature).
/// Points are `(firmware %, duty %)`.
const CPU_FAN_DUTY: [(u8, u8); 10] =
    [(0, 0), (20, 7), (30, 13), (40, 19), (50, 28), (60, 35), (70, 46), (80, 62), (90, 78), (100, 100)];
const GPU_FAN_DUTY: [(u8, u8); 10] =
    [(0, 0), (20, 6), (30, 11), (40, 16), (50, 22), (60, 31), (70, 41), (80, 55), (90, 74), (100, 100)];

/// Slot 0 is the CPU fan; every other slot is a GPU-side fan.
fn fan_duty_table(slot: u32) -> &'static [(u8, u8)] {
    if slot == 0 {
        &CPU_FAN_DUTY
    } else {
        &GPU_FAN_DUTY
    }
}

/// Linear interpolation through a table rising in both columns; `inverse` reads it right to left.
fn interpolate(table: &[(u8, u8)], x: u8, inverse: bool) -> u8 {
    let point = |p: (u8, u8)| if inverse { (p.1, p.0) } else { p };
    let mut prev = point(table[0]);
    for &p in &table[1..] {
        let (x1, y1) = point(p);
        if x <= x1 {
            let (x0, y0) = prev;
            let f = f32::from(x - x0) / f32::from(x1 - x0);
            return round_to_u8(f32::from(y0) + f * (f32::from(y1) - f32::from(y0)));
        }
        prev = (x1, y1);
    }
    prev.1
}

/// Firmware Custom % that makes the fan in `slot` run at `duty` %.
#[must_use]
pub fn firmware_fan_pct(slot: u32, duty: u8) -> u8 {
    interpolate(fan_duty_table(slot), duty.min(100), true)
}

/// Duty % the fan in `slot` runs at for a firmware Custom % (for converting old settings).
#[must_use]
pub fn fan_duty_of(slot: u32, firmware_pct: u8) -> u8 {
    interpolate(fan_duty_table(slot), firmware_pct.min(100), false)
}

/// `SetGamingFanSpeed` input: fan id in bits 0..8, firmware Custom % in bits 8..16. `duty` is the
/// duty cycle wanted, converted through the measured response.
#[must_use]
pub fn fan_speed_input(slot: u32, duty: u8) -> Option<u64> {
    (duty <= 100).then(|| fan_speed_id(slot) | (u64::from(firmware_fan_pct(slot, duty)) << 8))
}

// ---------------------------------------------------------------- turbo / misc settings

/// `SetGamingLED` input that turns the Turbo LED on (LED id 1, state in bits 16..).
pub const TURBO_LED_ON: u64 = 0x1_0001;
/// `SetGamingLED` input that turns the Turbo LED off.
pub const TURBO_LED_OFF: u64 = 0x0_0001;
/// `GetGamingLED` input for the Turbo LED.
pub const TURBO_LED_ID: u64 = 0x1;

/// GetGamingLED(1) -> Turbo LED state (bits 0..2 of payload, 1 = on).
///
/// # Errors
/// The firmware's status when it is not 0 (see `check`).
pub fn decode_turbo_led(output: u64) -> Result<bool, FirmwareError> {
    check(output).map(|v| v & 3 == 1)
}

/// Index of a Get/SetGamingMiscSetting value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MiscSetting {
    /// Firmware turbo flag toggled together with the Turbo LED.
    Turbo = 1,
    /// GPU mode (MUX): 1 hybrid, 2 discrete only; takes effect after a restart.
    GpuMode = 2,
    /// GPU overclock level (0 normal, 1 faster, 2 turbo).
    GpuOc = 5,
    /// Boot (POST) animation and sound, 1 = on (`CommonFunction.Set_Post_AnimationSound`;
    /// `PSAdminAgent` keeps it as `post_animation_support`).
    BootAnimation = 6,
}

/// Display MUX setting, as PredatorSense's "Discrete GPU only" checkbox
/// (`CommonFunction.Set_System_GPUMode` / `Get_GPUMode_Support`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuMode {
    /// Optimus: the Intel GPU drives the screen, NVIDIA renders on demand.
    Hybrid = 1,
    /// The NVIDIA GPU drives the screen directly; the Intel GPU is switched off.
    Discrete = 2,
}

impl GpuMode {
    /// Decode misc setting 2; anything but 1 or 2 means the switch is not supported.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<GpuMode> {
        match v {
            1 => Some(GpuMode::Hybrid),
            2 => Some(GpuMode::Discrete),
            _ => None,
        }
    }

    /// Name shown in the window.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            GpuMode::Hybrid => "Hybrid (Optimus)",
            GpuMode::Discrete => "Discrete only",
        }
    }
}

/// GPU mode the machine is running in, from the display adapters Windows sees: in Hybrid the
/// Intel GPU (PCI vendor 8086) is on; in Discrete only the firmware hides it.
pub fn running_gpu_mode<S: AsRef<str>>(adapter_ids: &[S]) -> GpuMode {
    if adapter_ids.iter().any(|id| id.as_ref().to_ascii_uppercase().starts_with("PCI\\VEN_8086")) {
        GpuMode::Hybrid
    } else {
        GpuMode::Discrete
    }
}

/// CoolBoost (`APGeAction` function 7): Acer's stronger Auto fan curve. PredatorSense reads it
/// with GetFunction(0x207) and writes it with SetFunction(7 | on << 16)
/// (`CommonFunction.get_coolboost_state` / `set_coolboost_state`).
pub const COOLBOOST_GET: u64 = 0x207;

/// `SetFunction` input switching CoolBoost on or off.
#[must_use]
pub const fn coolboost_set_input(on: bool) -> u64 {
    7 | ((on as u64) << 16)
}

/// GetFunction(0x207): status in byte 0, state in byte 1 (1 = on).
///
/// # Errors
/// The firmware's status when it is not 0 (see `check`).
pub fn decode_coolboost(output: u64) -> Result<bool, FirmwareError> {
    check(output).map(|v| v & 0xFF == 1)
}

/// Gaming-profile word (PredatorSense "ProfileConfiguration"): GetGamingProfile(0) returns status
/// in byte 0 and one setting per byte. `SetGamingProfile`'s byte 0 selects which bytes to write
/// (bit n = byte n + 2), so the rest of the word is left alone.
pub const PROFILE_GET: u64 = 0;

/// Win key (PredatorSense "Windows and menu key", 4-zone keyboards): byte 3, 1 = the Windows and
/// Menu keys work (`Advanced_Setting_CheckBox.SetWinMenuKey`; `PSAdminAgent` restores it at start).
#[must_use]
pub const fn win_key_set_input(on: bool) -> u64 {
    0x2 | ((on as u64) << 24)
}

/// `GetGamingProfile(PROFILE_GET)` -> Win key state (byte 3, true = keys work).
///
/// # Errors
/// The firmware's status when it is not 0 (see `check`).
pub fn decode_win_key(output: u64) -> Result<bool, FirmwareError> {
    check(output).map(|v| (v >> 16) & 0xFF == 1)
}

/// LCD overdrive: byte 6, 1 = on (`CommonFunction.set_lcd_over_drive` / `get_lcd_over_drive`).
#[must_use]
pub const fn lcd_overdrive_set_input(on: bool) -> u64 {
    0x10 | ((on as u64) << 48)
}

/// `GetGamingProfile(PROFILE_GET)` -> LCD overdrive state (byte 6).
///
/// # Errors
/// The firmware's status when it is not 0 (see `check`).
pub fn decode_lcd_overdrive(output: u64) -> Result<bool, FirmwareError> {
    check(output).map(|v| (v >> 40) & 0xFF == 1)
}

/// Keyboard backlight timeout (PredatorSense "Backlight" checkbox, 4-zone keyboards): `APGeAction`
/// with 0x01 (read) or 0x02 (write) in byte 0, 0x84 in byte 1 (`BK_Hotkey_Number` on Acer's
/// factory image; the only value the PT515-51 accepts) and 0x08 in byte 2. Get returns brightness
/// in byte 4 and the idle seconds before the backlight goes off in byte 5 (0 = never). Set sends
/// both back; PredatorSense uses 30 and 0.
pub const KB_TIMEOUT_GET: u64 = 0x8_8401;

/// Timeouts offered (seconds); 120 is what the PT515-51 firmware held out of the box.
pub const KB_TIMEOUTS: [u8; 4] = [0, 30, 60, 120];

/// `SetFunction` input for the backlight timeout: brightness in byte 4, seconds in byte 5.
#[must_use]
pub const fn kb_timeout_set_input(brightness: u8, secs: u8) -> u64 {
    0x8_8402 | ((brightness as u64) << 32) | ((secs as u64) << 40)
}

/// `GetFunction(KB_TIMEOUT_GET)` -> (brightness, timeout seconds).
///
/// # Errors
/// The firmware's status when it is not 0 (see `check`).
pub fn decode_kb_timeout(output: u64) -> Result<(u8, u8), FirmwareError> {
    check(output).map(|v| (byte(v, 3), byte(v, 4)))
}

/// Power-off USB charging (Acer Care Center's setting; same layout in opensense and
/// Acer-Predator-Scripts, checked with writes on the PT515-51): `APGeAction` function 4. The USB ports
/// stay powered while the laptop is off until the battery falls to the floor. Get returns the
/// state in byte 1 (0x0F on, 0x1F off) and the floor in byte 2. Byte 1 of the get input is
/// ignored: 0x004, 0x104 and 0x204 return the same answer.
pub const USB_CHARGE_GET: u64 = 0x204;

/// Choices: 0 = off, otherwise the battery floor in percent.
pub const USB_CHARGE_FLOORS: [u8; 4] = [0, 10, 20, 30];

/// SetFunction(0x04 | state << 8 | floor << 16); off is sent with floor 10, as the other tools do.
#[must_use]
pub const fn usb_charge_set_input(floor: u8) -> u64 {
    if floor == 0 {
        0x04 | (0x1F << 8) | (10 << 16)
    } else {
        0x04 | (0x0F << 8) | ((floor as u64) << 16)
    }
}

/// -> 0 when off, else the floor in percent.
///
/// # Errors
/// The firmware's status when it is not 0 (see `check`).
pub fn decode_usb_charge(output: u64) -> Result<u8, FirmwareError> {
    check(output).map(|v| if byte(v, 0) == 0x0F { byte(v, 1) } else { 0 })
}

/// Text for a `USB_CHARGE_FLOORS` choice.
#[must_use]
pub fn usb_charge_label(floor: u8) -> String {
    if floor == 0 {
        "Off".into()
    } else {
        format!("Until {floor}%")
    }
}

/// Text for a `KB_TIMEOUTS` choice.
#[must_use]
pub fn kb_timeout_label(secs: u8) -> String {
    match secs {
        0 => "Never".into(),
        s if s % 60 == 0 => format!("{} min", s / 60),
        s => format!("{s} s"),
    }
}

/// `SetGamingMiscSetting` input: index in bits 0..8, value in bits 8..16.
#[must_use]
pub const fn misc_input(idx: MiscSetting, value: u8) -> u64 {
    (idx as u64) | ((value as u64) << 8)
}

/// GetGamingMiscSetting(index) -> value (one byte).
///
/// # Errors
/// The firmware's status when it is not 0 (see `check`).
pub fn decode_misc(output: u64) -> Result<u8, FirmwareError> {
    check(output).map(|v| byte(v, 0))
}

/// GPU overclock level (misc setting 5), ordered from lowest to highest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GpuOc {
    Normal = 0,
    Faster = 1,
    Turbo = 2,
}

impl GpuOc {
    /// All levels, in combo-box order (the index is the level).
    pub const ALL: [GpuOc; 3] = [GpuOc::Normal, GpuOc::Faster, GpuOc::Turbo];

    /// Decode a level; the caller masks off the 0x20 the firmware reads back with it.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<GpuOc> {
        match v {
            0 => Some(GpuOc::Normal),
            1 => Some(GpuOc::Faster),
            2 => Some(GpuOc::Turbo),
            _ => None,
        }
    }

    /// Name shown in the window.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            GpuOc::Normal => "Normal",
            GpuOc::Faster => "Faster",
            GpuOc::Turbo => "Turbo",
        }
    }
}

// ---------------------------------------------------------------- keyboard lighting

/// 24-bit color of a keyboard zone or lighting effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    /// A color from its red, green and blue parts.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    /// Acer's default zone color (`HW_Support.ini`).
    pub const ACER_CYAN: Rgb = Rgb::new(0x00, 0xAE, 0xC7);

    /// Parse `#RRGGBB` (the `#` is optional).
    #[must_use]
    pub fn parse_hex(s: &str) -> Option<Rgb> {
        let s = s.strip_prefix('#').unwrap_or(s);
        // from_str_radix alone would accept a leading '+'.
        if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        let [blue, green, red, _] = v.to_le_bytes();
        Some(Rgb::new(red, green, blue))
    }

    /// Format as `#RRGGBB` (upper case).
    #[must_use]
    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }

    /// Win32 COLORREF (0x00BBGGRR).
    #[must_use]
    pub const fn colorref(self) -> u32 {
        (self.r as u32) | ((self.g as u32) << 8) | ((self.b as u32) << 16)
    }

    /// Inverse of `colorref`.
    #[must_use]
    pub const fn from_colorref(c: u32) -> Rgb {
        let [red, green, blue, _] = c.to_le_bytes();
        Rgb::new(red, green, blue)
    }
}

/// Keyboard color zones the protocol can address (the PT515-51 has 3).
pub const KB_ZONES: usize = 4;

/// `SetGamingRgbKb` input: zone bitmap in bits 0..8, then R, G, B bytes.
#[must_use]
pub fn rgb_zone_input(zone_mask: u8, c: Rgb) -> Option<u64> {
    (zone_mask != 0 && zone_mask < (1 << KB_ZONES))
        .then_some(u64::from(zone_mask) | u64::from(c.r) << 8 | u64::from(c.g) << 16 | u64::from(c.b) << 24)
}

/// GetGamingRgbKb(zone bit) -> color of that zone.
///
/// # Errors
/// The firmware's status when it is not 0 (see `check`).
pub fn decode_rgb(output: u64) -> Result<Rgb, FirmwareError> {
    check(output).map(|v| Rgb::new(byte(v, 0), byte(v, 1), byte(v, 2)))
}

/// Keyboard lighting effect (byte 0 of the backlight payload).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KbEffect {
    /// Per-zone colors, sent separately with `SetGamingRgbKb`.
    Static = 0,
    Breathing = 1,
    Neon = 2,
    Wave = 3,
    Shifting = 4,
    Zoom = 5,
}

impl KbEffect {
    /// All effects, in combo-box order (the index is the effect code).
    pub const ALL: [KbEffect; 6] =
        [KbEffect::Static, KbEffect::Breathing, KbEffect::Neon, KbEffect::Wave, KbEffect::Shifting, KbEffect::Zoom];

    /// Decode an effect number (byte 0 of the backlight payload).
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<KbEffect> {
        match v {
            0 => Some(KbEffect::Static),
            1 => Some(KbEffect::Breathing),
            2 => Some(KbEffect::Neon),
            3 => Some(KbEffect::Wave),
            4 => Some(KbEffect::Shifting),
            5 => Some(KbEffect::Zoom),
            _ => None,
        }
    }

    /// Name shown in the window.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            KbEffect::Static => "Static",
            KbEffect::Breathing => "Breathing",
            KbEffect::Neon => "Neon",
            KbEffect::Wave => "Wave",
            KbEffect::Shifting => "Shifting",
            KbEffect::Zoom => "Zoom",
        }
    }

    /// Effects that use the single effect color. Neon needs one too: without it the keys
    /// stay dark (`PSAdminAgent`'s 4-zone builder sends a color for Neon, Breathing and Zoom).
    #[must_use]
    pub const fn uses_color(self) -> bool {
        matches!(self, KbEffect::Breathing | KbEffect::Neon | KbEffect::Zoom)
    }

    /// Effects that move across the keyboard and take a `Direction`.
    #[must_use]
    pub const fn uses_direction(self) -> bool {
        matches!(self, KbEffect::Wave | KbEffect::Zoom)
    }

    /// Byte 3: Wave and Shifting carry a pattern flag (0x08; PredatorSense also offers 0x09/0x0A).
    #[must_use]
    pub const fn pattern(self) -> u8 {
        match self {
            KbEffect::Wave | KbEffect::Shifting => 0x08,
            _ => 0,
        }
    }
}

/// Direction of a moving effect (byte 4 of the backlight payload).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    RightToLeft = 1,
    LeftToRight = 2,
}

/// Everything `SetGamingKBBacklight` sets in one call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KbBacklight {
    pub effect: KbEffect,
    /// `0..=KB_MAX_SPEED`; ignored (sent as 0) for Static.
    pub speed: u8,
    /// `0..=KB_MAX_BRIGHTNESS` percent.
    pub brightness: u8,
    /// Used only by effects with `uses_direction`.
    pub direction: Direction,
    /// Used only by effects with `uses_color`.
    pub color: Rgb,
}

impl Default for KbBacklight {
    fn default() -> Self {
        KbBacklight {
            effect: KbEffect::Static,
            speed: 4,
            brightness: 100,
            direction: Direction::RightToLeft,
            color: Rgb::ACER_CYAN,
        }
    }
}

/// Highest lighting-effect speed.
pub const KB_MAX_SPEED: u8 = 9;
/// Highest keyboard brightness (percent).
pub const KB_MAX_BRIGHTNESS: u8 = 100;
/// PredatorSense offers five brightness levels: 0, 25, 50, 75, 100.
pub const KB_BRIGHTNESS_STEP: u8 = 25;

/// `SetGamingKBBacklight` payload (uint8[16]), as `PSAdminAgent` builds it:
/// bytes 0..8 = `[effect, speed, brightness, pattern, direction, r, g, b]`, and bytes 8..16
/// repeat bytes 0..8 (`PSAdminAgent` expands a u64 with 64-bit shifts that wrap at 64).
#[must_use]
pub fn kb_backlight_payload(kb: &KbBacklight) -> [u8; 16] {
    let mut p = [0u8; 16];
    p[0] = kb.effect as u8;
    p[1] = if kb.effect == KbEffect::Static { 0 } else { kb.speed.min(KB_MAX_SPEED) };
    p[2] = kb.brightness.min(KB_MAX_BRIGHTNESS);
    p[3] = kb.effect.pattern();
    p[4] = if kb.effect.uses_direction() { kb.direction as u8 } else { 0 };
    if kb.effect.uses_color() {
        p[5] = kb.color.r;
        p[6] = kb.color.g;
        p[7] = kb.color.b;
    }
    p.copy_within(0..8, 8);
    p
}

// ---------------------------------------------------------------- events

/// Decoded `APGeEvent.EventDetail` (uint8[8]); only the first two bytes matter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HwEvent {
    /// `[7, 4, ..]`: Turbo key event. The PT515-51's Turbo button sends none (see
    /// `Controller::poll_turbo_button`).
    TurboKey,
    /// `[1, _, ..]`: the keyboard backlight changed (e.g. by its hotkey).
    KbBacklightChanged,
    /// Anything else: the first two bytes.
    Other(u8, u8),
}

/// The PredatorSense button is an ordinary keyboard key: scan code E0 75, virtual key 0xFF
/// (captured on a PT515-51). With tools\install-predatorsense-button.ps1 it is remapped to
/// "Launch App 1" (`VK_LAUNCH_APP1` 0xB6). Raw input then reports the remapped virtual key with the
/// *original* scan code (E0 75, logged on the PT515-51), while hooks see E0 6B; all are accepted.
/// `e0` is the extended flag.
#[must_use]
pub const fn is_predator_key(scan: u16, e0: bool, vkey: u16) -> bool {
    let original = scan == SCAN_PREDATOR_KEY && vkey == VK_PREDATOR_KEY;
    let remapped = (scan == SCAN_LAUNCH_APP1 || scan == SCAN_PREDATOR_KEY) && vkey == VK_LAUNCH_APP1;
    e0 && (original || remapped)
}

/// Scan code (with the E0 prefix) of the PredatorSense button.
pub const SCAN_PREDATOR_KEY: u16 = 0x75;
/// Virtual key Windows gives the unmapped button (0xFF, "no virtual key").
pub const VK_PREDATOR_KEY: u16 = 0xFF;
/// Scan code (with the E0 prefix) of "Launch App 1", which the setup script maps the button to.
pub const SCAN_LAUNCH_APP1: u16 = 0x6B;
/// `VK_LAUNCH_APP1`.
pub const VK_LAUNCH_APP1: u16 = 0xB6;

/// `None` when the detail is shorter than two bytes.
#[must_use]
pub fn decode_event(detail: &[u8]) -> Option<HwEvent> {
    match detail {
        [7, 4, ..] => Some(HwEvent::TurboKey),
        [1, _, ..] => Some(HwEvent::KbBacklightChanged),
        [a, b, ..] => Some(HwEvent::Other(*a, *b)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Values captured from the read-only probe on a PT515-51.
    #[test]
    fn status_byte() {
        assert_eq!(check(0x4D00), Ok(0x4D));
        assert_eq!(check(0x1), Err(FirmwareError::Unsupported));
        assert_eq!(check(0x2), Err(FirmwareError::InvalidParam));
        assert_eq!(check(0x7F), Err(FirmwareError::Status(0x7F)));
        assert_eq!(check(0), Ok(0));
    }

    #[test]
    fn sensors_from_probe() {
        assert_eq!(Sensor::CpuTemp.input(), 0x101);
        assert_eq!(Sensor::GpuTemp.input(), 0xA01);
        assert_eq!(decode_sensor(0x4D00), Ok(77));
        assert_eq!(decode_sensor(0x0B_9000), Ok(2960));
        assert_eq!(decode_sensor(0x10_B200), Ok(4274));
        assert_eq!(decode_sensor(0x1), Err(FirmwareError::Unsupported));
        // Bits above the 16-bit reading are ignored.
        assert_eq!(decode_sensor(0xFF_0000_4D00), Ok(77));
    }

    #[test]
    fn sensor_formatting_rejects_garbage() {
        assert_eq!(format_sensor(Sensor::CpuTemp, Some(77), None), "CPU: 77 °C");
        assert_eq!(format_sensor(Sensor::CpuTemp, Some(0), None), "CPU: --");
        assert_eq!(format_sensor(Sensor::CpuTemp, Some(255), None), "CPU: --");
        assert_eq!(format_sensor(Sensor::CpuFan, Some(0), None), "CPU fan: 0 RPM");
        assert_eq!(format_sensor(Sensor::CpuFan, Some(65535), Some(40)), "CPU fan: --");
        assert_eq!(format_sensor(Sensor::GpuFan, None, None), "GPU fan: --");
        assert_eq!(format_sensor(Sensor::CpuFan, Some(3413), Some(38)), "CPU fan: 3413 RPM, 38%");
    }

    #[test]
    fn fan_mode_from_probe() {
        // GetGamingFanBehavior(0x1) = 0x100, (0x8) = 0x4000, (0x10) = 0x10000, (0x2) = 0.
        assert_eq!(decode_fan_mode(0, 0x100), Ok(Some(FanMode::Auto)));
        assert_eq!(decode_fan_mode(3, 0x4000), Ok(Some(FanMode::Auto)));
        assert_eq!(decode_fan_mode(4, 0x1_0000), Ok(Some(FanMode::Auto)));
        assert_eq!(decode_fan_mode(1, 0x0), Ok(None));
        assert_eq!(decode_fan_mode(0, 0x1), Err(FirmwareError::Unsupported));
    }

    #[test]
    fn fan_behavior_encoding() {
        let all = 0b11001; // slots 0, 3, 4
        assert_eq!(
            fan_behavior_input(&[(FanGroup::Cpu, FanMode::Auto), (FanGroup::Gpu, FanMode::Auto)], all),
            0x19 | (0b01_01_00_00_01 << 16)
        );
        assert_eq!(
            fan_behavior_input(&[(FanGroup::Cpu, FanMode::Max), (FanGroup::Gpu, FanMode::Max)], all),
            0x19 | (0b10_10_00_00_10 << 16)
        );
        assert_eq!(fan_behavior_input(&[(FanGroup::Gpu, FanMode::Custom)], all), 0x18 | (0b11_11_00_00_00 << 16));
        // Machine with a single GPU fan in slot 3.
        assert_eq!(fan_behavior_input(&[(FanGroup::Gpu, FanMode::Max)], 0b01001), 0x8 | (0b10 << 22));
        // No fans present -> nothing is addressed.
        assert_eq!(fan_behavior_input(&[(FanGroup::Cpu, FanMode::Max)], 0), 0);
        assert_eq!(fan_behavior_input(&[], all), 0);
    }

    #[test]
    fn fan_speed_encoding_and_bounds() {
        assert_eq!(fan_speed_input(0, 0), Some(0x0001));
        // 35 % duty needs firmware 60 % on the CPU fan, 31 % duty does on the GPU fans.
        assert_eq!(fan_speed_input(0, 35), Some(0x3C01));
        assert_eq!(fan_speed_input(3, 31), Some(0x3C04));
        assert_eq!(fan_speed_input(3, 100), Some(0x6404));
        // Regression: fan 3 (slot 4) has its own id 5; it kept spinning when only id 4 was set.
        assert_eq!(fan_speed_input(4, 0), Some(0x0005));
        assert_eq!(fan_speed_input(3, 101), None);
        assert_eq!(fan_speed_input(0, 255), None);
    }

    #[test]
    fn fan_duty_calibration() {
        // Measured points come back exactly.
        assert_eq!(fan_duty_of(0, 80), 62);
        assert_eq!(fan_duty_of(3, 80), 55);
        assert_eq!(firmware_fan_pct(0, 62), 80);
        assert_eq!(firmware_fan_pct(4, 55), 80);
        // Between points: linear.
        assert_eq!(fan_duty_of(0, 25), 10);
        assert_eq!(firmware_fan_pct(0, 10), 25);
        for slot in [0, 3, 4] {
            assert_eq!((firmware_fan_pct(slot, 0), firmware_fan_pct(slot, 100)), (0, 100));
            let mut last = 0;
            for d in 0..=100u8 {
                let p = firmware_fan_pct(slot, d);
                assert!(p >= last, "slot {slot} duty {d}");
                last = p;
                // Converting there and back stays within one point.
                assert!(fan_duty_of(slot, p).abs_diff(d) <= 1, "slot {slot} duty {d} -> {p}");
            }
        }
    }

    #[test]
    fn turbo_led_and_misc() {
        assert_eq!(decode_turbo_led(0x0), Ok(false));
        assert_eq!(decode_turbo_led(0x100), Ok(true));
        assert_eq!(decode_turbo_led(0x1), Err(FirmwareError::Unsupported));
        // Constants observed in PSSvc / PSAdminAgent.
        assert_eq!(misc_input(MiscSetting::GpuOc, GpuOc::Normal as u8), 0x005);
        assert_eq!(misc_input(MiscSetting::GpuOc, GpuOc::Faster as u8), 0x105);
        assert_eq!(misc_input(MiscSetting::GpuOc, GpuOc::Turbo as u8), 0x205);
        assert_eq!(misc_input(MiscSetting::Turbo, 1), 0x101);
        assert_eq!(misc_input(MiscSetting::Turbo, 0), 0x001);
        assert_eq!(misc_input(MiscSetting::BootAnimation, 1), 0x106);
        // GPU mode, as Set_System_GPUMode builds it.
        assert_eq!(misc_input(MiscSetting::GpuMode, GpuMode::Hybrid as u8), 0x102);
        assert_eq!(misc_input(MiscSetting::GpuMode, GpuMode::Discrete as u8), 0x202);
        assert_eq!(GpuMode::from_u8(2), Some(GpuMode::Discrete));
        assert_eq!(GpuMode::from_u8(0), None);
        assert_eq!(GpuMode::from_u8(3), None);
        assert_eq!(decode_misc(0x0), Ok(0));
        assert_eq!(decode_misc(0x200), Ok(2));
        assert_eq!(GpuOc::from_u8(3), None);
    }

    #[test]
    fn rgb_from_probe() {
        assert_eq!(decode_rgb(0xC7AE_0000), Ok(Rgb::ACER_CYAN));
        assert_eq!(rgb_zone_input(0x1, Rgb::ACER_CYAN), Some(0xC7AE_0001));
        assert_eq!(rgb_zone_input(0xF, Rgb::new(0xFF, 0, 0)), Some(0x0000_FF0F));
        assert_eq!(rgb_zone_input(0, Rgb::ACER_CYAN), None);
        assert_eq!(rgb_zone_input(0x10, Rgb::ACER_CYAN), None);
        assert_eq!(decode_rgb(0x1), Err(FirmwareError::Unsupported));
    }

    #[test]
    fn rgb_hex_and_colorref() {
        assert_eq!(Rgb::parse_hex("#00AEC7"), Some(Rgb::ACER_CYAN));
        assert_eq!(Rgb::parse_hex("00aec7"), Some(Rgb::ACER_CYAN));
        assert_eq!(Rgb::parse_hex("#00AEC"), None);
        assert_eq!(Rgb::parse_hex("#00AEC7FF"), None);
        assert_eq!(Rgb::parse_hex("#GGAEC7"), None);
        assert_eq!(Rgb::parse_hex("+0AEC7"), None);
        assert_eq!(Rgb::parse_hex("#é0AEC"), None);
        assert_eq!(Rgb::parse_hex(""), None);
        assert_eq!(Rgb::ACER_CYAN.to_hex(), "#00AEC7");
        let c = Rgb::new(1, 2, 3);
        assert_eq!(c.colorref(), 0x0003_0201);
        assert_eq!(Rgb::from_colorref(c.colorref()), c);
    }

    #[test]
    fn kb_payload_layout_and_clamping() {
        let kb = KbBacklight {
            effect: KbEffect::Wave,
            speed: 50,
            brightness: 250,
            direction: Direction::LeftToRight,
            color: Rgb::new(9, 8, 7),
        };
        // Layouts match the u64 values PSAdminAgent builds for each effect.
        let wave = kb_backlight_payload(&kb);
        assert_eq!(wave[..8], [3, 9, 100, 8, 2, 0, 0, 0]);
        assert_eq!(wave[8..], wave[..8]);

        let p = kb_backlight_payload(&KbBacklight { effect: KbEffect::Breathing, ..kb });
        assert_eq!(p[..8], [1, 9, 100, 0, 0, 9, 8, 7]);
        let p = kb_backlight_payload(&KbBacklight { effect: KbEffect::Zoom, ..kb });
        assert_eq!(p[..8], [5, 9, 100, 0, 2, 9, 8, 7]);
        // Regression (PT515-51): Shifting needs pattern 0x08 and no color; Neon needs the color,
        // otherwise it is sent black and the keyboard stays dark.
        let p = kb_backlight_payload(&KbBacklight { effect: KbEffect::Shifting, ..kb });
        assert_eq!(p[..8], [4, 9, 100, 8, 0, 0, 0, 0]);
        let p = kb_backlight_payload(&KbBacklight { effect: KbEffect::Neon, ..kb });
        assert_eq!(p[..8], [2, 9, 100, 0, 0, 9, 8, 7]);

        // Static: PSAdminAgent sends just `brightness << 16`.
        let p = kb_backlight_payload(&KbBacklight { brightness: 75, ..KbBacklight::default() });
        assert_eq!(u64::from_le_bytes(p[..8].try_into().unwrap()), 75 << 16);
        assert_eq!(p[8..], p[..8]);
    }

    #[test]
    fn events() {
        assert_eq!(decode_event(&[7, 4, 0, 0, 0, 0, 0, 0]), Some(HwEvent::TurboKey));
        assert_eq!(decode_event(&[7, 4]), Some(HwEvent::TurboKey));
        assert_eq!(decode_event(&[7, 5, 0]), Some(HwEvent::Other(7, 5)));
        assert_eq!(decode_event(&[1, 0, 0]), Some(HwEvent::KbBacklightChanged));
        assert_eq!(decode_event(&[7]), None);
        assert_eq!(decode_event(&[]), None);
    }

    #[test]
    fn predator_key() {
        assert!(is_predator_key(0x75, true, 0xFF));
        assert!(!is_predator_key(0x75, false, 0xFF)); // plain 0x75 is not extended
        assert!(!is_predator_key(0x75, true, 0x2E));
        assert!(!is_predator_key(0x74, true, 0xFF));
        // Remapped to Launch App 1 (captured after the remap).
        assert!(is_predator_key(0x6B, true, 0xB6));
        assert!(!is_predator_key(0x6B, false, 0xB6));
        assert!(!is_predator_key(0x6B, true, 0xFF));
        // Regression: what raw input actually delivers after the remap (original scan, new key).
        assert!(is_predator_key(0x75, true, 0xB6));
        assert!(!is_predator_key(0x75, false, 0xB6));
        assert!(!is_predator_key(0x30, true, 0xB6));
    }

    #[test]
    fn running_gpu_mode_from_adapters() {
        // Seen on the PT515-51 in Discrete mode: NVIDIA plus a virtual adapter, no Intel.
        let discrete = ["PCI\\VEN_10DE&DEV_1ED0&SUBSYS_132D1025&REV_A1", "ROOT\\DISPLAY\\0000"];
        assert_eq!(running_gpu_mode(&discrete), GpuMode::Discrete);
        let hybrid = ["PCI\\VEN_8086&DEV_3E9B&SUBSYS_132D1025&REV_02", "PCI\\VEN_10DE&DEV_1ED0"];
        assert_eq!(running_gpu_mode(&hybrid), GpuMode::Hybrid);
        assert_eq!(running_gpu_mode(&["pci\\ven_8086&dev_3e9b"]), GpuMode::Hybrid);
        assert_eq!(running_gpu_mode::<&str>(&[]), GpuMode::Discrete);
    }

    #[test]
    fn coolboost_encoding() {
        assert_eq!(coolboost_set_input(true), 0x1_0007);
        assert_eq!(coolboost_set_input(false), 0x0_0007);
        // Probe on the PT515-51: 0x10000 = OK, off.
        assert_eq!(decode_coolboost(0x1_0000), Ok(false));
        assert_eq!(decode_coolboost(0x1_0100), Ok(true));
        // APGeAction error codes.
        assert_eq!(decode_coolboost(0xE2), Err(FirmwareError::Status(0xE2)));
    }

    #[test]
    fn win_key_encoding() {
        assert_eq!(win_key_set_input(true), 0x0100_0002);
        assert_eq!(win_key_set_input(false), 0x0000_0002);
        // PT515-51 before and after enabling the keys (byte 6, LCD overdrive, untouched).
        assert_eq!(decode_win_key(0x0001_0000_0000_0100), Ok(false));
        assert_eq!(decode_win_key(0x0001_0000_0100_0100), Ok(true));
        assert_eq!(decode_win_key(0x1), Err(FirmwareError::Unsupported));
    }

    #[test]
    fn lcd_overdrive_encoding() {
        assert_eq!(lcd_overdrive_set_input(true), 0x1_0000_0000_0010);
        assert_eq!(lcd_overdrive_set_input(false), 0x10);
        // PT515-51: overdrive on, Win key off.
        assert_eq!(decode_lcd_overdrive(0x0001_0000_0000_0100), Ok(true));
        assert_eq!(decode_lcd_overdrive(0x0000_0000_0100_0100), Ok(false));
    }

    #[test]
    fn fan_duty_encoding() {
        assert_eq!(Sensor::CpuFan.duty_input(), Some(0xC01));
        assert_eq!(Sensor::GpuFan.duty_input(), Some(0x1001));
        assert_eq!(Sensor::Fan3.duty_input(), Some(0x1101));
        assert_eq!(Sensor::CpuTemp.duty_input(), None);
        // PT515-51 readings: 38 %; out of range; unsupported.
        assert_eq!(decode_duty(0x2600), Ok(Some(38)));
        assert_eq!(decode_duty(0x6500), Ok(None));
        assert_eq!(decode_duty(0x1), Err(FirmwareError::Unsupported));
    }

    #[test]
    fn usb_charge_encoding() {
        // Writes and read-backs captured on the PT515-51.
        assert_eq!(usb_charge_set_input(20), 0x14_0F04);
        assert_eq!(usb_charge_set_input(30), 0x1E_0F04);
        assert_eq!(usb_charge_set_input(0), 0x0A_1F04);
        assert_eq!(decode_usb_charge(0x1E_0F00), Ok(30));
        assert_eq!(decode_usb_charge(0x0A_1F00), Ok(0));
        // AN515-57 before the first write (opensense): neither on nor off, shown as off.
        assert_eq!(decode_usb_charge(0x6400), Ok(0));
        assert_eq!(USB_CHARGE_FLOORS.map(usb_charge_label), ["Off", "Until 10%", "Until 20%", "Until 30%"]);
    }

    #[test]
    fn kb_timeout_encoding() {
        // Captured on the PT515-51 (brightness 100, then timeouts 120 and 30).
        assert_eq!(decode_kb_timeout(0x7864_0008_0000), Ok((100, 120)));
        assert_eq!(kb_timeout_set_input(100, 30), 0x1E64_0008_8402);
        assert_eq!(kb_timeout_set_input(100, 0), 0x0064_0008_8402);
        assert_eq!(decode_kb_timeout(0xE2), Err(FirmwareError::Status(0xE2)));
        assert_eq!(KB_TIMEOUTS.map(kb_timeout_label), ["Never", "30 s", "1 min", "2 min"]);
    }

    #[test]
    fn enum_round_trips() {
        for m in [FanMode::Auto, FanMode::Max, FanMode::Custom] {
            assert_eq!(FanMode::parse(m.as_str()), Some(m));
            assert_eq!(FanMode::from_bits(m as u64), Some(m));
        }
        assert_eq!(FanMode::parse("Auto"), None);
        assert_eq!(FanMode::from_bits(0), None);
        for e in KbEffect::ALL {
            assert_eq!(KbEffect::from_u8(e as u8), Some(e));
        }
        assert_eq!(KbEffect::from_u8(6), None);
        for g in GpuOc::ALL {
            assert_eq!(GpuOc::from_u8(g as u8), Some(g));
        }
    }

    fn all_distinct<T: PartialEq>(v: &[T]) -> bool {
        v.iter().enumerate().all(|(i, a)| v[i + 1..].iter().all(|b| a != b))
    }

    const METHODS: [Method; Method::COUNT] = [
        Method::SetGamingProfile,
        Method::GetGamingProfile,
        Method::SetGamingLed,
        Method::GetGamingLed,
        Method::GetGamingSysInfo,
        Method::SetGamingRgbKb,
        Method::GetGamingRgbKb,
        Method::SetGamingFanBehavior,
        Method::GetGamingFanBehavior,
        Method::SetGamingFanSpeed,
        Method::GetGamingFanSpeed,
        Method::SetGamingKbBacklight,
        Method::GetGamingKbBacklight,
        Method::SetGamingMiscSetting,
        Method::GetGamingMiscSetting,
        Method::ApgeGetFunction,
        Method::ApgeSetFunction,
    ];

    #[test]
    fn method_table() {
        for (i, m) in METHODS.iter().enumerate() {
            assert_eq!(m.index(), i, "{m:?}");
        }
        assert!(all_distinct(&METHODS.map(Method::name)));
        for m in METHODS {
            let apge = matches!(m, Method::ApgeGetFunction | Method::ApgeSetFunction);
            assert_eq!(m.class(), if apge { "APGeAction" } else { "AcerGamingFunction" });
            assert_eq!(m.params(), if apge { ("uiInput", "uiOutput") } else { ("gmInput", "gmOutput") });
            if m.name().starts_with("Get") {
                assert!(!m.input_is_u64(), "{m:?}");
            }
        }
        assert!(!Method::SetGamingRgbKb.input_is_u64());
        assert_eq!(METHODS.iter().filter(|m| m.input_is_u64()).count(), 6);
    }

    #[test]
    fn status_byte_limits() {
        assert_eq!(check(0x100), Ok(1));
        assert_eq!(check(0xFF00), Ok(0xFF));
        assert_eq!(check(!0xFF), Ok(u64::MAX >> 8));
        assert_eq!(check(3), Err(FirmwareError::Status(3)));
        assert_eq!(check(0xFF), Err(FirmwareError::Status(0xFF)));
        assert_eq!(check(u64::MAX), Err(FirmwareError::Status(0xFF)));
        // A non-zero status wins over any payload.
        assert_eq!(check(0xDEAD_BEEF_0001), Err(FirmwareError::Unsupported));
        assert_eq!(check(0xDEAD_BEEF_0002), Err(FirmwareError::InvalidParam));
    }

    #[test]
    fn firmware_error_display() {
        assert_eq!(FirmwareError::Unsupported.to_string(), "not supported by this model");
        assert_eq!(FirmwareError::InvalidParam.to_string(), "invalid parameter");
        assert_eq!(FirmwareError::Status(0xE2).to_string(), "firmware status 0xe2");
        assert_eq!(FirmwareError::Status(3).to_string(), "firmware status 0x03");
    }

    #[test]
    fn every_decoder_reports_status_errors() {
        let decoders: [fn(u64) -> Result<(), FirmwareError>; 12] = [
            |o| decode_sensor(o).map(|_| ()),
            |o| decode_duty(o).map(|_| ()),
            |o| decode_fan_mode(0, o).map(|_| ()),
            |o| decode_fan_mode(FAN_SLOTS - 1, o).map(|_| ()),
            |o| decode_turbo_led(o).map(|_| ()),
            |o| decode_coolboost(o).map(|_| ()),
            |o| decode_win_key(o).map(|_| ()),
            |o| decode_lcd_overdrive(o).map(|_| ()),
            |o| decode_kb_timeout(o).map(|_| ()),
            |o| decode_usb_charge(o).map(|_| ()),
            |o| decode_misc(o).map(|_| ()),
            |o| decode_rgb(o).map(|_| ()),
        ];
        for (i, decode) in decoders.iter().enumerate() {
            assert_eq!(decode(0), Ok(()), "decoder {i}");
            assert_eq!(decode(!0xFF), Ok(()), "decoder {i}");
            assert_eq!(decode(0xFFFF_0001), Err(FirmwareError::Unsupported), "decoder {i}");
            assert_eq!(decode(0xFFFF_0002), Err(FirmwareError::InvalidParam), "decoder {i}");
            for s in [3u8, 0x7F, 0xE2, 0xFF] {
                assert_eq!(decode(0xFFFF_0000 | u64::from(s)), Err(FirmwareError::Status(s)), "decoder {i}");
            }
        }
    }

    #[test]
    fn decoders_on_zero_payload() {
        assert_eq!(decode_sensor(0), Ok(0));
        assert_eq!(decode_duty(0), Ok(Some(0)));
        assert_eq!(decode_coolboost(0), Ok(false));
        assert_eq!(decode_win_key(0), Ok(false));
        assert_eq!(decode_lcd_overdrive(0), Ok(false));
        assert_eq!(decode_kb_timeout(0), Ok((0, 0)));
        assert_eq!(decode_usb_charge(0), Ok(0));
        assert_eq!(decode_misc(0), Ok(0));
        assert_eq!(decode_rgb(0), Ok(Rgb::default()));
        for slot in 0..FAN_SLOTS {
            assert_eq!(decode_fan_mode(slot, 0), Ok(None), "slot {slot}");
        }
    }

    #[test]
    fn sensor_reading_limits() {
        assert_eq!(decode_sensor(0xFF_FF00), Ok(u16::MAX));
        assert_eq!(decode_sensor(!0xFF), Ok(u16::MAX));
        // Bit 16 of the reading is dropped.
        assert_eq!(decode_sensor(0x0100_0000), Ok(0));
        assert_eq!(decode_duty(0x6400), Ok(Some(100)));
        assert_eq!(decode_duty(0xFF00), Ok(None));
        // 256 does not fit a percentage.
        assert_eq!(decode_duty(0x1_0000), Ok(None));
        assert_eq!(decode_duty(0xFFFF_0000_2600), Ok(Some(38)));
        assert_eq!(decode_duty(0x2), Err(FirmwareError::InvalidParam));
    }

    #[test]
    fn sensor_formatting_limits() {
        assert_eq!(format_sensor(Sensor::CpuTemp, Some(1), None), "CPU: 1 °C");
        assert_eq!(format_sensor(Sensor::GpuTemp, Some(125), None), "GPU: 125 °C");
        assert_eq!(format_sensor(Sensor::SysTemp, Some(126), None), "System: --");
        assert_eq!(format_sensor(Sensor::SysTemp, None, None), "System: --");
        // A temperature has no duty, and a fan-sized reading is not a temperature.
        assert_eq!(format_sensor(Sensor::CpuTemp, Some(77), Some(50)), "CPU: 77 °C");
        assert_eq!(format_sensor(Sensor::CpuTemp, Some(3000), None), "CPU: --");
        assert_eq!(format_sensor(Sensor::CpuFan, Some(12000), Some(100)), "CPU fan: 12000 RPM, 100%");
        assert_eq!(format_sensor(Sensor::GpuFan, Some(12001), None), "GPU fan: --");
        assert_eq!(format_sensor(Sensor::Fan3, Some(2000), Some(0)), "Fan 3: 2000 RPM, 0%");
        assert_eq!(format_sensor(Sensor::Fan3, None, Some(40)), "Fan 3: --");
    }

    #[test]
    fn sensor_table() {
        assert_eq!(Sensor::ALL.map(|s| s as u8), [0x01, 0x0A, 0x03, 0x02, 0x06, 0x09]);
        assert_eq!(Sensor::ALL.map(Sensor::is_temp), [true, true, true, false, false, false]);
        assert_eq!(Sensor::ALL.map(Sensor::label), ["CPU", "GPU", "System", "CPU fan", "GPU fan", "Fan 3"]);
        for s in Sensor::ALL {
            assert_eq!(s.input(), (u64::from(s as u8) << 8) | 1);
            assert_eq!(s.duty_input().is_some(), !s.is_temp(), "{s:?}");
        }
        // Duty ids never collide with each other or with the sensors themselves.
        let inputs: Vec<u64> = Sensor::ALL
            .map(Sensor::input)
            .into_iter()
            .chain(Sensor::ALL.into_iter().filter_map(Sensor::duty_input))
            .collect();
        assert_eq!(inputs.len(), 9);
        assert!(all_distinct(&inputs));
    }

    #[test]
    fn fan_mode_bits_and_names() {
        assert_eq!(FanMode::from_bits(0b101), Some(FanMode::Auto));
        assert_eq!(FanMode::from_bits(0b100), None);
        assert_eq!(FanMode::from_bits(u64::MAX), Some(FanMode::Custom));
        for s in ["", "AUTO", "Max", " auto", "auto ", "custom\n", "manual"] {
            assert_eq!(FanMode::parse(s), None, "{s:?}");
        }
    }

    #[test]
    fn fan_mode_of_each_slot() {
        // Every slot holds a different code; neighbours' bits must not leak into a slot.
        let codes = [1u64, 2, 3, 0, 3, 2, 1, 3];
        let payload = codes.iter().enumerate().fold(0, |acc, (slot, c)| acc | c << (2 * slot));
        // Bits past the last slot are ignored.
        let output = (payload | 0xFF << 16) << 8;
        for slot in 0..FAN_SLOTS {
            assert_eq!(decode_fan_mode(slot, output), Ok(FanMode::from_bits(codes[slot as usize])), "slot {slot}");
        }
    }

    #[test]
    fn fan_behavior_reads_back_per_slot() {
        let all = 0b11001;
        let modes = [FanMode::Auto, FanMode::Max, FanMode::Custom];
        for cpu_mode in modes {
            for gpu_mode in modes {
                let input = fan_behavior_input(&[(FanGroup::Cpu, cpu_mode), (FanGroup::Gpu, gpu_mode)], all);
                assert_eq!(input & 0xFFFF, u64::from(all));
                // GetGamingFanBehavior returns the same 2-bit codes right above the status byte.
                let output = (input >> 16) << 8;
                assert_eq!(decode_fan_mode(0, output), Ok(Some(cpu_mode)));
                assert_eq!(decode_fan_mode(3, output), Ok(Some(gpu_mode)));
                assert_eq!(decode_fan_mode(4, output), Ok(Some(gpu_mode)));
                assert_eq!(decode_fan_mode(1, output), Ok(None));
                assert_eq!(decode_fan_mode(2, output), Ok(None));
            }
        }
    }

    #[test]
    fn fan_behavior_present_mask() {
        let items = [(FanGroup::Cpu, FanMode::Max), (FanGroup::Gpu, FanMode::Custom)];
        assert_eq!(fan_behavior_input(&items, u32::MAX), fan_behavior_input(&items, 0b11001));
        // Slots no group uses address nothing.
        assert_eq!(fan_behavior_input(&items, 0b1110_0110), 0);
        // Only the second GPU-side fan.
        assert_eq!(fan_behavior_input(&items, 0b10000), 0x10 | (0b11 << 24));
        assert_eq!(fan_behavior_input(&items, 0b00001), 0x01 | (0b10 << 16));
    }

    #[test]
    fn fan_group_table() {
        for (i, g) in FanGroup::ALL.iter().enumerate() {
            assert_eq!(g.index(), i);
        }
        assert_eq!(FanGroup::ALL.map(FanGroup::label), ["CPU", "GPU"]);
        let slots: Vec<u32> = FanGroup::ALL.iter().flat_map(|g| g.slots().iter().copied()).collect();
        assert!(all_distinct(&slots));
        assert!(slots.iter().all(|&s| s < FAN_SLOTS));
        assert_eq!(slots.iter().map(|&s| fan_speed_id(s)).collect::<Vec<_>>(), [1, 4, 5]);
        assert_eq!(fan_speed_id(FAN_SLOTS - 1), 8);
        assert_eq!(fan_speed_id(u32::MAX), 1 << 32);
    }

    #[test]
    fn fan_duty_tables_are_well_formed() {
        for table in [&CPU_FAN_DUTY, &GPU_FAN_DUTY] {
            assert_eq!((table[0], table[table.len() - 1]), ((0, 0), (100, 100)));
            // interpolate divides by each segment's width in both directions.
            for w in table.windows(2) {
                assert!(w[0].0 < w[1].0 && w[0].1 < w[1].1, "{w:?}");
            }
        }
        for (c, g) in CPU_FAN_DUTY.iter().zip(&GPU_FAN_DUTY) {
            assert_eq!(c.0, g.0);
            assert!(g.1 <= c.1, "GPU fans run lower: {c:?} {g:?}");
        }
    }

    #[test]
    fn fan_duty_at_and_between_table_points() {
        for (slot, table) in [(0, &CPU_FAN_DUTY), (3, &GPU_FAN_DUTY), (4, &GPU_FAN_DUTY)] {
            for &(fw, duty) in table {
                assert_eq!(fan_duty_of(slot, fw), duty, "slot {slot} firmware {fw}");
                assert_eq!(firmware_fan_pct(slot, duty), fw, "slot {slot} duty {duty}");
            }
        }
        assert_eq!(fan_duty_of(0, 85), 70);
        assert_eq!(fan_duty_of(3, 95), 87);
        assert_eq!(fan_duty_of(0, 15), 5);
        // 3.5 rounds up.
        assert_eq!(fan_duty_of(0, 10), 4);
        assert_eq!(firmware_fan_pct(0, 70), 85);
        assert_eq!(firmware_fan_pct(3, 3), 10);
        assert_eq!(firmware_fan_pct(0, 1), 3);
        assert_eq!(firmware_fan_pct(0, 99), 100);
    }

    #[test]
    fn fan_duty_outside_calibrated_range() {
        for slot in [0, 3, 4] {
            for v in [101, 200, u8::MAX] {
                assert_eq!(fan_duty_of(slot, v), 100, "slot {slot} firmware {v}");
                assert_eq!(firmware_fan_pct(slot, v), 100, "slot {slot} duty {v}");
            }
        }
        // Every slot but 0 is a GPU-side fan.
        for slot in [1, 2, 5, FAN_SLOTS - 1, u32::MAX] {
            assert_eq!(fan_duty_of(slot, 80), 55, "slot {slot}");
            assert_eq!(firmware_fan_pct(slot, 55), 80, "slot {slot}");
        }
    }

    #[test]
    fn fan_duty_of_is_monotonic_and_invertible() {
        for slot in [0, 3] {
            let mut last = 0;
            for fw in 0..=100u8 {
                let d = fan_duty_of(slot, fw);
                assert!(d >= last && d <= 100, "slot {slot} firmware {fw}");
                last = d;
                // The low end is flat (0.3 duty points per firmware point), so a duty rounded to
                // a whole point can land up to 2 firmware points away.
                assert!(firmware_fan_pct(slot, d).abs_diff(fw) <= 2, "slot {slot} firmware {fw} -> {d}");
            }
        }
        for v in 0..=u8::MAX {
            assert!(fan_duty_of(3, v) <= fan_duty_of(0, v), "firmware {v}");
            assert!(firmware_fan_pct(3, v) >= firmware_fan_pct(0, v), "duty {v}");
        }
    }

    #[test]
    fn interpolate_both_directions() {
        let t = [(0, 0), (10, 100)];
        assert_eq!(interpolate(&t, 0, false), 0);
        assert_eq!(interpolate(&t, 5, false), 50);
        assert_eq!(interpolate(&t, 10, false), 100);
        assert_eq!(interpolate(&t, 50, true), 5);
        // Past the last point: its value.
        assert_eq!(interpolate(&t, 11, false), 100);
        assert_eq!(interpolate(&t, u8::MAX, true), 10);
    }

    #[test]
    fn fan_speed_input_for_every_duty() {
        for slot in [0, 3, 4] {
            for d in 0..=100 {
                let v = fan_speed_input(slot, d).unwrap();
                assert_eq!(v & 0xFF, fan_speed_id(slot));
                assert_eq!(v >> 8, u64::from(firmware_fan_pct(slot, d)));
                assert!(v >> 8 <= 100);
            }
            for d in 101..=u8::MAX {
                assert_eq!(fan_speed_input(slot, d), None);
            }
        }
        assert_eq!(fan_speed_input(0, 100), Some(0x6401));
        assert_eq!(fan_speed_input(4, 100), Some(0x6405));
    }

    #[test]
    fn turbo_led_reads_two_bits() {
        assert_eq!(decode_turbo_led(0x200), Ok(false));
        assert_eq!(decode_turbo_led(0x300), Ok(false));
        // Bits above bit 1 are ignored.
        assert_eq!(decode_turbo_led(0x500), Ok(true));
        assert_eq!(decode_turbo_led(0xFFFF_FD00), Ok(true));
        assert_eq!(decode_turbo_led(0xFFFF_FC00), Ok(false));
        assert_eq!(decode_turbo_led(0x2), Err(FirmwareError::InvalidParam));
        assert_eq!((TURBO_LED_ON & 0xFFFF, TURBO_LED_OFF & 0xFFFF), (TURBO_LED_ID, TURBO_LED_ID));
        assert_eq!((TURBO_LED_ON >> 16, TURBO_LED_OFF >> 16), (1, 0));
    }

    #[test]
    fn misc_round_trip_and_masking() {
        for idx in [MiscSetting::Turbo, MiscSetting::GpuMode, MiscSetting::GpuOc, MiscSetting::BootAnimation] {
            for v in 0..=u8::MAX {
                let input = misc_input(idx, v);
                assert_eq!(input & 0xFF, idx as u64);
                assert_eq!(decode_misc(input & !0xFF), Ok(v), "{idx:?} {v}");
            }
        }
        // One byte is read back.
        assert_eq!(decode_misc(0xFFFF_FF00), Ok(0xFF));
        assert_eq!(decode_misc(0x1_2300), Ok(0x23));
        // GPU OC is read back with 0x20 set; the caller masks it off.
        assert_eq!(decode_misc(0x2200).map(GpuOc::from_u8), Ok(None));
        assert_eq!(decode_misc(0x2200).map(|v| GpuOc::from_u8(v & 0x0F)), Ok(Some(GpuOc::Turbo)));
    }

    #[test]
    fn gpu_mode_and_oc_values() {
        for m in [GpuMode::Hybrid, GpuMode::Discrete] {
            assert_eq!(GpuMode::from_u8(m as u8), Some(m));
        }
        assert_eq!(GpuMode::from_u8(1), Some(GpuMode::Hybrid));
        assert_eq!(GpuMode::from_u8(u8::MAX), None);
        assert_eq!((GpuMode::Hybrid.label(), GpuMode::Discrete.label()), ("Hybrid (Optimus)", "Discrete only"));
        for (i, g) in GpuOc::ALL.iter().enumerate() {
            assert_eq!(*g as usize, i);
        }
        assert!(GpuOc::ALL.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(GpuOc::from_u8(u8::MAX), None);
        assert_eq!(GpuOc::ALL.map(GpuOc::label), ["Normal", "Faster", "Turbo"]);
    }

    #[test]
    fn running_gpu_mode_needs_intel_prefix() {
        // Intel's vendor id elsewhere in the id is not an Intel adapter.
        assert_eq!(running_gpu_mode(&["PCI\\VEN_10DE&DEV_1ED0&SUBSYS_80861025"]), GpuMode::Discrete);
        assert_eq!(running_gpu_mode(&["ROOT\\VEN_8086"]), GpuMode::Discrete);
        assert_eq!(running_gpu_mode(&["PCI\\VEN_808"]), GpuMode::Discrete);
        assert_eq!(running_gpu_mode(&[""]), GpuMode::Discrete);
        assert_eq!(running_gpu_mode(&["PCI\\VEN_8086"]), GpuMode::Hybrid);
        assert_eq!(running_gpu_mode(&["Pci\\Ven_8086&Dev_3E9B"]), GpuMode::Hybrid);
        let owned = vec![String::from("ROOT\\DISPLAY\\0000"), String::from("PCI\\VEN_8086&DEV_3E9B")];
        assert_eq!(running_gpu_mode(&owned), GpuMode::Hybrid);
    }

    #[test]
    fn coolboost_limits() {
        assert_eq!(COOLBOOST_GET & 0xFF, 7);
        assert_eq!(coolboost_set_input(true) & 0xFFFF, 7);
        assert_eq!(decode_coolboost(0x200), Ok(false));
        assert_eq!(decode_coolboost(0xFF00), Ok(false));
        // Only byte 1 is the state.
        assert_eq!(decode_coolboost(0xFFFF_0100), Ok(true));
        assert_eq!(decode_coolboost(0x2), Err(FirmwareError::InvalidParam));
    }

    #[test]
    fn profile_word_round_trips() {
        assert_eq!(PROFILE_GET, 0);
        for on in [false, true] {
            // Get returns the bytes in the places the setter writes them; byte 0 becomes the status.
            assert_eq!(decode_win_key(win_key_set_input(on) & !0xFF), Ok(on));
            assert_eq!(decode_lcd_overdrive(lcd_overdrive_set_input(on) & !0xFF), Ok(on));
        }
        // The select bit for byte n is bit n - 2.
        assert_eq!(win_key_set_input(true) & 0xFF, 1 << (3 - 2));
        assert_eq!(lcd_overdrive_set_input(true) & 0xFF, 1 << (6 - 2));
        // Each setting reads only its own byte.
        let others = !0xFF & !(0xFF << 24) & !(0xFF << 48);
        assert_eq!(decode_win_key(others), Ok(false));
        assert_eq!(decode_lcd_overdrive(others), Ok(false));
        assert_eq!(decode_win_key(others | 1 << 24), Ok(true));
        assert_eq!(decode_lcd_overdrive(others | 1 << 48), Ok(true));
        // Only 1 means on.
        assert_eq!(decode_win_key(2 << 24), Ok(false));
        assert_eq!(decode_win_key(0xFF << 24), Ok(false));
        assert_eq!(decode_lcd_overdrive(2 << 48), Ok(false));
        assert_eq!(decode_lcd_overdrive(0xFF << 48), Ok(false));
    }

    #[test]
    fn kb_timeout_round_trip_and_limits() {
        for b in 0..=u8::MAX {
            for s in 0..=u8::MAX {
                assert_eq!(decode_kb_timeout(kb_timeout_set_input(b, s) & !0xFF), Ok((b, s)));
            }
        }
        // Set differs from get only in byte 0 (read 1, write 2).
        assert_eq!(kb_timeout_set_input(0, 0), KB_TIMEOUT_GET + 1);
        assert_eq!(decode_kb_timeout(!0xFF), Ok((0xFF, 0xFF)));
        assert_eq!(decode_kb_timeout(0xFFFF_0000_0000_0000), Ok((0, 0)));
        assert_eq!(decode_kb_timeout(0xFFFF_FF00), Ok((0, 0)));
        assert_eq!(decode_kb_timeout(0x1), Err(FirmwareError::Unsupported));
    }

    #[test]
    fn kb_timeout_labels() {
        assert_eq!(kb_timeout_label(1), "1 s");
        assert_eq!(kb_timeout_label(59), "59 s");
        assert_eq!(kb_timeout_label(60), "1 min");
        assert_eq!(kb_timeout_label(90), "90 s");
        assert_eq!(kb_timeout_label(240), "4 min");
        assert_eq!(kb_timeout_label(u8::MAX), "255 s");
        assert!(all_distinct(&KB_TIMEOUTS.map(kb_timeout_label)));
    }

    #[test]
    fn usb_charge_round_trip_and_limits() {
        for f in USB_CHARGE_FLOORS {
            assert_eq!(usb_charge_set_input(f) & 0xFF, USB_CHARGE_GET & 0xFF);
            assert_eq!(decode_usb_charge(usb_charge_set_input(f) & !0xFF), Ok(f), "floor {f}");
        }
        assert_eq!(usb_charge_set_input(10), 0x0A_0F04);
        assert_eq!(usb_charge_set_input(u8::MAX), 0xFF_0F04);
        // On with floors the window does not offer.
        assert_eq!(decode_usb_charge(0x00_0F00), Ok(0));
        assert_eq!(decode_usb_charge(0x64_0F00), Ok(100));
        assert_eq!(decode_usb_charge(0xFF_0F00), Ok(0xFF));
        // Bytes above the floor are ignored.
        assert_eq!(decode_usb_charge(0xFFFF_FF14_0F00), Ok(20));
        // Any state but 0x0F reads as off.
        for state in [0x00, 0x0E, 0x10, 0x1F, 0x8F, 0xFF] {
            assert_eq!(decode_usb_charge(0x14_0000 | state << 8), Ok(0), "state {state:#x}");
        }
        assert_eq!(usb_charge_label(100), "Until 100%");
    }

    #[test]
    fn rgb_hex_edge_cases() {
        for s in [
            "#",
            "##00AEC7",
            "#00 AEC7",
            " 00AEC7",
            "00AEC7 ",
            "#00AEC7\n",
            "00AEC",
            "0x00AE",
            "-0AEC7",
            "00AEC7#",
            // Six bytes, but not six ASCII digits.
            "#ÿÿÿ",
            "€€",
            "１２",
        ] {
            assert_eq!(Rgb::parse_hex(s), None, "{s:?}");
        }
        assert_eq!(Rgb::parse_hex("#FFFFFF"), Some(Rgb::new(0xFF, 0xFF, 0xFF)));
        assert_eq!(Rgb::parse_hex("000000"), Some(Rgb::default()));
        assert_eq!(Rgb::parse_hex("#aBcDeF"), Some(Rgb::new(0xAB, 0xCD, 0xEF)));
        assert_eq!(Rgb::parse_hex("#abcdef").map(Rgb::to_hex).as_deref(), Some("#ABCDEF"));
    }

    #[test]
    fn rgb_round_trips() {
        for i in 0..=u8::MAX {
            let c = Rgb::new(i, i.wrapping_mul(7), u8::MAX - i);
            let hex = c.to_hex();
            assert_eq!(hex.len(), 7);
            assert_eq!(Rgb::parse_hex(&hex), Some(c));
            assert_eq!(Rgb::parse_hex(&hex[1..]), Some(c));
            assert_eq!(Rgb::from_colorref(c.colorref()), c);
            assert_eq!(c.colorref() >> 24, 0);
        }
        assert_eq!(Rgb::ACER_CYAN.colorref(), 0x00C7_AE00);
        assert_eq!(Rgb::from_colorref(u32::MAX), Rgb::new(0xFF, 0xFF, 0xFF));
        // The high byte of a COLORREF is ignored.
        assert_eq!(Rgb::from_colorref(0xFF00_0000), Rgb::default());
    }

    #[test]
    fn rgb_zone_masks_and_read_back() {
        let c = Rgb::new(0x12, 0x34, 0x56);
        for m in 1..16u8 {
            let input = rgb_zone_input(m, c).unwrap();
            assert_eq!(input & 0xFF, u64::from(m));
            // GetGamingRgbKb returns the color in the bytes the setter takes it in.
            assert_eq!(decode_rgb(input & !0xFF), Ok(c));
        }
        for m in 16..=u8::MAX {
            assert_eq!(rgb_zone_input(m, c), None, "mask {m:#x}");
        }
        assert_eq!(rgb_zone_input(0xF, Rgb::new(0xFF, 0xFF, 0xFF)), Some(0xFFFF_FF0F));
        // Bytes above blue are ignored.
        assert_eq!(decode_rgb(0xFFFF_0302_0100), Ok(Rgb::new(1, 2, 3)));
    }

    #[test]
    fn kb_payload_for_every_effect() {
        let kb = KbBacklight {
            effect: KbEffect::Static,
            speed: 5,
            brightness: 60,
            direction: Direction::RightToLeft,
            color: Rgb::new(1, 2, 3),
        };
        let expected = [
            [0, 0, 60, 0, 0, 0, 0, 0],
            [1, 5, 60, 0, 0, 1, 2, 3],
            [2, 5, 60, 0, 0, 1, 2, 3],
            [3, 5, 60, 8, 1, 0, 0, 0],
            [4, 5, 60, 8, 0, 0, 0, 0],
            [5, 5, 60, 0, 1, 1, 2, 3],
        ];
        for (e, want) in KbEffect::ALL.into_iter().zip(expected) {
            let p = kb_backlight_payload(&KbBacklight { effect: e, ..kb });
            assert_eq!(p[..8], want, "{e:?}");
            assert_eq!(p[8..], want, "{e:?}");
        }
    }

    #[test]
    fn kb_payload_limits() {
        let kb = KbBacklight { effect: KbEffect::Breathing, ..KbBacklight::default() };
        for (speed, sent) in [(0, 0), (KB_MAX_SPEED, 9), (10, 9), (u8::MAX, 9)] {
            assert_eq!(kb_backlight_payload(&KbBacklight { speed, ..kb })[1], sent, "speed {speed}");
        }
        for (brightness, sent) in [(0, 0), (KB_MAX_BRIGHTNESS, 100), (101, 100), (u8::MAX, 100)] {
            assert_eq!(kb_backlight_payload(&KbBacklight { brightness, ..kb })[2], sent, "brightness {brightness}");
        }
        // Static at brightness 0 sends nothing at all, whatever else is set.
        let dark = KbBacklight {
            effect: KbEffect::Static,
            speed: u8::MAX,
            brightness: 0,
            direction: Direction::LeftToRight,
            color: Rgb::new(0xFF, 0xFF, 0xFF),
        };
        assert_eq!(kb_backlight_payload(&dark), [0; 16]);
        assert_eq!(kb_backlight_payload(&KbBacklight::default())[..8], [0, 0, 100, 0, 0, 0, 0, 0]);
        assert_eq!(KB_MAX_BRIGHTNESS % KB_BRIGHTNESS_STEP, 0);
    }

    #[test]
    fn kb_effect_table() {
        for (i, e) in KbEffect::ALL.iter().enumerate() {
            assert_eq!(*e as usize, i);
        }
        assert_eq!(KbEffect::from_u8(u8::MAX), None);
        assert_eq!(KbEffect::ALL.map(KbEffect::label), ["Static", "Breathing", "Neon", "Wave", "Shifting", "Zoom"]);
        assert_eq!(KbEffect::ALL.map(KbEffect::uses_color), [false, true, true, false, false, true]);
        assert_eq!(KbEffect::ALL.map(KbEffect::uses_direction), [false, false, false, true, false, true]);
        assert_eq!(KbEffect::ALL.map(KbEffect::pattern), [0, 0, 0, 8, 8, 0]);
        assert_eq!((Direction::RightToLeft as u8, Direction::LeftToRight as u8), (1, 2));
    }

    #[test]
    fn event_edge_cases() {
        assert_eq!(decode_event(&[1]), None);
        assert_eq!(decode_event(&[4]), None);
        assert_eq!(decode_event(&[1, 0xFF]), Some(HwEvent::KbBacklightChanged));
        assert_eq!(decode_event(&[1, 4]), Some(HwEvent::KbBacklightChanged));
        assert_eq!(decode_event(&[4, 7]), Some(HwEvent::Other(4, 7)));
        assert_eq!(decode_event(&[7, 0]), Some(HwEvent::Other(7, 0)));
        assert_eq!(decode_event(&[0, 0]), Some(HwEvent::Other(0, 0)));
        assert_eq!(decode_event(&[0xFF, 0xFF]), Some(HwEvent::Other(0xFF, 0xFF)));
        // Only the first two bytes matter, however long the detail is.
        assert_eq!(decode_event(&[7, 4, 1, 0xFF, 0, 0, 0, 0, 9, 9, 9]), Some(HwEvent::TurboKey));
        assert_eq!(decode_event(&[2, 3, 7, 4]), Some(HwEvent::Other(2, 3)));
    }

    #[test]
    fn predator_key_every_combination() {
        assert_eq!((SCAN_PREDATOR_KEY, VK_PREDATOR_KEY, SCAN_LAUNCH_APP1, VK_LAUNCH_APP1), (0x75, 0xFF, 0x6B, 0xB6));
        // Scan codes come without the E0 prefix, which is the separate flag.
        let scans = [0x75, 0x6B, 0, 0x74, 0x76, 0x6A, 0x6C, 0xE075, 0xE06B, u16::MAX];
        let vkeys = [0xFF, 0xB6, 0, 0xB5, 0xB7, 0x1FF, u16::MAX];
        for scan in scans {
            for e0 in [false, true] {
                for vkey in vkeys {
                    let expected = e0 && matches!((scan, vkey), (0x75, 0xFF | 0xB6) | (0x6B, 0xB6));
                    assert_eq!(is_predator_key(scan, e0, vkey), expected, "{scan:#x} {e0} {vkey:#x}");
                }
            }
        }
    }
}
