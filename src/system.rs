//! Small Windows helpers shared by the window, Eco's limits and the example tools: starting
//! System32 programs without a console window, reading the power status, listing the display
//! adapters, remembering a GPU mode switch until the next restart, and struct sizes for Win32
//! calls.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Graphics::Gdi::{EnumDisplayDevicesW, DISPLAY_DEVICEW};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegGetValueW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_SET_VALUE, REG_DWORD,
    REG_OPTION_VOLATILE, RRF_RT_REG_DWORD,
};

/// Process creation flag: no console window for the child.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Volatile registry key (Windows drops it at the next restart) holding the GPU mode chosen in
/// this Windows session. HKLM, like the firmware setting it mirrors: it is machine-wide and a
/// sign-out must not lose it.
const PENDING_GPU_MODE_KEY: PCWSTR = w!("SOFTWARE\\PredatorLite");
const PENDING_GPU_MODE_VALUE: PCWSTR = w!("PendingGpuMode");
/// `SYSTEM_POWER_STATUS::BatteryFlag`: no system battery.
const BATTERY_FLAG_NONE: u8 = 128;

/// `size_of::<T>()` as the `u32` that Win32 size fields (`cbSize`, `dwSize`, ...) take.
#[must_use]
#[allow(clippy::cast_possible_truncation, reason = "Win32 structs are a few hundred bytes")]
pub const fn size_u32<T>() -> u32 {
    size_of::<T>() as u32
}

/// Path of a program in `%SystemRoot%\System32`.
#[must_use]
pub fn system32_exe(name: &str) -> PathBuf {
    let system_root = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()));
    system_root.join("System32").join(name)
}

/// A command for a System32 program that runs without a console window.
#[must_use]
pub fn hidden_command(exe_name: &str) -> Command {
    let mut command = Command::new(system32_exe(exe_name));
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// (on AC, battery percent if known), for `PowerGate::update`.
#[must_use]
pub fn power_status() -> (bool, Option<u8>) {
    let mut ps = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&raw mut ps) }.is_err() {
        return (true, None);
    }
    // ACLineStatus 255 = unknown: assume AC so a desktop-like setup is not locked out.
    let on_ac = ps.ACLineStatus != 0;
    let known = ps.BatteryLifePercent <= 100 && ps.BatteryFlag & BATTERY_FLAG_NONE == 0;
    (on_ac, known.then_some(ps.BatteryLifePercent))
}

/// Device ids of the display adapters Windows sees (e.g. `PCI\VEN_8086&DEV_3E9B&...`), for
/// `protocol::running_gpu_mode`.
#[must_use]
pub fn display_adapter_ids() -> Vec<String> {
    let mut ids = Vec::new();
    for i in 0..16 {
        let mut dd = DISPLAY_DEVICEW { cb: size_u32::<DISPLAY_DEVICEW>(), ..Default::default() };
        if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &raw mut dd, 0) }.as_bool() {
            break;
        }
        let n = dd.DeviceID.iter().position(|&c| c == 0).unwrap_or(dd.DeviceID.len());
        ids.push(String::from_utf16_lossy(&dd.DeviceID[..n]));
    }
    ids
}

/// The GPU mode value (`GpuMode as u8`) chosen since Windows started, if any. The firmware's
/// own value can't tell: on the PT515-51 it reads Discrete after every start.
#[must_use]
pub fn pending_gpu_mode() -> Option<u8> {
    read_dword(HKEY_LOCAL_MACHINE, PENDING_GPU_MODE_KEY, PENDING_GPU_MODE_VALUE).and_then(|v| u8::try_from(v).ok())
}

/// Remember a GPU mode switch until the next restart (needs administrator rights).
///
/// # Errors
/// The registry error, if the key can't be created or written.
pub fn remember_pending_gpu_mode(mode: u8) -> windows::core::Result<()> {
    write_volatile_dword(HKEY_LOCAL_MACHINE, PENDING_GPU_MODE_KEY, PENDING_GPU_MODE_VALUE, mode.into())
}

