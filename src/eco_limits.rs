//! Eco mode's limits on the Windows side.
//!
//! CPU: a "Predator Lite Eco" power plan (a copy of the plan that was active the first time) with
//! processor Turbo Boost disabled. The user's own plan is never modified: it is recorded in
//! `%APPDATA%\PredatorLite\eco_prev_plan` and re-activated when Eco ends, also after a crash or a
//! restart.
//! GPU: NVIDIA clock cap through `nvidia-smi -lgc` / `-rgc` (laptop GPUs refuse power limits).
//!
//! The firmware side of Eco (fans, GPU level) is in `controller`; this module only implements
//! its `EcoLimiter`.

use crate::controller::{EcoLimiter, Error, Result};
use crate::system::{hidden_command, size_u32, system32_exe};
use std::path::PathBuf;
use windows::core::GUID;
use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL, WIN32_ERROR};
use windows::Win32::System::Power::{
    PowerDuplicateScheme, PowerEnumerate, PowerGetActiveScheme, PowerReadFriendlyName, PowerSetActiveScheme,
    PowerWriteACValueIndex, PowerWriteDCValueIndex, PowerWriteFriendlyName, ACCESS_SCHEME,
};

/// Name of the power plan Eco creates and activates.
const PLAN_NAME: &str = "Predator Lite Eco";
/// Name the plan had when Eco was called Quiet; such a plan is renamed instead of duplicated.
const OLD_PLAN_NAME: &str = "Predator Lite Quiet";
/// File remembering the user's plan while Eco is on, and its name in older versions.
const PREV_PLAN_FILE: &str = "eco_prev_plan";
const OLD_PREV_PLAN_FILE: &str = "quiet_prev_plan";

/// Power setting subgroup "Processor power management".
const SUB_PROCESSOR: GUID = GUID::from_u128(0x54533251_82be_4824_96c1_47b60b740d00);
/// Setting "Processor performance boost mode" (0 = disabled).
const PERF_BOOST_MODE: GUID = GUID::from_u128(0xbe337238_0d82_4146_a960_4f3749d470c7);
/// Windows' built-in Balanced plan: the fallback when the user's plan is unknown.
const WINDOWS_BALANCED: GUID = GUID::from_u128(0x381b4222_f694_41f0_9685_ff5bb260df2e);
/// Lowest clock of the `nvidia-smi -lgc` range (MHz); the cap is the highest.
const GPU_MIN_MHZ: u16 = 300;

/// Turn a Win32 error code into `Error::Os` naming the failed call.
fn check_win32(what: &str, e: WIN32_ERROR) -> Result<()> {
    if e == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(Error::Os(format!("{what} failed ({})", e.0)))
    }
}

/// Copy out a GUID the power API allocated, and free it.
unsafe fn take_guid(ptr: *mut GUID) -> GUID {
    let guid = *ptr;
    let _ = LocalFree(Some(HLOCAL(ptr.cast())));
    guid
}

fn active_plan() -> Result<GUID> {
    let mut ptr: *mut GUID = std::ptr::null_mut();
    unsafe {
        check_win32("PowerGetActiveScheme", PowerGetActiveScheme(None, &raw mut ptr))?;
        Ok(take_guid(ptr))
    }
}

fn set_active_plan(plan: &GUID) -> Result<()> {
    check_win32("PowerSetActiveScheme", unsafe { PowerSetActiveScheme(None, Some(plan)) })
}

/// The plan's display name (UTF-16 in the API).
fn plan_name(plan: &GUID) -> Option<String> {
    let mut size = 0u32;
    unsafe {
        // First call: size only.
        let _ = PowerReadFriendlyName(None, Some(plan), None, None, None, &raw mut size);
        if size < 2 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        if PowerReadFriendlyName(None, Some(plan), None, None, Some(buf.as_mut_ptr()), &raw mut size) != ERROR_SUCCESS {
            return None;
        }
        let utf16: Vec<u16> =
            buf.as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes(*pair)).take_while(|&unit| unit != 0).collect();
        Some(String::from_utf16_lossy(&utf16))
    }
}

fn set_plan_name(plan: &GUID, name: &str) -> Result<()> {
    let name: Vec<u8> = name.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
    check_win32("PowerWriteFriendlyName", unsafe { PowerWriteFriendlyName(None, plan, None, None, &name) })
}

