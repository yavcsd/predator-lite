//! Read-only 30 s capture of every source a hardware button could use:
//! all WMI events in ROOT\WMI (minus chatty kernel/network classes), the keyboard (non-typing keys
//! only) and raw input from system-control, radio, consumer and vendor HID collections.
//!
//! Run elevated: `capture_button_inputs.exe` <report-file>

mod common;

use common::{capture_for_30s, log, save_log};
use predator_lite::system::size_u32;
use predator_lite::wmi::{init_security, EventStream};
use windows::core::w;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, RegisterRawInputDevices, HRAWINPUT, RAWHID, RAWINPUT, RAWINPUTDEVICE,
    RAWINPUTHEADER, RIDEV_INPUTSINK, RIDEV_PAGEONLY, RIDI_DEVICENAME, RID_INPUT, RIM_TYPEHID,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, RegisterClassW, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_INPUT, WNDCLASSW,
};

/// Event classes that fire constantly on their own; counted, not logged.
fn noisy(class: &str) -> bool {
    ["Kernel", "MSNdis", "IdleAccounting", "MSMCAEvent", "WmiMonitor", "MSStorageDriver", "MSAcpi_Thermal"]
        .iter()
        .any(|p| class.starts_with(p))
}

/// Device path of a raw input device, for the log.
unsafe fn device_name(h: HANDLE) -> String {
    let mut buf = [0u16; 260];
    let mut n = u32::try_from(buf.len()).unwrap();
    let r = GetRawInputDeviceInfoW(Some(h), RIDI_DEVICENAME, Some(buf.as_mut_ptr().cast()), &raw mut n);
    if r == u32::MAX || r == 0 {
        return "?".into();
    }
    String::from_utf16_lossy(&buf[..r as usize]).trim_end_matches('\0').to_string()
}

/// Message-only window procedure: logs HID raw input reports.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_INPUT {
        // First call: size only.
        let mut size = 0u32;
        let hdr = size_u32::<RAWINPUTHEADER>();
        GetRawInputData(HRAWINPUT(lp.0 as *mut _), RID_INPUT, None, &raw mut size, hdr);
        // u64 elements keep the buffer aligned for RAWINPUT (8 bytes); a Vec<u8> need not be.
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        if GetRawInputData(HRAWINPUT(lp.0 as *mut _), RID_INPUT, Some(buf.as_mut_ptr().cast()), &raw mut size, hdr)
            == size
        {
            let ri = &*buf.as_ptr().cast::<RAWINPUT>();
            let bytes = std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), size as usize);
            if ri.header.dwType == RIM_TYPEHID.0 {
                let hid = &ri.data.hid;
                let len = (hid.dwSizeHid * hid.dwCount) as usize;
                let off = std::mem::offset_of!(RAWINPUT, data) + std::mem::offset_of!(RAWHID, bRawData);
                let data = &bytes[off..(off + len).min(bytes.len())];
                log(&format!("HID {} report {:02X?}", device_name(ri.header.hDevice), data));
            }
        }
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "capture_button_inputs.txt".into());
    log("start");
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    init_security();

    std::thread::spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        match EventStream::open_query("SELECT * FROM WMIEvent") {
            Ok(es) => {
                log("WMI: subscribed to all WMIEvent classes in ROOT\\WMI");
                let mut skipped = 0u32;
                loop {
                    match es.next_any("EventDetail") {
                        Ok((class, _)) if noisy(&class) => skipped += 1,
                        Ok((class, d)) => {
                            log(&format!("WMI {class} EventDetail={d:02X?} (noisy skipped so far: {skipped})"));
                        }
                        Err(e) => {
                            log(&format!("WMI error: {e}"));
                            break;
                        }
                    }
                }
            }
            Err(e) => log(&format!("WMI subscribe failed: {e}")),
        }
    });

    unsafe {
        let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: w!("CaptureAll"),
            ..Default::default()
        };
        RegisterClassW(&raw const wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("CaptureAll"),
            w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinst.into()),
            None,
        )
        .unwrap_or_default();
        // (usage page, usage): system control, wireless radio, consumer, vendor pages (page-only).
        let mut devs = vec![
            RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x80, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd },
            RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x0C, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd },
            RAWINPUTDEVICE { usUsagePage: 0x0C, usUsage: 0x01, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd },
        ];
        for page in [0xFF00u16, 0xFF01, 0xFF02, 0xFF31, 0xFFA0, 0xFFC0, 0xFF60] {
            devs.push(RAWINPUTDEVICE {
                usUsagePage: page,
                usUsage: 0,
                dwFlags: RIDEV_INPUTSINK | RIDEV_PAGEONLY,
                hwndTarget: hwnd,
            });
        }
        for d in &devs {
            let r = RegisterRawInputDevices(std::slice::from_ref(d), size_u32::<RAWINPUTDEVICE>());
            log(&format!(
                "raw input page {:#06x} usage {:#04x}: {}",
                d.usUsagePage,
                d.usUsage,
                if r.is_ok() { "ok" } else { "failed" }
            ));
        }
    }
    capture_for_30s("capture started: press the Turbo button now");
    save_log(&out);
}
