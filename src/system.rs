//! Small Windows helpers shared by the window, Eco's limits and the example tools: starting
//! System32 programs without a console window, reading the power status, and struct sizes for
//! Win32 calls.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

/// Process creation flag: no console window for the child.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
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
}