/// The Eco plan, if it exists. A plan left by an older version under the old name is renamed.
fn find_eco_plan() -> Option<GUID> {
    for i in 0..256 {
        let mut plan = GUID::zeroed();
        let mut size = size_u32::<GUID>();
        let buffer = Some((&raw mut plan).cast::<u8>());
        let r = unsafe { PowerEnumerate(None, None, None, ACCESS_SCHEME, i, buffer, &raw mut size) };
        if r != ERROR_SUCCESS {
            return None;
        }
        match plan_name(&plan).as_deref() {
            Some(PLAN_NAME) => return Some(plan),
            Some(OLD_PLAN_NAME) => {
                let _ = set_plan_name(&plan, PLAN_NAME);
                return Some(plan);
            }
            _ => {}
        }
    }
    None
}

/// Copy `base` (the user's plan) as the Eco plan.
fn create_eco_plan(base: &GUID) -> Result<GUID> {
    let mut ptr: *mut GUID = std::ptr::null_mut();
    let plan = unsafe {
        check_win32("PowerDuplicateScheme", PowerDuplicateScheme(None, base, &raw mut ptr))?;
        take_guid(ptr)
    };
    set_plan_name(&plan, PLAN_NAME)?;
    Ok(plan)
}

/// Turbo Boost off, on AC and on battery.
fn configure_eco_plan(plan: &GUID) -> Result<()> {
    unsafe {
        let ac = PowerWriteACValueIndex(None, plan, Some(&SUB_PROCESSOR), Some(&PERF_BOOST_MODE), 0);
        check_win32("PowerWriteACValueIndex", ac)?;
        let dc = PowerWriteDCValueIndex(None, plan, Some(&SUB_PROCESSOR), Some(&PERF_BOOST_MODE), 0);
        check_win32("PowerWriteDCValueIndex", WIN32_ERROR(dc))
    }
}

/// A file in `%APPDATA%\PredatorLite`.
fn app_data_file(name: &str) -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("PredatorLite").join(name))
}

/// The user's plan recorded when Eco started (also under the old file name).
fn read_prev_plan() -> Option<GUID> {
    [PREV_PLAN_FILE, OLD_PREV_PLAN_FILE].into_iter().find_map(|name| {
        let text = std::fs::read_to_string(app_data_file(name)?).ok()?;
        u128::from_str_radix(text.trim(), 16).ok().map(GUID::from_u128)
    })
}

/// Record the user's plan (as 32 hex digits) so it can be restored, even after a crash.
fn write_prev_plan(plan: &GUID) {
    if let Some(path) = app_data_file(PREV_PLAN_FILE) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, format!("{:032x}", plan.to_u128()));
    }
}