/// Write a DWORD, creating `subkey` as a volatile key if it doesn't exist.
fn write_volatile_dword(root: HKEY, subkey: PCWSTR, name: PCWSTR, v: u32) -> windows::core::Result<()> {
    let mut key = HKEY::default();
    unsafe {
        let r = RegCreateKeyExW(
            root,
            subkey,
            None,
            PCWSTR::null(),
            REG_OPTION_VOLATILE,
            KEY_SET_VALUE,
            None,
            &raw mut key,
            None,
        );
        r.ok()?;
        let r = RegSetValueExW(key, name, None, REG_DWORD, Some(&v.to_le_bytes()));
        let _ = RegCloseKey(key);
        r.ok()
    }
}

/// A DWORD value, or `None` if the key or value is missing or not a DWORD.
fn read_dword(root: HKEY, subkey: PCWSTR, name: PCWSTR) -> Option<u32> {
    let mut v = 0u32;
    let mut size = size_u32::<u32>();
    let r = unsafe {
        RegGetValueW(root, subkey, name, RRF_RT_REG_DWORD, None, Some((&raw mut v).cast()), Some(&raw mut size))
    };
    (r == ERROR_SUCCESS).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_u32_matches_size_of() {
        assert_eq!(size_u32::<u64>(), 8);
        assert_eq!(usize::try_from(size_u32::<SYSTEM_POWER_STATUS>()), Ok(size_of::<SYSTEM_POWER_STATUS>()));
    }

    #[test]
    fn system32_exe_is_under_system32() {
        let path = system32_exe("cmd.exe");
        assert!(path.ends_with(r"System32\cmd.exe"), "{path:?}");
        assert!(path.is_absolute(), "{path:?}");
        assert_eq!(system32_exe("nvidia-smi.exe").parent(), path.parent());
    }

    #[test]
    fn hidden_command_runs_the_system32_program() {
        let command = hidden_command("shutdown.exe");
        assert_eq!(std::path::Path::new(command.get_program()), system32_exe("shutdown.exe"));
        assert_eq!(command.get_args().count(), 0);
    }

    #[test]
    fn power_status_is_consistent() {
        let (_, pct) = power_status();
        assert!(pct.is_none_or(|p| p <= 100), "{pct:?}");
    }

    #[test]
    fn display_adapters_are_listed() {
        // A CI machine may have no display adapter, so only the ids' form is checked.
        let ids = display_adapter_ids();
        assert!(ids.len() <= 16, "{ids:?}");
        assert!(ids.iter().all(|id| !id.contains('\0')), "{ids:?}");
    }

    #[test]
    fn volatile_dword_round_trip() {
        use windows::core::HSTRING;
        use windows::Win32::System::Registry::{RegDeleteKeyW, HKEY_CURRENT_USER};
        // HKCU so the test needs no administrator rights; the key is removed afterwards.
        let subkey = HSTRING::from(format!("Software\\PredatorLiteTest{}", std::process::id()));
        let subkey = PCWSTR(subkey.as_ptr());
        let name = w!("Value");
        assert_eq!(read_dword(HKEY_CURRENT_USER, subkey, name), None);
        write_volatile_dword(HKEY_CURRENT_USER, subkey, name, 2).unwrap();
        assert_eq!(read_dword(HKEY_CURRENT_USER, subkey, name), Some(2));
        write_volatile_dword(HKEY_CURRENT_USER, subkey, name, 1).unwrap();
        assert_eq!(read_dword(HKEY_CURRENT_USER, subkey, name), Some(1));
        assert_eq!(read_dword(HKEY_CURRENT_USER, subkey, w!("Missing")), None);
        assert_eq!(unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, subkey) }, ERROR_SUCCESS);
        assert_eq!(read_dword(HKEY_CURRENT_USER, subkey, name), None);
    }
}
