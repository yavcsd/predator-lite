//! Native Win32 window + tray icon. No framework; standard controls only.
//!
//! `run` connects, restores the settings, builds the window and runs the message loop. All
//! state lives in one `App` reached from the window's `GWLP_USERDATA`; every user action goes
//! through the `Controller` and ends in `App::done` (status line, save, refresh controls).
//!
//! Inputs besides the controls: the tray icon, a WMI event thread (`APGeEvent` hotkeys), a timer
//! polling the Turbo button, raw keyboard input for the PredatorSense button, `--toggle` /
//! second-instance messages, and power broadcasts (battery, resume from sleep).
//!
//! Resource use: sensors are polled every 2 s only while the window is visible; when hidden in
//! the tray the process just blocks in `GetMessage` and the hotkey thread blocks inside WMI.

use crate::controller::{Capabilities, Controller, Error, TurboButton};
use crate::eco_limits::WindowsEcoLimiter;
use crate::protocol::{
    decode_event, format_sensor, is_predator_key, kb_timeout_label, next_gpu_mode, running_gpu_mode, usb_charge_label,
    Direction, FanGroup, FanMode, GpuMode, GpuOc, HwEvent, KbBacklight, KbEffect, Rgb, Sensor, KB_BRIGHTNESS_STEP,
    KB_MAX_BRIGHTNESS, KB_MAX_SPEED, KB_TIMEOUTS, KB_ZONES, SCAN_LAUNCH_APP1, SCAN_PREDATOR_KEY, USB_CHARGE_FLOORS,
    VK_LAUNCH_APP1, VK_PREDATOR_KEY,
};
use crate::settings;
use crate::system::{
    display_adapter_ids, hidden_command, pending_gpu_mode, power_status, remember_pending_gpu_mode, size_u32,
};
use crate::wmi::{EventStream, Wmi};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, COLORREF, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontIndirectW, CreateFontW, CreateSolidBrush,
    DeleteDC, DeleteObject, DrawEdge, DrawFocusRect, DrawTextW, FillRect, GetDC, GetSysColor, GetSysColorBrush,
    InflateRect, InvalidateRect, ReleaseDC, SelectObject, SetBkMode, SetTextColor, ANTIALIASED_QUALITY, BF_ADJUST,
    BF_RECT, CLIP_DEFAULT_PRECIS, COLOR_BTNFACE, COLOR_GRAYTEXT, DEFAULT_CHARSET, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    EDGE_RAISED, EDGE_SUNKEN, FW_HEAVY, HDC, HFONT, OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, CreateMutexW, GetCurrentThreadId};
use windows::Win32::UI::Controls::Dialogs::{ChooseColorW, CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW};
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, DRAWITEMSTRUCT, ICC_BAR_CLASSES, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX, ODS_DISABLED,
    ODS_FOCUS, ODS_SELECTED,
};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK,
    RID_INPUT, RIM_TYPEKEYBOARD,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_NONE, NIIF_NOSOUND, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW, NOTIFY_ICON_MESSAGE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, AllowSetForegroundWindow, AppendMenuW, BringWindowToTop, CreateIconIndirect, CreatePopupMenu,
    CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, FindWindowW,
    GetCursorPos, GetDlgCtrlID, GetDlgItem, GetForegroundWindow, GetMessageW, GetWindowLongPtrW, GetWindowTextW,
    GetWindowThreadProcessId, IsDialogMessageW, IsIconic, IsWindow, IsWindowVisible, KillTimer, LoadCursorW,
    MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SendMessageW,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowTextW, ShowWindow, SwitchToThisWindow,
    SystemParametersInfoW, TrackPopupMenu, TranslateMessage, ASFW_ANY, CW_USEDEFAULT, GWLP_USERDATA, HICON, HMENU,
    ICONINFO, IDC_ARROW, IDNO, IDYES, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONQUESTION, MB_OK, MB_YESNOCANCEL, MF_CHECKED,
    MF_SEPARATOR, MF_STRING, MF_UNCHECKED, MSG, NONCLIENTMETRICSW, RI_KEY_BREAK, RI_KEY_E0, SIZE_MINIMIZED,
    SIZE_RESTORED, SPI_GETNONCLIENTMETRICS, SW_HIDE, SW_SHOWNORMAL, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_APPCOMMAND, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_DRAWITEM, WM_HSCROLL, WM_INPUT, WM_LBUTTONDBLCLK, WM_LBUTTONUP,
    WM_POWERBROADCAST, WM_RBUTTONUP, WM_SETFONT, WM_SIZE, WM_TIMER, WM_USER, WNDCLASSEXW, WS_CAPTION, WS_CHILD,
    WS_GROUP, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

/// Window class; a second instance finds the running one by it.
const CLASS: PCWSTR = w!("PredatorLite.Main");
const TITLE: &str = "Predator Lite";

// Control ids. Each fan group's row uses the same ids offset by `group index * FAN_ID_STRIDE`.
const FAN_ID_STRIDE: i32 = 10;
const ID_FAN_RADIO: i32 = 100; // + group*stride + mode index (0..3)
const ID_FAN_TRACK: i32 = 103; // + group*stride
const ID_FAN_PCT: i32 = 104; // + group*stride
const ID_FAN_TRACK_GPU: i32 = ID_FAN_TRACK + fan_row(FanGroup::Gpu);
const ID_TURBO: i32 = 120;
const ID_GPU_OC: i32 = 121;
const ID_LCD: i32 = 122;
const ID_ECO: i32 = 123;
const ID_GPU_MODE: i32 = 124;
const ID_GPU_MODE_NOTE: i32 = 125;
const ID_COOLBOOST: i32 = 126;
const ID_BALANCED: i32 = 127;
const ID_WIN_KEY: i32 = 128;
const ID_BOOT: i32 = 129;
const ID_KB_EFFECT: i32 = 130;
const ID_KB_DIR: i32 = 131;
const ID_KB_BRIGHT: i32 = 132;
const ID_KB_SPEED: i32 = 133;
const ID_KB_TIMEOUT: i32 = 134;
const ID_USB_CHARGE: i32 = 135;
const ID_ZONE: i32 = 140; // + zone
const ID_KB_COLOR: i32 = 144;
const ID_SENSOR: i32 = 150; // + index in Sensor::ALL
const ID_STATUS: i32 = 160;
// Tray menu items.
const IDM_SHOW: usize = 200;
const IDM_TURBO: usize = 201;
const IDM_EXIT: usize = 202;
const IDM_TURBO_BUTTON: usize = 203;
const IDM_ECO: usize = 204;
const IDM_BALANCED: usize = 205;

/// Tray icon callback (mouse message in the low word of lParam).
const WM_APP_TRAY: u32 = WM_APP + 1;
/// From the event thread: `APGeEvent` detail bytes 0 and 1 in wParam / lParam.
const WM_APP_HWEVENT: u32 = WM_APP + 2;
/// Sent by a second instance started without `--toggle`: show the window.
const WM_APP_SHOW: u32 = WM_APP + 3;
/// Sent by `--toggle` (PredatorSense button launcher) to a running instance.
const WM_APP_TOGGLE: u32 = WM_APP + 4;
const TIMER_SENSORS: usize = 1;
const SENSOR_PERIOD_MS: u32 = 2000;
/// The firmware gives no event for the Turbo button, so its flag is read this often (~9 ms/read).
const TIMER_TURBO_BUTTON: usize = 2;
const TURBO_BUTTON_PERIOD_MS: u32 = 1000;
/// Balanced/Eco fan curve tick (only while one of them is on, also when hidden in the tray).
const TIMER_CURVE: usize = 3;
const CURVE_PERIOD_MS: u32 = 2000;
/// A `--toggle` this soon after a raw-input press of the PredatorSense button is the launcher's
/// echo of that press (Explorer takes up to ~1 s to start it).
const TOGGLE_ECHO_MS: u64 = 2500;

// Raw control styles / messages (kept as plain integers to avoid newtype juggling; the
// trailing `_` keeps them apart from the windows crate's typed constants of the same name).
const BS_AUTOCHECKBOX_: u32 = 0x3;
const BS_GROUPBOX_: u32 = 0x7;
const BS_AUTORADIOBUTTON_: u32 = 0x9;
const BS_OWNERDRAW_: u32 = 0xB;
const BS_PUSHLIKE_: u32 = 0x1000;
const SS_CENTERIMAGE_: u32 = 0x200;
const SS_ENDELLIPSIS_: u32 = 0x4000;
const CBS_DROPDOWNLIST_: u32 = 0x3;
const TBS_AUTOTICKS_: u32 = 0x1;
const TBS_NOTICKS_: u32 = 0x10;
const TBM_GETPOS_: u32 = WM_USER;
const TBM_SETPOS_: u32 = WM_USER + 5;
const TBM_SETRANGE_: u32 = WM_USER + 6;
const TBM_SETTICFREQ_: u32 = WM_USER + 20;
const TBM_SETPAGESIZE_: u32 = WM_USER + 21;
const TB_THUMBTRACK_: u32 = 5;
const CB_ADDSTRING_: u32 = 0x143;
const CB_GETCURSEL_: u32 = 0x147;
const CB_SETCURSEL_: u32 = 0x14E;
const CBN_SELCHANGE_: u32 = 1;
const BM_GETCHECK_: u32 = 0xF0;
const BM_SETCHECK_: u32 = 0xF1;
const BN_CLICKED_: u32 = 0;
const PBT_APMPOWERSTATUSCHANGE_: usize = 0xA;
const APPCOMMAND_LAUNCH_APP1_: u32 = 17;
const PBT_APMRESUMEAUTOMATIC_: usize = 0x12;