fn forget_prev_plan() {
    for name in [PREV_PLAN_FILE, OLD_PREV_PLAN_FILE] {
        if let Some(path) = app_data_file(name) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Run `nvidia-smi` (from System32, where the NVIDIA driver installs it) without a console
/// window; the error is its first output line.
fn nvidia_smi(args: &[&str]) -> std::result::Result<(), String> {
    if !system32_exe("nvidia-smi.exe").exists() {
        return Err("nvidia-smi not found".into());
    }
    let out = hidden_command("nvidia-smi.exe").args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    Err(first_output_line(&out.stdout, &out.stderr).unwrap_or_else(|| "nvidia-smi failed".into()))
}

/// The first non-empty line a program printed, stdout before stderr. The two streams are read
/// separately, so a last stdout line without a newline never merges with stderr's first line.
fn first_output_line(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    let (stdout, stderr) = (String::from_utf8_lossy(stdout), String::from_utf8_lossy(stderr));
    stdout.lines().chain(stderr.lines()).map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

/// Eco limits through the Windows power plan and nvidia-smi. Stateless: what is active is read
/// from Windows and the `eco_prev_plan` file each time.
#[derive(Default)]
pub struct WindowsEcoLimiter;

impl EcoLimiter for WindowsEcoLimiter {
    /// A power-plan failure is an error (nothing to fall back on); a GPU cap failure is only a
    /// note, since the CPU part already worked.
    fn set_eco(&mut self, on: bool, gpu_mhz: u16) -> Result<Option<String>> {
        let active = active_plan()?;
        let eco_plan = find_eco_plan();
        if on {
            // Remember the user's plan (unless ours is already active, e.g. after a crash).
            if Some(active) != eco_plan {
                write_prev_plan(&active);
            }
            let plan = match eco_plan {
                Some(p) => p,
                None => create_eco_plan(&active)?,
            };
            configure_eco_plan(&plan)?;
            set_active_plan(&plan)?;
            if gpu_mhz == 0 {
                return Ok(None);
            }
            let capped = nvidia_smi(&["-lgc", &format!("{GPU_MIN_MHZ},{gpu_mhz}")]);
            return Ok(capped.err().map(|e| format!("GPU clock cap not applied: {e}")));
        }
        // Off. A cheap no-op when Eco was not active (this runs at every start, for crash recovery).
        let prev = read_prev_plan();
        let ours_active = eco_plan.is_some() && Some(active) == eco_plan;
        if !ours_active && prev.is_none() {
            return Ok(None);
        }
        if ours_active {
            let back = prev.filter(|g| Some(*g) != eco_plan).unwrap_or(WINDOWS_BALANCED);
            set_active_plan(&back).or_else(|_| set_active_plan(&WINDOWS_BALANCED))?;
        }
        forget_prev_plan();
        // Reset the clocks; a failure only matters when a cap was set.
        let reset = nvidia_smi(&["-rgc"]);
        Ok(reset.err().filter(|_| gpu_mhz != 0).map(|e| format!("GPU clock cap not removed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::ERROR_ACCESS_DENIED;

    #[test]
    fn win32_success_is_ok_and_failure_names_the_call() {
        assert_eq!(check_win32("PowerSetActiveScheme", ERROR_SUCCESS), Ok(()));
        assert_eq!(
            check_win32("PowerSetActiveScheme", ERROR_ACCESS_DENIED),
            Err(Error::Os("PowerSetActiveScheme failed (5)".into()))
        );
        assert_eq!(check_win32("x", WIN32_ERROR(u32::MAX)), Err(Error::Os(format!("x failed ({})", u32::MAX))));
    }

    #[test]
    fn power_guids_match_the_documented_values() {
        let fields = |g: GUID| (g.data1, g.data2, g.data3, g.data4);
        assert_eq!(
            fields(SUB_PROCESSOR),
            (0x5453_3251, 0x82be, 0x4824, [0x96, 0xc1, 0x47, 0xb6, 0x0b, 0x74, 0x0d, 0x00])
        );
        assert_eq!(
            fields(PERF_BOOST_MODE),
            (0xbe33_7238, 0x0d82, 0x4146, [0xa9, 0x60, 0x4f, 0x37, 0x49, 0xd4, 0x70, 0xc7])
        );
        assert_eq!(
            fields(WINDOWS_BALANCED),
            (0x381b_4222, 0xf694, 0x41f0, [0x96, 0x85, 0xff, 0x5b, 0xb2, 0x60, 0xdf, 0x2e])
        );
    }

    #[test]
    fn nvidia_smi_error_is_the_first_non_empty_line() {
        assert_eq!(
            first_output_line(b"\n  Failed to set clocks  \nmore\n", b"err"),
            Some("Failed to set clocks".into())
        );
        // A last stdout line without a newline stays separate from stderr.
        assert_eq!(first_output_line(b"A", b"B\n"), Some("A".into()));
        assert_eq!(first_output_line(b"", b"\r\nNo devices were found\r\n"), Some("No devices were found".into()));
        assert_eq!(first_output_line(b" \n\t\n", b""), None);
        assert_eq!(first_output_line(b"\xFF\xFE bad utf-8", b""), Some("\u{FFFD}\u{FFFD} bad utf-8".into()));
    }

    #[test]
    fn plan_and_file_names_are_distinct() {
        assert_ne!(PLAN_NAME, OLD_PLAN_NAME);
        assert_ne!(PREV_PLAN_FILE, OLD_PREV_PLAN_FILE);
        assert!(PLAN_NAME.starts_with("Predator Lite") && OLD_PLAN_NAME.starts_with("Predator Lite"));
    }

    #[test]
    fn app_data_files_live_in_the_app_folder() {
        if let Some(path) = app_data_file(PREV_PLAN_FILE) {
            assert!(path.ends_with(r"PredatorLite\eco_prev_plan"), "{path:?}");
        }
    }
}