/// Fan modes in radio-button order.
const FAN_MODES: [FanMode; 3] = [FanMode::Auto, FanMode::Max, FanMode::Custom];

/// Everything the window procedure works with.
struct App {
    ctl: Controller<Wmi>,
    hwnd: HWND,
    font: HFONT,
    icon: HICON,
    icon_small: HICON,
    /// System DPI / 96: layout coordinates are in 96-DPI pixels.
    scale: f32,
    /// A settings file exists (or the user changed something): reapply on resume, save on change.
    has_saved_settings: bool,
    /// `sync` is updating the controls: ignore the notifications that causes.
    syncing: bool,
    /// Message id Explorer broadcasts when it restarts (the tray icon must be re-added).
    taskbar_created: u32,
    /// The color dialog's custom colors, kept for the session.
    custom_colors: [COLORREF; 16],
    /// Text shown per sensor label, to skip redundant redraws.
    sensor_text: Vec<String>,
    /// PredatorSense button currently held (ignore auto-repeat).
    predator_key_held: bool,
    /// When the button was last handled from raw input; the `--toggle` request Explorer sends
    /// for the same press (remapped key) arrives later and must not toggle again.
    predator_key_pressed_at: Option<std::time::Instant>,
    /// Opt-in diagnostics: `%APPDATA%\PredatorLite\debug` exists -> events go to debug.log.
    debug: Option<(std::fs::File, std::time::Instant)>,
    /// GPU mode in use since boot, and the one the firmware will use from the next start (the
    /// running one unless it was switched since Windows started; `None` without a GPU mode switch).
    gpu_mode_running: GpuMode,
    gpu_mode_next: Option<GpuMode>,
}

/// UTF-16 string for Win32 calls.
fn wide(s: &str) -> HSTRING {
    HSTRING::from(s)
}

/// `SendMessageW` with plain integers.
unsafe fn send(h: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    SendMessageW(h, msg, Some(WPARAM(wp)), Some(LPARAM(lp))).0
}

/// The child control with this id.
unsafe fn item(parent: HWND, id: i32) -> HWND {
    GetDlgItem(Some(parent), id).unwrap_or_default()
}

/// Low 16 bits of a message parameter (Win32 `LOWORD`).
#[allow(clippy::cast_possible_truncation, reason = "keeping the low 16 bits is the point")]
const fn loword(v: usize) -> u16 {
    v as u16
}

/// Bits 16..32 of a message parameter (Win32 `HIWORD`).
#[allow(clippy::cast_possible_truncation, reason = "keeping 16 bits is the point")]
const fn hiword(v: usize) -> u16 {
    (v >> 16) as u16
}

/// Control id of item `index` in a row of controls whose ids start at `base` (sensor labels,
/// zone buttons).
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap, reason = "rows hold fewer than 16 controls")]
const fn row_id(base: i32, index: usize) -> i32 {
    base + index as i32
}

/// Position of control `id` in the row of `len` controls starting at `base`, if it is in it.
fn row_index(id: i32, base: i32, len: usize) -> Option<usize> {
    usize::try_from(id - base).ok().filter(|&i| i < len)
}

/// Id offset of fan group `g`'s row of controls: its ids are `ID_FAN_*` plus this.
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap, reason = "there are two fan groups")]
const fn fan_row(g: FanGroup) -> i32 {
    g.index() as i32 * FAN_ID_STRIDE
}

/// The fan group and mode of a fan mode radio button; `None` for any other control.
fn fan_radio(id: i32) -> Option<(FanGroup, FanMode)> {
    let offset = id - ID_FAN_RADIO;
    let group = FanGroup::ALL.get(usize::try_from(offset / FAN_ID_STRIDE).ok()?)?;
    let mode = FAN_MODES.get(usize::try_from(offset % FAN_ID_STRIDE).ok()?)?;
    Some((*group, *mode))
}

/// `v` layout pixels (96 DPI) in device pixels at display `scale`.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, reason = "layout sizes are far below 2^23")]
fn scale_px(v: i32, scale: f32) -> i32 {
    (v as f32 * scale).round() as i32
}

/// `debug.log` next to the settings, only when a file named `debug` exists there.
fn open_debug_log() -> Option<(std::fs::File, std::time::Instant)> {
    let dir = settings::path()?.parent()?.to_path_buf();
    if !dir.join("debug").exists() {
        return None;
    }
    let f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("debug.log")).ok()?;
    Some((f, std::time::Instant::now()))
}

/// Restart Windows in a few seconds (GPU mode changes need it).
fn restart_windows() -> std::io::Result<()> {
    hidden_command("shutdown.exe")
        .args(["/r", "/t", "5", "/c", "Predator Lite: switching the GPU mode"])
        .spawn()
        .map(drop)
}

impl App {
    /// 96-DPI layout pixels -> device pixels.
    fn px(&self, v: i32) -> i32 {
        scale_px(v, self.scale)
    }

    /// Create a child control at a 96-DPI position, with the dialog font.
    #[allow(clippy::too_many_arguments, reason = "mirrors CreateWindowExW: class, text, style, rectangle, id")]
    unsafe fn child(&self, class: PCWSTR, text: &str, style: u32, x: i32, y: i32, w: i32, h: i32, id: i32) -> HWND {
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            &wide(text),
            WINDOW_STYLE(style) | WS_CHILD | WS_VISIBLE,
            self.px(x),
            self.px(y),
            self.px(w),
            self.px(h),
            Some(self.hwnd),
            Some(HMENU(id as isize as *mut _)),
            None,
            None,
        )
        .unwrap_or_default();
        send(hwnd, WM_SETFONT, self.font.0 as usize, 0);
        hwnd
    }

    /// One-line static text (id -1 when it is never updated).
    unsafe fn label(&self, text: &str, x: i32, y: i32, w: i32, id: i32) -> HWND {
        self.child(w!("STATIC"), text, SS_CENTERIMAGE_ | SS_ENDELLIPSIS_, x, y, w, 24, id)
    }

    /// Group box frame around a section.
    unsafe fn group(&self, text: &str, x: i32, y: i32, w: i32, h: i32) {
        self.child(w!("BUTTON"), text, BS_GROUPBOX_, x, y, w, h, -1);
    }

    /// Horizontal slider over `min..=max`; page steps of 10 on wide ranges, else 1.
    #[allow(clippy::too_many_arguments, reason = "rectangle, id, range and tick style of one slider")]
    unsafe fn trackbar(&self, x: i32, y: i32, w: i32, id: i32, min: u8, max: u8, ticks: bool) -> HWND {
        let style = WS_TABSTOP.0 | if ticks { TBS_AUTOTICKS_ } else { TBS_NOTICKS_ };
        let h = self.child(w!("msctls_trackbar32"), "", style, x, y, w, 28, id);
        send(h, TBM_SETRANGE_, 1, (isize::from(max) << 16) | isize::from(min));
        send(h, TBM_SETTICFREQ_, 10, 0);
        send(h, TBM_SETPAGESIZE_, 0, if max > 20 { 10 } else { 1 });
        h
    }

    /// Drop-down list with `items` (the height includes the open list).
    unsafe fn combo(&self, x: i32, y: i32, w: i32, id: i32, items: &[&str]) -> HWND {
        let combo = self.child(w!("COMBOBOX"), "", CBS_DROPDOWNLIST_ | WS_TABSTOP.0 | WS_VSCROLL.0, x, y, w, 200, id);
        for item in items {
            let text = wide(item);
            send(combo, CB_ADDSTRING_, 0, text.as_ptr() as isize);
        }
        combo
    }

    /// Check box / radio button `id` is checked.
    unsafe fn is_checked(&self, id: i32) -> bool {
        send(item(self.hwnd, id), BM_GETCHECK_, 0, 0) == 1
    }

    unsafe fn set_checked(&self, id: i32, on: bool) {
        send(item(self.hwnd, id), BM_SETCHECK_, usize::from(on), 0);
    }

    /// Selected index of combo box `id` (negative when nothing is selected).
    unsafe fn combo_sel(&self, id: i32) -> isize {
        send(item(self.hwnd, id), CB_GETCURSEL_, 0, 0)
    }

    /// Select index `sel` of combo box `id` (`usize::MAX` clears the selection).
    unsafe fn set_combo_sel(&self, id: i32, sel: usize) {
        send(item(self.hwnd, id), CB_SETCURSEL_, sel, 0);
    }

    /// Show `msg` as a tray balloon when the window is hidden (no other feedback then).
    unsafe fn notify_if_hidden(&self, msg: &str) {
        if !IsWindowVisible(self.hwnd).as_bool() {
            self.tray(NIM_MODIFY, Some(msg));
        }
    }

    /// Create every control, one group box at a time. Controls for features this machine lacks are
    /// disabled or hidden.
    unsafe fn build(&mut self) {
        let caps = self.ctl.caps.clone();
        self.build_monitoring(&caps);
        self.build_fans(&caps);
        self.build_performance(&caps);
        self.build_keyboard(&caps);
        self.build_system(&caps);
        self.child(w!("STATIC"), "", SS_CENTERIMAGE_ | SS_ENDELLIPSIS_, 12, 544, 436, 22, ID_STATUS);
    }

    /// One label per sensor; sensors that don't answer are hidden.
    unsafe fn build_monitoring(&mut self, caps: &Capabilities) {
        self.group("Monitoring", 8, 6, 444, 82);
        for (i, s) in (0..).zip(Sensor::ALL.iter()) {
            let x = 20 + (i % 3) * 145;
            let y = 26 + (i / 3) * 26;
            let h = self.label(&format_sensor(*s, None, None), x, y, 140, ID_SENSOR + i);
            if !caps.sensors.contains(s) {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
    }

    /// Auto / Max / Custom and a speed slider per fan group, and CoolBoost.
    unsafe fn build_fans(&mut self, caps: &Capabilities) {
        let tab = WS_TABSTOP.0;
        self.group("Fans", 8, 94, 444, 118);
        for (row, g) in (0..).zip(FanGroup::ALL) {
            let base = fan_row(g);
            let y = 114 + row * 32;
            let present = caps.has_group(g);
            self.label(g.label(), 20, y, 40, -1);
            // One radio button per `FAN_MODES` entry: (text, x, width).
            let radios = [("Auto", 62, 56), ("Max", 120, 50), ("Custom", 172, 66)];
            for (i, (text, x, width)) in (0..).zip(radios) {
                let style = BS_AUTORADIOBUTTON_ | tab | if i == 0 { WS_GROUP.0 } else { 0 };
                let h = self.child(w!("BUTTON"), text, style, x, y, width, 24, ID_FAN_RADIO + base + i);
                let _ = EnableWindow(h, present);
            }
            let track = self.trackbar(240, y - 2, 168, ID_FAN_TRACK + base, 0, 100, true);
            let _ = EnableWindow(track, present);
            self.label("", 410, y, 38, ID_FAN_PCT + base);
        }

        let h = self.child(
            w!("BUTTON"),
            "CoolBoost (stronger Auto fan curve)",
            BS_AUTOCHECKBOX_ | tab,
            20,
            178,
            300,
            24,
            ID_COOLBOOST,
        );
        let _ = EnableWindow(h, caps.coolboost);
    }

    /// The three modes, GPU overclock, LCD overdrive and GPU mode.
    unsafe fn build_performance(&mut self, caps: &Capabilities) {
        let tab = WS_TABSTOP.0;
        self.group("Performance", 8, 218, 444, 92);
        let h = self.child(
            w!("BUTTON"),
            "Turbo",
            BS_AUTOCHECKBOX_ | BS_PUSHLIKE_ | tab | WS_GROUP.0,
            20,
            238,
            58,
            28,
            ID_TURBO,
        );
        let _ = EnableWindow(h, caps.turbo_led || caps.gpu_oc || caps.fan_slots != 0);
        let h =
            self.child(w!("BUTTON"), "Balanced", BS_AUTOCHECKBOX_ | BS_PUSHLIKE_ | tab, 82, 238, 68, 28, ID_BALANCED);
        let _ = EnableWindow(h, caps.fan_slots != 0);
        // Eco is always enabled: its CPU/GPU limits work on any machine, its fan part follows the
        // detected fans.
        self.child(w!("BUTTON"), "Eco", BS_AUTOCHECKBOX_ | BS_PUSHLIKE_ | tab, 154, 238, 50, 28, ID_ECO);
        self.label("GPU OC", 210, 240, 46, -1);
        let h = self.combo(258, 239, 84, ID_GPU_OC, &GpuOc::ALL.map(GpuOc::label));
        let _ = EnableWindow(h, caps.gpu_oc);
        let h = self.child(w!("BUTTON"), "LCD overdrive", BS_AUTOCHECKBOX_ | tab, 348, 240, 100, 24, ID_LCD);
        let _ = EnableWindow(h, caps.lcd_overdrive);
        self.label("GPU mode", 20, 276, 62, -1);
        let h = self.combo(84, 275, 150, ID_GPU_MODE, &[GpuMode::Hybrid.label(), GpuMode::Discrete.label()]);
        let _ = EnableWindow(h, caps.gpu_mode);
        self.label("", 244, 276, 204, ID_GPU_MODE_NOTE);
    }

    /// Lighting effect and its options, zone colors, Win key and backlight timeout.
    unsafe fn build_keyboard(&mut self, caps: &Capabilities) {
        let tab = WS_TABSTOP.0;
        self.group("Keyboard", 8, 316, 444, 164);
        self.label("Effect", 20, 336, 46, -1);
        self.combo(68, 336, 110, ID_KB_EFFECT, &KbEffect::ALL.map(KbEffect::label));
        self.label("Direction", 194, 336, 60, -1);
        self.combo(256, 336, 130, ID_KB_DIR, &["Right to left", "Left to right"]);
        self.label("Brightness", 20, 370, 66, -1);
        self.trackbar(88, 368, 140, ID_KB_BRIGHT, 0, KB_MAX_BRIGHTNESS / KB_BRIGHTNESS_STEP, true);
        self.label("Speed", 240, 370, 40, -1);
        self.trackbar(282, 368, 150, ID_KB_SPEED, 1, KB_MAX_SPEED, true);
        for (z, column) in (0..KB_ZONES).zip(0..) {
            let h = self.child(
                w!("BUTTON"),
                &format!("Zone {}", z + 1),
                BS_OWNERDRAW_ | tab,
                20 + column * 76,
                406,
                70,
                30,
                ID_ZONE + column,
            );
            if !caps.has_zone(z) {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
        self.child(w!("BUTTON"), "Effect color", BS_OWNERDRAW_ | tab, 324, 406, 116, 30, ID_KB_COLOR);
        if !caps.kb_effects {
            for id in [ID_KB_EFFECT, ID_KB_DIR, ID_KB_BRIGHT, ID_KB_SPEED, ID_KB_COLOR] {
                let _ = EnableWindow(item(self.hwnd, id), false);
            }
        }
        let h = self.child(w!("BUTTON"), "Win key", BS_AUTOCHECKBOX_ | tab, 20, 446, 70, 24, ID_WIN_KEY);
        let _ = EnableWindow(h, caps.win_key);
        self.label("Backlight off when idle", 110, 446, 140, -1);
        let h =
            self.combo(252, 446, 90, ID_KB_TIMEOUT, &KB_TIMEOUTS.map(kb_timeout_label).each_ref().map(String::as_str));
        let _ = EnableWindow(h, caps.kb_timeout);
    }

    /// Boot animation and USB charging when off.
    unsafe fn build_system(&mut self, caps: &Capabilities) {
        let tab = WS_TABSTOP.0;
        self.group("System", 8, 486, 444, 52);
        let h = self.child(w!("BUTTON"), "Boot animation and sound", BS_AUTOCHECKBOX_ | tab, 20, 506, 190, 24, ID_BOOT);
        let _ = EnableWindow(h, caps.boot_animation);
        self.label("USB charging when off", 218, 506, 130, -1);
        let h = self.combo(
            350,
            506,
            94,
            ID_USB_CHARGE,
            &USB_CHARGE_FLOORS.map(usb_charge_label).each_ref().map(String::as_str),
        );
        let _ = EnableWindow(h, caps.usb_charge);
    }

    /// Reflect controller state in the controls without triggering handlers.
    unsafe fn sync(&mut self) {
        self.syncing = true;
        let s = self.ctl.settings.clone();
        for g in FanGroup::ALL {
            let base = fan_row(g);
            let mode = self.ctl.effective_fan_mode(g);
            for (i, m) in (0..).zip(FAN_MODES) {
                self.set_checked(ID_FAN_RADIO + base + i, m == mode);
            }
            // The slider shows the user's own speed, also while a curve drives the fans.
            let pct = s.fan_pct[g.index()];
            send(item(self.hwnd, ID_FAN_TRACK + base), TBM_SETPOS_, 1, pct as isize);
            let _ = SetWindowTextW(item(self.hwnd, ID_FAN_PCT + base), &wide(&format!("{pct}%")));
        }
        self.set_checked(ID_TURBO, s.turbo);
        self.set_checked(ID_ECO, s.eco);
        self.set_checked(ID_BALANCED, s.balanced);
        // The fan curve runs only while Balanced or Eco is on.
        if self.ctl.active_curve().is_some() {
            SetTimer(Some(self.hwnd), TIMER_CURVE, CURVE_PERIOD_MS, None);
        } else {
            let _ = KillTimer(Some(self.hwnd), TIMER_CURVE);
        }
        self.set_combo_sel(ID_GPU_OC, self.ctl.effective_gpu_oc() as usize);
        self.set_checked(ID_LCD, s.lcd_overdrive);
        self.set_checked(ID_COOLBOOST, s.coolboost);
        self.set_checked(ID_WIN_KEY, s.win_key);
        self.set_checked(ID_BOOT, s.boot_animation);
        // A value the combo box does not offer shows as no selection.
        let timeout = KB_TIMEOUTS.iter().position(|&t| t == s.kb_timeout).unwrap_or(usize::MAX);
        self.set_combo_sel(ID_KB_TIMEOUT, timeout);
        let usb = USB_CHARGE_FLOORS.iter().position(|&f| f == s.usb_charge).unwrap_or(usize::MAX);
        self.set_combo_sel(ID_USB_CHARGE, usb);
        let gpu_mode = match self.gpu_mode_next {
            Some(GpuMode::Hybrid) => 0,
            Some(GpuMode::Discrete) => 1,
            None => usize::MAX,
        };
        self.set_combo_sel(ID_GPU_MODE, gpu_mode);
        let note = match self.gpu_mode_next {
            _ if !self.ctl.caps.gpu_mode => "Not available on this model".to_string(),
            Some(next) if next != self.gpu_mode_running => format!("Restart to switch to {}", next.label()),
            _ => format!("Running: {}", self.gpu_mode_running.label()),
        };
        let _ = SetWindowTextW(item(self.hwnd, ID_GPU_MODE_NOTE), &wide(&note));
        self.set_combo_sel(ID_KB_EFFECT, s.kb.effect as usize);
        // Directions are 1-based.
        self.set_combo_sel(ID_KB_DIR, s.kb.direction as usize - 1);
        // The brightness slider counts steps of KB_BRIGHTNESS_STEP (rounded to the nearest).
        let brightness_step = (s.kb.brightness + KB_BRIGHTNESS_STEP / 2) / KB_BRIGHTNESS_STEP;
        send(item(self.hwnd, ID_KB_BRIGHT), TBM_SETPOS_, 1, brightness_step as isize);
        send(item(self.hwnd, ID_KB_SPEED), TBM_SETPOS_, 1, s.kb.speed.max(1) as isize);
        if self.ctl.caps.kb_effects {
            let effect = s.kb.effect;
            let _ = EnableWindow(item(self.hwnd, ID_KB_DIR), effect.uses_direction());
            let _ = EnableWindow(item(self.hwnd, ID_KB_SPEED), effect != KbEffect::Static);
            let _ = EnableWindow(item(self.hwnd, ID_KB_COLOR), effect.uses_color());
            for z in 0..KB_ZONES {
                let _ = EnableWindow(item(self.hwnd, row_id(ID_ZONE, z)), effect == KbEffect::Static);
            }
        }
        for id in (ID_ZONE..row_id(ID_ZONE, KB_ZONES)).chain([ID_KB_COLOR]) {
            let _ = InvalidateRect(Some(item(self.hwnd, id)), None, true);
        }
        self.syncing = false;
    }

    unsafe fn set_status(&self, text: &str) {
        let _ = SetWindowTextW(item(self.hwnd, ID_STATUS), &wide(text));
    }

    /// Common tail of every user action: report, persist, refresh the controls.
    unsafe fn done(&mut self, r: Result<(), Error>, ok_msg: &str) {
        match r {
            Ok(()) => {
                self.has_saved_settings = true;
                let msg = match settings::save(&self.ctl.settings) {
                    Ok(()) => ok_msg.to_string(),
                    Err(e) => format!("{ok_msg} (settings not saved: {e})"),
                };
                self.set_status(&msg);
            }
            Err(e) => self.set_status(&e.to_string()),
        }
        self.sync();
    }

    /// Re-read the sensors and update the labels whose text changed.
    unsafe fn refresh_sensors(&mut self) {
        for (s, v) in self.ctl.read_sensors() {
            let Some(i) = Sensor::ALL.iter().position(|x| *x == s) else { continue };
            let text = format_sensor(s, v, self.ctl.read_fan_duty(s));
            if self.sensor_text[i] != text {
                let _ = SetWindowTextW(item(self.hwnd, row_id(ID_SENSOR, i)), &wide(&text));
                self.sensor_text[i] = text;
            }
        }
    }

    /// Show the window in front and start sensor polling.
    unsafe fn show(&mut self) {
        let _ = ShowWindow(self.hwnd, SW_SHOWNORMAL);
        self.bring_to_front();
        self.refresh_sensors();
        SetTimer(Some(self.hwnd), TIMER_SENSORS, SENSOR_PERIOD_MS, None);
    }

    /// Windows only lets the foreground thread hand out focus; borrow its input queue briefly.
    unsafe fn bring_to_front(&self) {
        let fg = GetForegroundWindow();
        let me = GetCurrentThreadId();
        let other = if fg.is_invalid() { 0 } else { GetWindowThreadProcessId(fg, None) };
        let attached = other != 0 && other != me && AttachThreadInput(me, other, true).as_bool();
        let _ = BringWindowToTop(self.hwnd);
        let _ = SetForegroundWindow(self.hwnd);
        if attached {
            let _ = AttachThreadInput(me, other, false);
        }
        if GetForegroundWindow() != self.hwnd {
            SwitchToThisWindow(self.hwnd, true);
        }
    }

    /// Start or stop polling the Turbo button, per the setting.
    unsafe fn watch_turbo_button(&self) {
        if self.ctl.caps.turbo_flag && self.ctl.settings.turbo_button {
            SetTimer(Some(self.hwnd), TIMER_TURBO_BUTTON, TURBO_BUTTON_PERIOD_MS, None);
        } else {
            let _ = KillTimer(Some(self.hwnd), TIMER_TURBO_BUTTON);
        }
    }

    unsafe fn on_turbo_button_timer(&mut self) {
        let r = self.ctl.poll_turbo_button();
        let msg = match &r {
            Ok(TurboButton::Unchanged) => return,
            Ok(TurboButton::On) => "Turbo on (button)".to_string(),
            Ok(TurboButton::Off) => "Turbo off (button)".to_string(),
            Ok(TurboButton::RefusedOnBattery) => Error::NeedsAc.to_string(),
            // Transient WMI trouble: show it in the status line, keep polling.
            Err(e) => {
                self.set_status(&format!("Turbo button: {e}"));
                return;
            }
        };
        self.notify_if_hidden(&msg);
        self.done(Ok(()), &msg);
    }

    /// Append a line to debug.log, if diagnostics are on.
    fn debug_log(&mut self, msg: &str) {
        use std::io::Write;
        if let Some((f, t0)) = &mut self.debug {
            let _ = writeln!(f, "{:9.3}s  {msg}", t0.elapsed().as_secs_f64());
        }
    }

    /// PredatorSense button: bring the window up, or hide it if it is already in front.
    unsafe fn toggle_window_from_button(&mut self) {
        let visible = IsWindowVisible(self.hwnd).as_bool();
        let iconic = IsIconic(self.hwnd).as_bool();
        let fg = GetForegroundWindow();
        let front = visible && !iconic && fg == self.hwnd;
        self.debug_log(&format!(
            "predator_key: visible={visible} iconic={iconic} foreground={:?} ours={:?} -> {}",
            fg.0,
            self.hwnd.0,
            if front { "hide" } else { "show" }
        ));
        if front {
            self.hide();
        } else {
            self.show();
        }
    }

    /// `WM_INPUT`: react to the PredatorSense button's key-down (once per press).
    unsafe fn on_raw_input(&mut self, lp: LPARAM) {
        let mut ri = RAWINPUT::default();
        let mut size = size_u32::<RAWINPUT>();
        let header_size = size_u32::<RAWINPUTHEADER>();
        let n = GetRawInputData(
            HRAWINPUT(lp.0 as *mut _),
            RID_INPUT,
            Some((&raw mut ri).cast()),
            &raw mut size,
            header_size,
        );
        if n == u32::MAX || ri.header.dwType != RIM_TYPEKEYBOARD.0 {
            return;
        }
        let k = ri.data.keyboard;
        // Log anything that looks like the button (original or remapped codes, see is_predator_key).
        let maybe_button = matches!(k.VKey, VK_PREDATOR_KEY | VK_LAUNCH_APP1)
            || matches!(k.MakeCode, SCAN_PREDATOR_KEY | SCAN_LAUNCH_APP1);
        if self.debug.is_some() && maybe_button {
            let line = format!(
                "raw input: make={:#04x} flags={:#x} vkey={:#04x} message={:#x} device={:?} held={}",
                k.MakeCode, k.Flags, k.VKey, k.Message, ri.header.hDevice.0, self.predator_key_held
            );
            self.debug_log(&line);
        }
        if !is_predator_key(k.MakeCode, u32::from(k.Flags) & RI_KEY_E0 != 0, k.VKey) {
            return;
        }
        let down = u32::from(k.Flags) & RI_KEY_BREAK == 0;
        if down && !self.predator_key_held {
            self.predator_key_pressed_at = Some(std::time::Instant::now());
            self.toggle_window_from_button();
        }
        self.predator_key_held = down;
    }

    /// Hide to the tray and stop sensor polling.
    unsafe fn hide(&mut self) {
        let _ = KillTimer(Some(self.hwnd), TIMER_SENSORS);
        let _ = ShowWindow(self.hwnd, SW_HIDE);
    }

    /// Add, update or remove the tray icon (`msg`), optionally with a balloon message.
    unsafe fn tray(&self, msg: NOTIFY_ICON_MESSAGE, balloon: Option<&str>) {
        let mut nid = NOTIFYICONDATAW {
            cbSize: size_u32::<NOTIFYICONDATAW>(),
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_APP_TRAY,
            hIcon: self.icon_small,
            ..Default::default()
        };
        let tip: Vec<u16> = TITLE.encode_utf16().collect();
        nid.szTip[..tip.len()].copy_from_slice(&tip);
        if let Some(text) = balloon {
            nid.uFlags |= NIF_INFO;
            // szInfo holds 256 units including the terminating zero.
            let info: Vec<u16> = text.encode_utf16().take(255).collect();
            nid.szInfo[..info.len()].copy_from_slice(&info);
            nid.szInfoTitle[..tip.len()].copy_from_slice(&tip);
            nid.dwInfoFlags = NIIF_NONE | NIIF_NOSOUND;
        }
        let _ = Shell_NotifyIconW(msg, &raw const nid);
    }

    /// Right-click menu of the tray icon; the choice arrives as `WM_COMMAND` with an IDM_* id.
    unsafe fn tray_menu(&mut self) {
        let Ok(menu) = CreatePopupMenu() else { return };
        let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
        let s = &self.ctl.settings;
        let _ = AppendMenuW(menu, MF_STRING, IDM_SHOW, w!("Show"));
        let _ = AppendMenuW(menu, MF_STRING | check(s.turbo), IDM_TURBO, w!("Turbo"));
        let _ = AppendMenuW(menu, MF_STRING | check(s.balanced), IDM_BALANCED, w!("Balanced"));
        let _ = AppendMenuW(menu, MF_STRING | check(s.eco), IDM_ECO, w!("Eco"));
        if self.ctl.caps.turbo_flag {
            let _ = AppendMenuW(menu, MF_STRING | check(s.turbo_button), IDM_TURBO_BUTTON, w!("Watch Turbo button"));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, IDM_EXIT, w!("Exit"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&raw mut pt);
        // Required so the menu closes when clicking elsewhere.
        let _ = SetForegroundWindow(self.hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, None, self.hwnd, None);
        let _ = DestroyMenu(menu);
    }

    /// Eco from the button or the tray menu.
    unsafe fn toggle_eco(&mut self) {
        let r = self.ctl.toggle_eco();
        let msg = match &r {
            Ok((on, note)) => {
                let base = if *on {
                    let gpu = match self.ctl.settings.eco_gpu_mhz {
                        0 => "GPU at Normal".to_string(),
                        mhz => format!("GPU capped at {mhz} MHz"),
                    };
                    format!(
                        "Eco on: CPU boost off, {gpu}, fans off below {} °C. {}",
                        self.ctl.settings.eco_curve.start(),
                        self.ctl.curve_status()
                    )
                } else {
                    "Eco off".to_string()
                };
                match note {
                    Some(n) => format!("{base} ({n})"),
                    None => base,
                }
            }
            Err(e) => e.to_string(),
        };
        self.notify_if_hidden(&msg);
        self.done(r.map(drop), &msg);
    }

    /// GPU mode (MUX) from the combo box: confirm, write, optionally restart.
    unsafe fn change_gpu_mode(&mut self) {
        let m = if self.combo_sel(ID_GPU_MODE) == 0 { GpuMode::Hybrid } else { GpuMode::Discrete };
        if Some(m) == self.gpu_mode_next {
            return;
        }
        let detail = match m {
            GpuMode::Hybrid => {
                "The Intel GPU drives the screen and the NVIDIA GPU works on demand: \
                 longer battery life, slightly lower game performance."
            }
            GpuMode::Discrete => {
                "The NVIDIA GPU drives the screen directly: best game performance, shorter battery life."
            }
        };
        let text = format!(
            "Switch the GPU mode to {}?\n\n{detail}\n\nThe change takes effect after a restart.\n\n\
             Yes: switch and restart now (save your work first)\n\
             No: switch, restart later\n\
             Cancel: keep the current mode",
            m.label()
        );
        let answer =
            MessageBoxW(Some(self.hwnd), &wide(&text), &wide(TITLE), MB_YESNOCANCEL | MB_ICONQUESTION | MB_DEFBUTTON2);
        if answer != IDYES && answer != IDNO {
            self.sync();
            return;
        }
        let r = self.ctl.set_gpu_mode(m);
        if r.is_ok() {
            self.gpu_mode_next = Some(m);
            // So the box still shows the switch if the app is restarted before Windows is.
            if let Err(e) = remember_pending_gpu_mode(m as u8) {
                self.debug_log(&format!("could not remember the GPU mode switch: {e}"));
            }
        }
        let msg = match (&r, answer == IDYES) {
            (Ok(()), true) => match restart_windows() {
                Ok(()) => format!("GPU mode {}: restarting in 5 seconds", m.label()),
                Err(e) => format!("GPU mode {} set; restart failed ({e}), restart manually", m.label()),
            },
            (Ok(()), false) => format!("GPU mode {} after the next restart", m.label()),
            (Err(_), _) => String::new(),
        };
        self.done(r, &msg);
    }

    /// Balanced from the button or the tray menu.
    unsafe fn toggle_balanced(&mut self) {
        let r = self.ctl.toggle_balanced();
        let msg = match &r {
            Ok(true) => format!(
                "Balanced on: full performance, fans off below {} °C. {}",
                self.ctl.settings.balanced_curve.start(),
                self.ctl.curve_status()
            ),
            Ok(false) => "Balanced off".to_string(),
            Err(e) => e.to_string(),
        };
        self.notify_if_hidden(&msg);
        self.done(r.map(drop), &msg);
    }

    /// `TIMER_CURVE`: one fan-curve step; the status line follows the curve.
    unsafe fn on_curve_timer(&mut self) {
        match self.ctl.curve_tick() {
            Ok(Some(note)) => {
                self.set_status(&note);
                self.sync();
            }
            Ok(None) => {}
            Err(e) => self.set_status(&format!("Fan curve: {e}")),
        }
    }

    /// Turbo from the button, the tray menu or the Turbo hotkey event (`from_key`: show a
    /// balloon when hidden).
    unsafe fn toggle_turbo(&mut self, from_key: bool) {
        let r = self.ctl.toggle_turbo();
        let msg = match &r {
            Ok(true) => "Turbo on".to_string(),
            Ok(false) => "Turbo off".to_string(),
            Err(e) => e.to_string(),
        };
        if from_key {
            self.notify_if_hidden(&msg);
        }
        self.done(r.map(drop), &msg);
    }

    /// The color dialog, starting at `initial`; `None` when cancelled.
    unsafe fn pick_color(&mut self, initial: Rgb) -> Option<Rgb> {
        let mut cc = CHOOSECOLORW {
            lStructSize: size_u32::<CHOOSECOLORW>(),
            hwndOwner: self.hwnd,
            rgbResult: COLORREF(initial.colorref()),
            lpCustColors: self.custom_colors.as_mut_ptr(),
            Flags: CC_RGBINIT | CC_FULLOPEN,
            ..Default::default()
        };
        ChooseColorW(&raw mut cc).as_bool().then(|| Rgb::from_colorref(cc.rgbResult.0))
    }

    /// `WM_COMMAND` from a control: dispatch on the control id and notification code.
    unsafe fn on_command(&mut self, id: i32, code: u32) {
        if self.syncing {
            return;
        }
        if let Some((g, m)) = fan_radio(id).filter(|_| code == BN_CLICKED_) {
            if self.ctl.effective_fan_mode(g) != m {
                let r = self.ctl.set_fan_mode(g, m);
                self.done(r, &format!("{} fans: {}", g.label(), m.as_str()));
            }
            return;
        }
        if let Some(z) = row_index(id, ID_ZONE, KB_ZONES).filter(|_| code == BN_CLICKED_) {
            if let Some(c) = self.pick_color(self.ctl.settings.zones[z]) {
                let r = self.ctl.set_zone_color(z, c);
                self.done(r, &format!("Zone {}: {}", z + 1, c.to_hex()));
            }
            return;
        }
        match id {
            ID_TURBO if code == BN_CLICKED_ => self.toggle_turbo(false),
            ID_ECO if code == BN_CLICKED_ => self.toggle_eco(),
            ID_BALANCED if code == BN_CLICKED_ => self.toggle_balanced(),
            ID_GPU_OC if code == CBN_SELCHANGE_ => {
                if let Some(level) = u8::try_from(self.combo_sel(ID_GPU_OC)).ok().and_then(GpuOc::from_u8) {
                    let r = self.ctl.set_gpu_oc(level);
                    self.done(r, &format!("GPU overclock: {}", level.label()));
                }
            }
            ID_GPU_MODE if code == CBN_SELCHANGE_ => self.change_gpu_mode(),
            ID_COOLBOOST if code == BN_CLICKED_ => {
                let on = self.is_checked(ID_COOLBOOST);
                let r = self.ctl.set_coolboost(on);
                self.done(r, if on { "CoolBoost on: stronger Auto fan curve" } else { "CoolBoost off" });
            }
            ID_LCD if code == BN_CLICKED_ => {
                let on = self.is_checked(ID_LCD);
                let r = self.ctl.set_lcd_overdrive(on);
                self.done(r, if on { "LCD overdrive on" } else { "LCD overdrive off" });
            }
            ID_WIN_KEY if code == BN_CLICKED_ => {
                let on = self.is_checked(ID_WIN_KEY);
                let r = self.ctl.set_win_key(on);
                self.done(r, if on { "Win key on" } else { "Win key locked (Windows and Menu keys off)" });
            }
            ID_BOOT if code == BN_CLICKED_ => {
                let on = self.is_checked(ID_BOOT);
                let r = self.ctl.set_boot_animation(on);
                self.done(r, if on { "Boot animation and sound on" } else { "Boot animation and sound off" });
            }
            ID_USB_CHARGE if code == CBN_SELCHANGE_ => {
                let sel = self.combo_sel(ID_USB_CHARGE);
                if let Some(&floor) = usize::try_from(sel).ok().and_then(|i| USB_CHARGE_FLOORS.get(i)) {
                    let r = self.ctl.set_usb_charge(floor);
                    let msg = if floor == 0 {
                        "USB ports off while the laptop is off".to_string()
                    } else {
                        format!("USB ports powered while off, until the battery is at {floor}%")
                    };
                    self.done(r, &msg);
                }
            }
            ID_KB_TIMEOUT if code == CBN_SELCHANGE_ => {
                let sel = self.combo_sel(ID_KB_TIMEOUT);
                if let Some(&secs) = usize::try_from(sel).ok().and_then(|i| KB_TIMEOUTS.get(i)) {
                    let r = self.ctl.set_kb_timeout(secs);
                    let msg = if secs == 0 {
                        "Keyboard backlight stays on".to_string()
                    } else {
                        format!("Keyboard backlight off after {} idle", kb_timeout_label(secs))
                    };
                    self.done(r, &msg);
                }
            }
            ID_KB_EFFECT | ID_KB_DIR if code == CBN_SELCHANGE_ => {
                let mut kb = self.ctl.settings.kb;
                let effect = self.combo_sel(ID_KB_EFFECT);
                let direction = self.combo_sel(ID_KB_DIR);
                kb.effect = u8::try_from(effect).ok().and_then(KbEffect::from_u8).unwrap_or(kb.effect);
                kb.direction = if direction == 1 { Direction::LeftToRight } else { Direction::RightToLeft };
                let r = self.ctl.set_kb(kb);
                self.done(r, &format!("Keyboard: {}", kb.effect.label()));
            }
            ID_KB_COLOR if code == BN_CLICKED_ => {
                if let Some(c) = self.pick_color(self.ctl.settings.kb.color) {
                    let kb = KbBacklight { color: c, ..self.ctl.settings.kb };
                    let r = self.ctl.set_kb(kb);
                    self.done(r, &format!("Effect color: {}", c.to_hex()));
                }
            }
            _ => {}
        }
    }

    /// `WM_HSCROLL` from a slider: fan speed, keyboard brightness or effect speed.
    unsafe fn on_hscroll(&mut self, ctrl: HWND, code: u32) {
        if self.syncing {
            return;
        }
        let id = GetDlgCtrlID(ctrl);
        // Every slider's range is within 0..=100.
        let pos = u8::try_from(send(ctrl, TBM_GETPOS_, 0, 0)).unwrap_or(0);
        // The percentage label follows the slider live, also while dragging.
        if id == ID_FAN_TRACK || id == ID_FAN_TRACK_GPU {
            let base = id - ID_FAN_TRACK;
            let _ = SetWindowTextW(item(self.hwnd, ID_FAN_PCT + base), &wide(&format!("{pos}%")));
        }
        // Apply when the drag ends (or on keyboard/wheel steps), not on every pixel.
        if code == TB_THUMBTRACK_ {
            return;
        }
        match id {
            ID_FAN_TRACK | ID_FAN_TRACK_GPU => {
                let g = if id == ID_FAN_TRACK { FanGroup::Cpu } else { FanGroup::Gpu };
                if self.ctl.settings.fan_pct[g.index()] != pos {
                    let r = self.ctl.set_fan_pct(g, pos);
                    self.done(r, &format!("Fan speed: {pos}%"));
                }
            }
            ID_KB_BRIGHT | ID_KB_SPEED => {
                let mut kb = self.ctl.settings.kb;
                let (what, value) = if id == ID_KB_BRIGHT {
                    // The slider counts brightness steps.
                    kb.brightness = (pos.min(KB_MAX_BRIGHTNESS / KB_BRIGHTNESS_STEP)) * KB_BRIGHTNESS_STEP;
                    ("brightness", format!("{}%", kb.brightness))
                } else {
                    kb.speed = pos.clamp(1, KB_MAX_SPEED);
                    ("speed", kb.speed.to_string())
                };
                if kb != self.ctl.settings.kb {
                    let r = self.ctl.set_kb(kb);
                    self.done(r, &format!("Keyboard {what}: {value}"));
                }
            }
            _ => {}
        }
    }

    /// `WM_DRAWITEM`: paint a zone / effect color button as a swatch of its color, with the
    /// label in black or white, whichever reads better.
    unsafe fn draw_swatch(&self, dis: &DRAWITEMSTRUCT) {
        let zone = i32::try_from(dis.CtlID).ok().and_then(|id| row_index(id, ID_ZONE, KB_ZONES));
        let color = match zone {
            Some(z) => self.ctl.settings.zones[z],
            None => self.ctl.settings.kb.color,
        };
        let disabled = dis.itemState.0 & ODS_DISABLED.0 != 0;
        let mut rc = dis.rcItem;
        let edge = if dis.itemState.0 & ODS_SELECTED.0 != 0 { EDGE_SUNKEN } else { EDGE_RAISED };
        let _ = DrawEdge(dis.hDC, &raw mut rc, edge, BF_RECT | BF_ADJUST);
        let fill = if disabled { GetSysColor(COLOR_BTNFACE) } else { color.colorref() };
        let brush = CreateSolidBrush(COLORREF(fill));
        FillRect(dis.hDC, &raw const rc, brush);
        let _ = DeleteObject(brush.into());
        // Perceived brightness (Rec. 601 weights, x1000); above ~55 % black text reads better.
        let luma = u32::from(color.r) * 299 + u32::from(color.g) * 587 + u32::from(color.b) * 114;
        let text_color = if disabled {
            GetSysColor(COLOR_GRAYTEXT)
        } else if luma > 140_000 {
            0
        } else {
            WHITE
        };
        SetTextColor(dis.hDC, COLORREF(text_color));
        SetBkMode(dis.hDC, TRANSPARENT);
        let mut text = [0u16; 32];
        let len = usize::try_from(GetWindowTextW(dis.hwndItem, &mut text)).unwrap_or(0);
        let mut text_rc = rc;
        DrawTextW(dis.hDC, &mut text[..len], &raw mut text_rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        if dis.itemState.0 & ODS_FOCUS.0 != 0 {
            let _ = InflateRect(&raw mut rc, -2, -2);
            let _ = DrawFocusRect(dis.hDC, &raw const rc);
        }
    }

    /// `WM_POWERBROADCAST`: AC/battery change (`resumed` = false) or resume from sleep.
    unsafe fn on_power(&mut self, resumed: bool) {
        let (ac, pct) = power_status();
        match self.ctl.on_power(ac, pct) {
            Ok(Some(note)) => {
                self.set_status(note);
                self.notify_if_hidden(note);
            }
            Ok(None) => {}
            Err(e) => self.set_status(&e.to_string()),
        }
        // The firmware may reset fans/lighting across sleep; put the user's settings back.
        // Settings the firmware owns are read instead.
        if resumed && self.has_saved_settings {
            if self.ctl.sync_firmware_owned() {
                let _ = settings::save(&self.ctl.settings);
            }
            if let Err(e) = self.ctl.restore() {
                self.set_status(&format!("Restore after sleep failed: {e}"));
            }
            if self.ctl.settings.eco {
                if let Err(e) = self.ctl.sync_limits() {
                    self.set_status(&format!("Eco after sleep: {e}"));
                }
            }
        }
        self.sync();
    }

    /// `WM_APP_HWEVENT`: an `APGeEvent` from the event thread.
    unsafe fn on_hw_event(&mut self, detail0: u8, detail1: u8) {
        match decode_event(&[detail0, detail1]) {
            Some(HwEvent::TurboKey) => self.toggle_turbo(true),
            Some(HwEvent::Other(a, b)) => self.set_status(&format!("Hotkey event {a}/{b} (not handled)")),
            Some(HwEvent::KbBacklightChanged) | None => {}
        }
    }
}

/// White as a COLORREF (0x00BBGGRR).
const WHITE: u32 = 0x00FF_FFFF;
/// Icon background, as COLORREF (0x00BBGGRR): near-black.
const ICON_BACKGROUND: u32 = 0x0020_1C1A;

/// Draw the app icon (a cyan "P" on near-black) at `size` x `size` pixels; no icon resource
/// is needed.
unsafe fn make_icon(size: i32) -> HICON {
    let screen = GetDC(None);
    let dc = CreateCompatibleDC(Some(screen));
    let color = CreateCompatibleBitmap(screen, size, size);
    // All-zero AND mask (rows padded to 16 bits): every pixel opaque.
    let mask_bits = vec![0u8; usize::try_from(((size + 15) / 16) * 2 * size).unwrap_or(0)];
    let mask = CreateBitmap(size, size, 1, 1, Some(mask_bits.as_ptr().cast()));
    let old = SelectObject(dc, color.into());
    let rc = RECT { left: 0, top: 0, right: size, bottom: size };
    let bg = CreateSolidBrush(COLORREF(ICON_BACKGROUND));
    FillRect(dc, &raw const rc, bg);
    let _ = DeleteObject(bg.into());
    let font = CreateFontW(
        -(size * 9 / 10),
        0,
        0,
        0,
        FW_HEAVY.0.cast_signed(),
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        ANTIALIASED_QUALITY,
        0,
        w!("Segoe UI"),
    );
    let old_font = SelectObject(dc, font.into());
    SetBkMode(dc, TRANSPARENT);
    SetTextColor(dc, COLORREF(Rgb::ACER_CYAN.colorref()));
    let mut text: Vec<u16> = "P".encode_utf16().collect();
    let mut text_rc = rc;
    DrawTextW(dc, &mut text, &raw mut text_rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    SelectObject(dc, old_font);
    let _ = DeleteObject(font.into());
    SelectObject(dc, old);
    let info = ICONINFO { fIcon: TRUE, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
    let icon = CreateIconIndirect(&raw const info).unwrap_or_default();
    let _ = DeleteObject(color.into());
    let _ = DeleteObject(mask.into());
    let _ = DeleteDC(dc);
    ReleaseDC(None, screen);
    icon
}

impl App {
    /// A tray menu item was chosen.
    unsafe fn on_menu_command(&mut self, id: usize) {
        match id {
            IDM_SHOW => self.show(),
            IDM_TURBO => self.toggle_turbo(false),
            IDM_ECO => self.toggle_eco(),
            IDM_BALANCED => self.toggle_balanced(),
            IDM_TURBO_BUTTON => {
                self.ctl.settings.turbo_button = !self.ctl.settings.turbo_button;
                self.watch_turbo_button();
                let on = self.ctl.settings.turbo_button;
                self.done(Ok(()), if on { "Watching the Turbo button" } else { "Turbo button ignored" });
            }
            IDM_EXIT => {
                let _ = DestroyWindow(self.hwnd);
            }
            _ => {}
        }
    }

    /// `WM_SIZE`: stop polling the sensors while minimized, resume when restored.
    unsafe fn on_size(&mut self, kind: usize) {
        if kind == SIZE_MINIMIZED as usize {
            let _ = KillTimer(Some(self.hwnd), TIMER_SENSORS);
        } else if kind == SIZE_RESTORED as usize && IsWindowVisible(self.hwnd).as_bool() {
            self.refresh_sensors();
            SetTimer(Some(self.hwnd), TIMER_SENSORS, SENSOR_PERIOD_MS, None);
        }
    }

    /// `--toggle` from the PredatorSense button's launcher. Ignored when raw input already
    /// handled the same press (Explorer takes up to ~1 s to start the launcher).
    unsafe fn on_toggle_request(&mut self) {
        let echo_window = std::time::Duration::from_millis(TOGGLE_ECHO_MS);
        let dup = self.predator_key_pressed_at.is_some_and(|t| t.elapsed() < echo_window);
        let since = self.predator_key_pressed_at.map(|t| t.elapsed().as_millis());
        self.debug_log(&format!("WM_APP_TOGGLE (launcher): ms since last raw press={since:?} duplicate={dup}"));
        if !dup {
            self.toggle_window_from_button();
        }
    }

    /// The window is closing for good (Exit): remove the tray icon and timers, and leave no fan
    /// curve unguarded (fans back to Auto, Eco's limits removed).
    unsafe fn on_destroy(&mut self) {
        self.tray(NIM_DELETE, None);
        for timer in [TIMER_SENSORS, TIMER_TURBO_BUTTON, TIMER_CURVE] {
            let _ = KillTimer(Some(self.hwnd), timer);
        }
        self.ctl.release_modes_on_exit();
    }
}

/// The `App` stored in the window, once `run` has set it (messages before that, and after
/// shutdown, get default handling).
unsafe fn app<'a>(hwnd: HWND) -> Option<&'a mut App> {
    (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App).as_mut()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(a) = app(hwnd) else { return DefWindowProcW(hwnd, msg, wp, lp) };
    match msg {
        WM_COMMAND => {
            let id = loword(wp.0);
            // lParam 0: from a menu (the tray menu), not a control.
            if lp.0 == 0 {
                a.on_menu_command(usize::from(id));
            } else {
                a.on_command(i32::from(id), u32::from(hiword(wp.0)));
            }
            LRESULT(0)
        }
        WM_HSCROLL => {
            a.on_hscroll(HWND(lp.0 as *mut _), u32::from(loword(wp.0)));
            LRESULT(0)
        }
        WM_DRAWITEM => {
            a.draw_swatch(&*(lp.0 as *const DRAWITEMSTRUCT));
            LRESULT(1)
        }
        WM_TIMER if wp.0 == TIMER_SENSORS => {
            a.refresh_sensors();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_CURVE => {
            a.on_curve_timer();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_TURBO_BUTTON => {
            a.on_turbo_button_timer();
            LRESULT(0)
        }
        WM_SIZE => {
            a.on_size(wp.0);
            LRESULT(0)
        }
        WM_CLOSE => {
            a.hide();
            LRESULT(0)
        }
        WM_APP_TRAY => {
            match u32::from(loword(lp.0.cast_unsigned())) {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => a.show(),
                WM_RBUTTONUP | WM_CONTEXTMENU => a.tray_menu(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_APP_SHOW => {
            a.debug_log("WM_APP_SHOW (second instance)");
            a.show();
            LRESULT(0)
        }
        WM_APP_TOGGLE => {
            a.on_toggle_request();
            LRESULT(0)
        }
        // The remapped button reaches us through raw input (any window in front, elevated ones
        // too). When our window is in front it also arrives as an app command: swallow it so it
        // neither toggles twice nor makes Explorer start another instance.
        WM_APPCOMMAND if u32::from(hiword(lp.0.cast_unsigned()) & 0x0FFF) == APPCOMMAND_LAUNCH_APP1_ => {
            a.debug_log("WM_APPCOMMAND launch-app1 (swallowed)");
            LRESULT(1)
        }
        WM_INPUT => {
            a.on_raw_input(lp);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_APP_HWEVENT => {
            // `spawn_event_thread` posts the two detail bytes.
            a.on_hw_event(u8::try_from(wp.0).unwrap_or(0), u8::try_from(lp.0).unwrap_or(0));
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            match wp.0 {
                PBT_APMPOWERSTATUSCHANGE_ => a.on_power(false),
                PBT_APMRESUMEAUTOMATIC_ => a.on_power(true),
                _ => {}
            }
            LRESULT(1)
        }
        WM_CTLCOLORSTATIC => {
            // Keep labels on the window background color.
            SetBkMode(HDC(wp.0 as *mut _), TRANSPARENT);
            LRESULT(GetSysColorBrush(COLOR_BTNFACE).0 as isize)
        }
        WM_DESTROY => {
            a.on_destroy();
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ if msg == a.taskbar_created && msg != 0 => {
            // Explorer restarted: the tray icon is gone, add it again.
            a.tray(NIM_ADD, None);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Background thread that blocks on `APGeEvent` and posts each event to the window as
/// `WM_APP_HWEVENT`. Ends when the window is gone.
fn spawn_event_thread(hwnd: HWND) {
    // HWND is not Send; pass the raw value.
    let raw = hwnd.0 as isize;
    let _ = std::thread::Builder::new().stack_size(64 * 1024).spawn(move || unsafe {
        let hwnd = HWND(raw as *mut _);
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        loop {
            if let Ok(events) = EventStream::open() {
                while let Ok(detail) = events.next() {
                    if detail.len() >= 2 {
                        let (wp, lp) = (WPARAM(usize::from(detail[0])), LPARAM(isize::from(detail[1])));
                        let _ = PostMessageW(Some(hwnd), WM_APP_HWEVENT, wp, lp);
                    }
                }
            }
            if !IsWindow(Some(hwnd)).as_bool() {
                return;
            }
            // WMI hiccup: back off, then resubscribe.
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    });
}

unsafe fn error_box(text: &str) {
    MessageBoxW(None, &wide(text), &wide(TITLE), MB_ICONERROR | MB_OK);
}

/// Another instance is already running: ask it to show (or, for `--toggle`, show or hide) its
/// window.
unsafe fn signal_running_instance(mode: StartMode) {
    if let Ok(h) = FindWindowW(CLASS, None) {
        // Let the running instance take focus from whoever has it now.
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        let msg = if mode == StartMode::Toggle { WM_APP_TOGGLE } else { WM_APP_SHOW };
        let _ = PostMessageW(Some(h), msg, WPARAM(0), LPARAM(0));
    }
}

/// Build the controller and put the saved settings back on the hardware. Returns it, whether a
/// settings file existed, and the start-up status line.
fn start_controller(wmi: Wmi) -> (Controller<Wmi>, bool, String) {
    let caps = Capabilities::detect(&wmi);
    let saved = settings::load();
    let has_saved_settings = saved.is_some();
    let mut ctl = Controller::new(wmi, caps, settings::Settings::default());
    ctl.limiter = Box::new(WindowsEcoLimiter);
    let (ac, pct) = power_status();
    ctl.power.update(ac, pct);
    ctl.settings = match saved {
        Some(mut s) => {
            // Saved Turbo does not come back on battery.
            if !ctl.power.allowed() {
                s.turbo = false;
            }
            s
        }
        // No settings yet: show what the firmware has, change nothing.
        None => ctl.read_state(),
    };
    ctl.prime_mode_curve();
    // Boot animation and LCD overdrive may have been changed in BIOS setup, USB charging in
    // Acer Care Center: show what the firmware holds (and remember it) instead of
    // overwriting it with the saved value.
    if has_saved_settings && ctl.sync_firmware_owned() {
        let _ = settings::save(&ctl.settings);
    }
    let restore_err = if has_saved_settings { ctl.restore().err() } else { None };
    // Re-apply Eco's OS limits, or clean up after a crash that left them on.
    let limits_note = match ctl.sync_limits() {
        Ok(n) => n,
        Err(e) => Some(e.to_string()),
    };

    let status = match restore_err {
        Some(e) => format!("Could not restore settings: {e}"),
        None if has_saved_settings => "Settings restored".into(),
        None => "Ready".into(),
    };
    let status = match limits_note {
        Some(n) => format!("{status} ({n})"),
        None => status,
    };
    (ctl, has_saved_settings, status)
}

/// How the app was started (see main.rs for the command line).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StartMode {
    /// Open the window.
    Window,
    /// `--hidden`: start in the tray only.
    Hidden,
    /// `--toggle`: started by the PredatorSense button (via the remapped key). A running instance
    /// shows or hides its window; otherwise this instance starts with the window open.
    Toggle,
}

/// Register the window class and create the (still hidden) main window, sized for the layout
/// at the system DPI. Returns the window and the DPI scale.
unsafe fn create_main_window(icon: HICON, icon_small: HICON) -> Option<(HWND, f32)> {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let wc = WNDCLASSEXW {
        cbSize: size_u32::<WNDCLASSEXW>(),
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        hIcon: icon,
        hIconSm: icon_small,
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
        lpszClassName: CLASS,
        ..Default::default()
    };
    RegisterClassExW(&raw const wc);

    let dpi = u16::try_from(GetDpiForSystem()).unwrap_or(96).max(96);
    let scale = f32::from(dpi) / 96.0;
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
    // Client area 460 x 572 at 96 DPI (the layout in `build`).
    let mut rc = RECT { left: 0, top: 0, right: scale_px(460, scale), bottom: scale_px(572, scale) };
    let _ = AdjustWindowRectEx(&raw mut rc, style, false, WINDOW_EX_STYLE(0));
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        CLASS,
        &wide(TITLE),
        style,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        rc.right - rc.left,
        rc.bottom - rc.top,
        None,
        None,
        Some(hinst.into()),
        None,
    )
    .ok()?;
    Some((hwnd, scale))
}

/// The system's message-box font, used for all controls.
unsafe fn message_font() -> HFONT {
    let mut ncm = NONCLIENTMETRICSW { cbSize: size_u32::<NONCLIENTMETRICSW>(), ..Default::default() };
    let ncm_ptr = Some((&raw mut ncm).cast());
    let _ = SystemParametersInfoW(SPI_GETNONCLIENTMETRICS, ncm.cbSize, ncm_ptr, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    CreateFontIndirectW(&raw const ncm.lfMessageFont)
}

/// Dispatch messages until the window is destroyed (`PostQuitMessage`). `IsDialogMessageW`
/// gives the controls Tab-key navigation.
unsafe fn message_loop(hwnd: HWND) {
    let mut msg = MSG::default();
    while GetMessageW(&raw mut msg, None, 0, 0).as_bool() {
        if !IsDialogMessageW(hwnd, &raw const msg).as_bool() {
            let _ = TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }
}

/// Run the app until Exit; returns the process exit code. Only one instance runs: a second
/// one just signals the first and exits.
#[must_use]
pub fn run(mode: StartMode) -> i32 {
    unsafe {
        let _mutex = CreateMutexW(None, true, w!("Local\\PredatorLite.Instance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            signal_running_instance(mode);
            return 0;
        }

        let wmi = match Wmi::connect() {
            Ok(w) => w,
            Err(e) => {
                error_box(&format!("Cannot reach the Acer gaming interface.\n\n{e}"));
                return 1;
            }
        };
        let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: size_u32::<INITCOMMONCONTROLSEX>(),
            dwICC: ICC_BAR_CLASSES | ICC_STANDARD_CLASSES,
        });
        let (ctl, has_saved_settings, startup_status) = start_controller(wmi);
        let icon = make_icon(32);
        let icon_small = make_icon(16);
        let Some((hwnd, scale)) = create_main_window(icon, icon_small) else {
            error_box("Could not create the main window.");
            return 1;
        };
        let font = message_font();

        let app = Box::into_raw(Box::new(App {
            ctl,
            hwnd,
            font,
            icon,
            icon_small,
            scale,
            has_saved_settings,
            syncing: false,
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            custom_colors: [COLORREF(WHITE); 16],
            sensor_text: vec![String::new(); Sensor::ALL.len()],
            predator_key_held: false,
            predator_key_pressed_at: None,
            debug: open_debug_log(),
            gpu_mode_running: running_gpu_mode(&display_adapter_ids()),
            gpu_mode_next: None,
        }));
        let a = &mut *app;
        // Not the firmware's value: it reads Discrete after every start on the PT515-51.
        if a.ctl.caps.gpu_mode {
            a.gpu_mode_next = Some(next_gpu_mode(a.gpu_mode_running, pending_gpu_mode()));
        }
        a.build();
        a.sync();
        a.set_status(&startup_status);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, app as isize);
        a.tray(NIM_ADD, None);
        spawn_event_thread(hwnd);
        a.watch_turbo_button();
        // Keyboard raw input (also when in the background) for the PredatorSense button.
        // HID usage page 1 (generic desktop), usage 6 (keyboard).
        let kbd = RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x06, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd };
        if RegisterRawInputDevices(&[kbd], size_u32::<RAWINPUTDEVICE>()).is_err() {
            a.set_status("PredatorSense button unavailable (raw input registration failed)");
        }
        if mode == StartMode::Hidden {
            a.hide();
        } else {
            a.show();
        }

        message_loop(hwnd);

        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        let a = Box::from_raw(app);
        let _ = DeleteObject(a.font.into());
        let _ = DestroyIcon(a.icon);
        let _ = DestroyIcon(a.icon_small);
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Every control id `build` creates.
    fn control_ids() -> Vec<i32> {
        let mut ids = Vec::new();
        for g in FanGroup::ALL {
            let base = fan_row(g);
            ids.extend((0..FAN_MODES.len()).map(|i| row_id(ID_FAN_RADIO + base, i)));
            ids.extend([ID_FAN_TRACK + base, ID_FAN_PCT + base]);
        }
        ids.extend([
            ID_TURBO,
            ID_GPU_OC,
            ID_LCD,
            ID_ECO,
            ID_GPU_MODE,
            ID_GPU_MODE_NOTE,
            ID_COOLBOOST,
            ID_BALANCED,
            ID_WIN_KEY,
            ID_BOOT,
            ID_KB_EFFECT,
            ID_KB_DIR,
            ID_KB_BRIGHT,
            ID_KB_SPEED,
            ID_KB_TIMEOUT,
            ID_USB_CHARGE,
            ID_KB_COLOR,
            ID_STATUS,
        ]);
        ids.extend((0..KB_ZONES).map(|z| row_id(ID_ZONE, z)));
        ids.extend((0..Sensor::ALL.len()).map(|i| row_id(ID_SENSOR, i)));
        ids
    }

    #[test]
    fn control_ids_are_unique_and_positive() {
        let ids = control_ids();
        let unique: HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "{ids:?}");
        assert!(ids.iter().all(|&id| id > 0));
    }

    #[test]
    fn id_ranges_do_not_overlap() {
        const {
            assert!(ID_FAN_PCT - ID_FAN_RADIO < FAN_ID_STRIDE);
            assert!(ID_FAN_RADIO + fan_row(FanGroup::Gpu) + FAN_ID_STRIDE <= ID_TURBO);
            assert!(row_id(ID_ZONE, KB_ZONES) <= ID_KB_COLOR);
            assert!(row_id(ID_SENSOR, Sensor::ALL.len()) <= ID_STATUS);
            assert!(row_id(ID_FAN_RADIO, FAN_MODES.len()) <= ID_FAN_TRACK);
        }
    }

    #[test]
    fn fan_radio_decodes_every_radio_and_nothing_else() {
        for g in FanGroup::ALL {
            for (i, m) in (0..).zip(FAN_MODES) {
                assert_eq!(fan_radio(ID_FAN_RADIO + fan_row(g) + i), Some((g, m)));
            }
            // The slider and its label are not radio buttons.
            assert_eq!(fan_radio(ID_FAN_TRACK + fan_row(g)), None);
            assert_eq!(fan_radio(ID_FAN_PCT + fan_row(g)), None);
        }
        for id in [ID_FAN_RADIO - 1, ID_FAN_RADIO - FAN_ID_STRIDE, ID_TURBO, ID_ZONE, -1, 0] {
            assert_eq!(fan_radio(id), None, "{id}");
        }
        assert_eq!(FAN_MODES.map(FanMode::as_str), ["auto", "max", "custom"]);
    }

    #[test]
    fn menu_message_and_timer_ids_are_distinct() {
        let menu = [IDM_SHOW, IDM_TURBO, IDM_EXIT, IDM_TURBO_BUTTON, IDM_ECO, IDM_BALANCED];
        assert_eq!(menu.iter().collect::<HashSet<_>>().len(), menu.len());
        // 0 is the separator's id.
        assert!(!menu.contains(&0));
        let messages = [WM_APP_TRAY, WM_APP_HWEVENT, WM_APP_SHOW, WM_APP_TOGGLE];
        assert_eq!(messages.iter().collect::<HashSet<_>>().len(), messages.len());
        assert!(messages.iter().all(|&m| (WM_APP..0xC000).contains(&m)));
        let timers = [TIMER_SENSORS, TIMER_TURBO_BUTTON, TIMER_CURVE];
        assert_eq!(timers.iter().collect::<HashSet<_>>().len(), timers.len());
        assert!(!timers.contains(&0));
    }

    #[test]
    fn tray_texts_fit_the_notify_icon_buffers() {
        let nid = NOTIFYICONDATAW::default();
        let title = TITLE.encode_utf16().count();
        assert!(title < nid.szTip.len());
        assert!(title < nid.szInfoTitle.len());
        // `tray` keeps 255 units of a balloon text plus the terminating zero.
        assert_eq!(nid.szInfo.len(), 256);
    }

    #[test]
    fn wide_round_trips() {
        assert_eq!(wide(TITLE).to_string(), TITLE);
        assert!(wide("").is_empty());
        assert_eq!(wide("CPU: 77 °C").to_string(), "CPU: 77 °C");
        assert_eq!(wide("\u{1F600}").len(), 2);
        assert_eq!(wide("a\0b").len(), 3);
    }
}
