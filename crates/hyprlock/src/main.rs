#![windows_subsystem = "windows"]
//! `hyprlock` — Native Minimalist Lockscreen Overlay for `Hyprland-Win`.
//!
//! Features:
//! - Hardware DWM Acrylic blur backdrop (`SetWindowCompositionAttribute` + `UpdateLayeredWindow`).
//! - Crisp `JetBrainsMono Nerd Font` clock (`HH:MM`), date banner, user greeting pill, and
//!   SDF anti-aliased password input capsule.
//! - Global `WH_KEYBOARD_LL` hook that suppresses `Win` / `Alt+Tab` workspace switching while locked
//!   and supports `Ctrl+A`, `Ctrl+Backspace`/`Ctrl+W`, `Ctrl+U`, `Backspace`, `Escape`, and `Enter`.

mod ui;

use anyhow::Result;
use std::path::Path;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, SYSTEMTIME, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SetFocus, VK_BACK, VK_ESCAPE, VK_LMENU, VK_LWIN, VK_MENU, VK_RETURN,
    VK_RMENU, VK_RWIN, VK_SHIFT, VK_SPACE, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, CallNextHookEx, CreateWindowExW, DefWindowProcW,
    DispatchMessageW, GetMessageW, GetSystemMetrics, KBDLLHOOKSTRUCT, KillTimer, LoadCursorW,
    PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, SetTimer, SetWindowPos,
    SetWindowsHookExW, ShowWindow, TranslateMessage, UnhookWindowsHookEx, ASFW_ANY, CS_HREDRAW,
    CS_VREDRAW, HWND_TOPMOST, IDC_ARROW, MSG, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SW_SHOWNORMAL, WH_KEYBOARD_LL, WM_CHAR, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_PAINT, WM_SYSKEYDOWN, WM_TIMER, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use ui::LockRenderModel;

const TIMER_ID_CLOCK: usize = 1;
const TIMER_ID_ANIM: usize = 2;

static LOCK_HWND: AtomicIsize = AtomicIsize::new(0);

#[derive(Debug, Clone)]
enum LockAuthState {
    /// Step 1 of first-time setup: `pass.txt` does not exist yet; enter new password
    CreateNew,
    /// Step 2 of first-time setup: confirm the password entered in Step 1
    ConfirmNew(String),
    /// Normal lock screen: verify against decrypted password from `pass.txt`
    Unlock(String),
}

struct LockState {
    buffer: String,
    auth_state: LockAuthState,
    last_anim_tick: Instant,
    model: LockRenderModel,
}

static STATE: OnceLock<Mutex<LockState>> = OnceLock::new();

fn attach_current_thread_to_default_desktop() {
    #[link(name = "user32")]
    extern "system" {
        fn OpenDesktopW(
            lpszdesktop: *const u16,
            dwflags: u32,
            finherit: i32,
            dwdesiredaccess: u32,
        ) -> *mut core::ffi::c_void;
        fn SetThreadDesktop(hdesktop: *mut core::ffi::c_void) -> i32;
    }
    unsafe {
        let desk_name: Vec<u16> = "Default\0".encode_utf16().collect();
        let h_desk = OpenDesktopW(desk_name.as_ptr(), 0, 0, 0x01FF);
        if !h_desk.is_null() {
            let _ = SetThreadDesktop(h_desk);
        }
    }
}

#[repr(C)]
struct CryptoApiBlob {
    cb_data: u32,
    pb_data: *mut u8,
}

#[link(name = "crypt32")]
extern "system" {
    fn CryptProtectData(
        p_data_in: *const CryptoApiBlob,
        sz_data_descr: *const u16,
        p_optional_entropy: *const CryptoApiBlob,
        pv_reserved: *mut core::ffi::c_void,
        p_prompt_struct: *const core::ffi::c_void,
        dw_flags: u32,
        p_data_out: *mut CryptoApiBlob,
    ) -> i32;
    fn CryptUnprotectData(
        p_data_in: *const CryptoApiBlob,
        ppsz_data_descr: *mut *mut u16,
        p_optional_entropy: *const CryptoApiBlob,
        pv_reserved: *mut core::ffi::c_void,
        p_prompt_struct: *const core::ffi::c_void,
        dw_flags: u32,
        p_data_out: *mut CryptoApiBlob,
    ) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn LocalFree(hmem: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
}

const DPAPI_ENTROPY: &[u8] = b"Hyprland-Win-LockScreen-PassTxt-v1-Entropy-2026";
const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

fn candidate_pass_txt_paths() -> Vec<String> {
    let userprofile = std::env::var("USERPROFILE").unwrap_or_default();
    let mut paths = Vec::new();
    if !userprofile.is_empty() {
        paths.push(format!("{userprofile}\\.config\\hyprwin\\pass.txt"));
    }
    paths.push("C:\\Users\\mbura\\Projects\\Other\\hyprland-win\\pass.txt".to_string());
    paths.push("pass.txt".to_string());
    paths
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02X}");
    }
    out
}

fn hex_to_bytes(hex: &str) -> Option<Vec<u8>> {
    let clean = hex.trim();
    if clean.is_empty() || clean.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(clean.len() / 2);
    let bytes = clean.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let s = std::str::from_utf8(&bytes[i..i + 2]).ok()?;
        let val = u8::from_str_radix(s, 16).ok()?;
        out.push(val);
    }
    Some(out)
}

fn dpapi_encrypt_password(plain: &str) -> Option<String> {
    let mut in_bytes = plain.as_bytes().to_vec();
    let mut entropy = DPAPI_ENTROPY.to_vec();
    let in_blob = CryptoApiBlob {
        cb_data: in_bytes.len() as u32,
        pb_data: in_bytes.as_mut_ptr(),
    };
    let ent_blob = CryptoApiBlob {
        cb_data: entropy.len() as u32,
        pb_data: entropy.as_mut_ptr(),
    };
    let mut out_blob = CryptoApiBlob {
        cb_data: 0,
        pb_data: core::ptr::null_mut(),
    };
    unsafe {
        if CryptProtectData(
            &in_blob,
            core::ptr::null(),
            &ent_blob,
            core::ptr::null_mut(),
            core::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        ) != 0
            && !out_blob.pb_data.is_null()
            && out_blob.cb_data > 0
        {
            let slice =
                std::slice::from_raw_parts(out_blob.pb_data, out_blob.cb_data as usize);
            let encoded = format!("HYPRENC1:{}", bytes_to_hex(slice));
            let _ = LocalFree(out_blob.pb_data as *mut _);
            return Some(encoded);
        }
    }
    None
}

fn dpapi_decrypt_password(ciphertext_line: &str) -> Option<String> {
    let trimmed = ciphertext_line.trim();
    let hex_part = trimmed.strip_prefix("HYPRENC1:")?;
    let mut cipher_bytes = hex_to_bytes(hex_part)?;
    let mut entropy = DPAPI_ENTROPY.to_vec();
    let in_blob = CryptoApiBlob {
        cb_data: cipher_bytes.len() as u32,
        pb_data: cipher_bytes.as_mut_ptr(),
    };
    let ent_blob = CryptoApiBlob {
        cb_data: entropy.len() as u32,
        pb_data: entropy.as_mut_ptr(),
    };
    let mut out_blob = CryptoApiBlob {
        cb_data: 0,
        pb_data: core::ptr::null_mut(),
    };
    unsafe {
        if CryptUnprotectData(
            &in_blob,
            core::ptr::null_mut(),
            &ent_blob,
            core::ptr::null_mut(),
            core::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        ) != 0
            && !out_blob.pb_data.is_null()
            && out_blob.cb_data > 0
        {
            let slice =
                std::slice::from_raw_parts(out_blob.pb_data, out_blob.cb_data as usize);
            let plain = String::from_utf8(slice.to_vec()).ok();
            let _ = LocalFree(out_blob.pb_data as *mut _);
            return plain.filter(|s| !s.is_empty());
        }
    }
    None
}

fn load_encrypted_pass_txt() -> Option<String> {
    for p in candidate_pass_txt_paths() {
        if Path::new(&p).is_file() {
            if let Ok(txt) = std::fs::read_to_string(&p) {
                for line in txt.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("HYPRENC1:") {
                        if let Some(plain) = dpapi_decrypt_password(trimmed) {
                            return Some(plain);
                        }
                    }
                }
            }
        }
    }
    None
}

fn encrypt_and_save_pass_txt(plain: &str) -> bool {
    let Some(encrypted_payload) = dpapi_encrypt_password(plain) else {
        return false;
    };
    let mut saved_any = false;
    for p in candidate_pass_txt_paths() {
        if let Some(parent) = Path::new(&p).parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        if std::fs::write(&p, format!("{encrypted_payload}\n")).is_ok() {
            saved_any = true;
        }
    }
    saved_any
}

fn mojave_stage_for_local_time() -> u8 {
    let st: SYSTEMTIME = unsafe { GetLocalTime() };
    let mins = (st.wHour as u32) * 60 + (st.wMinute as u32);
    match mins {
        60..=299 => 16,   // 01:00 - 04:59
        300..=374 => 1,   // 05:00 - 06:14
        375..=434 => 2,   // 06:15 - 07:14
        435..=509 => 3,   // 07:15 - 08:29
        510..=584 => 4,   // 08:30 - 09:44
        585..=674 => 5,   // 09:45 - 11:14
        675..=764 => 6,   // 11:15 - 12:44
        765..=854 => 7,   // 12:45 - 14:14
        855..=929 => 8,   // 14:15 - 15:29
        930..=989 => 9,   // 15:30 - 16:29
        990..=1034 => 10, // 16:30 - 17:14
        1035..=1079 => 11,// 17:15 - 17:59
        1080..=1124 => 12,// 18:00 - 18:44
        1125..=1184 => 13,// 18:45 - 19:44
        1185..=1289 => 14,// 19:45 - 21:29
        _ => 15,          // 21:30 - 00:59
    }
}

fn resolve_mojave_dynamic_wallpaper() -> Option<String> {
    use std::path::PathBuf;
    let stage = mojave_stage_for_local_time();
    let file_name = format!("mojave_dynamic_{stage}.jpeg");
    let home = std::env::var("USERPROFILE").unwrap_or_default();

    let mut dirs = vec![
        PathBuf::from(r"c:\Users\mbura\Projects\Other\Hyprland-Win\assets\wallpapers\mojave"),
        PathBuf::from("assets/wallpapers/mojave"),
        PathBuf::from("../assets/wallpapers/mojave"),
    ];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(root) = exe.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
            dirs.push(root.join("assets").join("wallpapers").join("mojave"));
        }
    }
    if !home.is_empty() {
        dirs.push(PathBuf::from(&home).join(r".config\Hyprwin\wallpapers\mojave"));
        dirs.push(PathBuf::from(&home).join(r".config\hypr\wallpapers\mojave"));
    }

    for dir in dirs {
        let candidate = dir.join(&file_name);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    None
}

fn resolve_normal_wallpaper_path() -> Option<String> {
    use std::path::PathBuf;
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let cfg_dir = PathBuf::from(&home).join(".config").join("hyprwin");
    let candidates = [
        cfg_dir.join("hyprpaper.conf"),
        cfg_dir.join("hyprland.conf"),
        PathBuf::from("config/hyprpaper.conf"),
    ];
    for path in candidates {
        if let Ok(src) = std::fs::read_to_string(&path) {
            for raw in src.lines() {
                let line = raw.split('#').next().unwrap_or("").trim();
                if let Some((k, v)) = line.split_once('=') {
                    if k.trim() == "wallpaper" {
                        let wp = v.split(',').last().unwrap_or(v).trim();
                        if wp.eq_ignore_ascii_case("dynamic:mojave")
                            || wp.eq_ignore_ascii_case("mojave")
                            || wp.eq_ignore_ascii_case("dynamic")
                        {
                            if let Some(dyn_wp) = resolve_mojave_dynamic_wallpaper() {
                                return Some(dyn_wp);
                            }
                        }
                        if !wp.is_empty() && Path::new(wp).exists() {
                            return Some(wp.to_string());
                        }
                        let rel = wp
                            .trim_start_matches(".config/hyprwin/")
                            .trim_start_matches(".config\\hyprwin\\")
                            .trim_start_matches(".config/hypr/")
                            .trim_start_matches(".config\\hypr\\")
                            .trim_start_matches(".config/")
                            .trim_start_matches(".config\\");
                        let in_cfg = cfg_dir.join(rel);
                        if in_cfg.is_file() {
                            return Some(in_cfg.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }
    }
    if let Some(dyn_wp) = resolve_mojave_dynamic_wallpaper() {
        return Some(dyn_wp);
    }
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    let fallback_wallpapers = [
        cfg_dir.join("wallpaper.bmp"),
        cfg_dir.join("wallpaper.png"),
        PathBuf::from(r"c:\Users\mbura\Projects\Other\Hyprland-Win\.config\wallpaper.bmp"),
        PathBuf::from("config/wallpaper.bmp"),
        PathBuf::from(format!("{appdata}\\Microsoft\\Windows\\Themes\\TranscodedWallpaper")),
    ];
    for fb in fallback_wallpapers {
        if fb.exists() {
            return Some(fb.to_string_lossy().to_string());
        }
    }
    None
}

fn resolve_user_avatar_path() -> Option<String> {
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    let dirs = [
        format!("{appdata}\\Microsoft\\Windows\\AccountPictures"),
        "C:\\Users\\Public\\AccountPictures".to_string(),
        "C:\\ProgramData\\Microsoft\\User Account Pictures".to_string(),
    ];
    for d in &dirs {
        if let Ok(entries) = std::fs::read_dir(d) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file() {
                    let ext = p
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "bmp") {
                        return Some(p.to_string_lossy().to_string());
                    }
                }
            }
        }
    }
    None
}

fn query_battery_percent() -> u8 {
    #[repr(C)]
    struct SystemPowerStatus {
        ac_line_status: u8,
        battery_flag: u8,
        battery_life_percent: u8,
        system_status_flag: u8,
        battery_life_time: u32,
        battery_full_life_time: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetSystemPowerStatus(lp_system_power_status: *mut SystemPowerStatus) -> i32;
    }
    unsafe {
        let mut sps = SystemPowerStatus {
            ac_line_status: 1,
            battery_flag: 0,
            battery_life_percent: 100,
            system_status_flag: 0,
            battery_life_time: 0,
            battery_full_life_time: 0,
        };
        if GetSystemPowerStatus(&mut sps) != 0 && sps.battery_life_percent <= 100 {
            return sps.battery_life_percent;
        }
    }
    100
}

fn trigger_system_shutdown() {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("shutdown.exe")
        .args(["/s", "/t", "0", "/f"])
        .creation_flags(0x0800_0000)
        .spawn();
}

fn trigger_system_restart() {
    use std::os::windows::process::CommandExt;
    // Remove the session boot-unlocked flag so Hyprland-Win immediately opens `hyprlock` upon reboot
    let _ = std::fs::remove_file(std::env::temp_dir().join("hyprland_win_boot_unlocked.flag"));
    // `/g` triggers Windows 11 ARSO (Automatic Restart Sign-On) so `winlogon.exe` + `lsass.exe`
    // automatically sign into the user session after restart without stopping at `LogonUI.exe`,
    // booting straight into `hyprlock`!
    let _ = std::process::Command::new("shutdown.exe")
        .args(["/g", "/t", "0", "/f"])
        .creation_flags(0x0800_0000)
        .spawn();
}

fn trigger_system_sleep() {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("rundll32.exe")
        .args(["powrprof.dll,SetSuspendState", "0,1,0"])
        .creation_flags(0x0800_0000)
        .spawn();
}

fn query_time_and_date() -> (String, String) {
    let st: SYSTEMTIME = unsafe { GetLocalTime() };
    let short_day = match st.wDayOfWeek {
        0 => "Sun",
        1 => "Mon",
        2 => "Tue",
        3 => "Wed",
        4 => "Thu",
        5 => "Fri",
        _ => "Sat",
    };
    let (hour12, ampm) = match st.wHour {
        0 => (12, "AM"),
        1..=11 => (st.wHour, "AM"),
        12 => (12, "PM"),
        _ => (st.wHour - 12, "PM"),
    };
    let time_str = format!("{short_day} {hour12}:{:02} {ampm}", st.wMinute);
    let weekday = match st.wDayOfWeek {
        0 => "SUNDAY",
        1 => "MONDAY",
        2 => "TUESDAY",
        3 => "WEDNESDAY",
        4 => "THURSDAY",
        5 => "FRIDAY",
        _ => "SATURDAY",
    };
    let month = match st.wMonth {
        1 => "JANUARY",
        2 => "FEBRUARY",
        3 => "MARCH",
        4 => "APRIL",
        5 => "MAY",
        6 => "JUNE",
        7 => "JULY",
        8 => "AUGUST",
        9 => "SEPTEMBER",
        10 => "OCTOBER",
        11 => "NOVEMBER",
        _ => "DECEMBER",
    };
    let date_str = format!("{weekday}, {month} {:02}, {}", st.wDay, st.wYear);
    (time_str, date_str)
}

fn main() -> Result<()> {
    attach_current_thread_to_default_desktop();

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    // Prevent duplicate lockscreen instances if `hyprlock` is already active
    unsafe {
        let cls: Vec<u16> = "HyprLockOverlayClass\0".encode_utf16().collect();
        if let Ok(existing) =
            windows::Win32::UI::WindowsAndMessaging::FindWindowW(PCWSTR(cls.as_ptr()), PCWSTR::null())
        {
            if !existing.0.is_null() {
                return Ok(());
            }
        }
    }

    let power_menu_mode = std::env::args().any(|a| a == "--power-menu" || a == "--shutdown");
    let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN).max(1280) };
    let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN).max(720) };
    let username = std::env::var("USERNAME").unwrap_or_else(|_| "user".to_string());
    let (time_str, date_str) = query_time_and_date();
    let (auth_state, initial_setup_step) = match load_encrypted_pass_txt() {
        Some(saved_pw) => (LockAuthState::Unlock(saved_pw), 0u8),
        None => (LockAuthState::CreateNew, 1u8),
    };

    let _ = STATE.set(Mutex::new(LockState {
        buffer: String::new(),
        auth_state,
        last_anim_tick: Instant::now(),
        model: LockRenderModel {
            width: screen_w,
            height: screen_h,
            time_str,
            seconds_str: "00".to_string(),
            date_str,
            username,
            input_len: 0,
            input_selected: false,
            auth_failed: false,
            setup_step: initial_setup_step,
            dot_scales: Vec::new(),
            anim_dots_width: 0.0,
            typing_pulse: 0.0,
            fail_shake: 0.0,
            unlocking: false,
            unlock_progress: 0.0,
            wallpaper_path: resolve_normal_wallpaper_path(),
            avatar_path: resolve_user_avatar_path(),
            kbd_layout_str: "U.S.".to_string(),
            battery_percent: query_battery_percent(),
            power_menu_mode,
            ..Default::default()
        },
    }));

    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let class_name: Vec<u16> = "HyprLockOverlayClass\0".encode_utf16().collect();
        let title_name: Vec<u16> = "HyprLock\0".encode_utf16().collect();

        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(lock_wnd_proc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title_name.as_ptr()),
            WS_POPUP,
            0,
            0,
            screen_w,
            screen_h,
            HWND::default(),
            None,
            hinstance,
            None,
        )?;

        LOCK_HWND.store(hwnd.0 as isize, Ordering::SeqCst);

        ui::set_overlay_lock_cutout(screen_w, screen_h, true);
        ui::enable_window_acrylic(hwnd, 0x991B_1716);
        refresh_lockscreen(hwnd);

        let _ = SetTimer(hwnd, TIMER_ID_CLOCK, 1000, None);
        let _ = SetTimer(hwnd, TIMER_ID_ANIM, 7, None);
        let kbd_hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(lock_ll_kbd_proc), None, 0)?;

        let _ = AllowSetForegroundWindow(ASFW_ANY);
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            screen_w,
            screen_h,
            SWP_SHOWWINDOW,
        );
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let _ = UnhookWindowsHookEx(kbd_hook);
        ui::set_overlay_lock_cutout(screen_w, screen_h, false);
    }

    Ok(())
}

unsafe fn refresh_lockscreen(hwnd: HWND) {
    if let Some(st_mutex) = STATE.get() {
        if let Ok(st) = st_mutex.lock() {
            ui::render_lockscreen(hwnd, &st.model);
        }
    }
}

unsafe extern "system" fn lock_ll_kbd_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    #[link(name = "user32")]
    extern "system" {
        fn GetKeyState(n_virt_key: i32) -> i16;
        fn GetKeyboardLayout(id_thread: u32) -> isize;
        fn GetWindowThreadProcessId(hwnd: HWND, lpdw_process_id: *mut u32) -> u32;
        fn GetForegroundWindow() -> HWND;
        fn ToUnicodeEx(
            w_virt_key: u32,
            w_scan_code: u32,
            lp_key_state: *const u8,
            pwsz_buff: *mut u16,
            cch_buff: i32,
            w_flags: u32,
            dwhkl: isize,
        ) -> i32;
    }

    if code < 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
    let vk = kb.vkCode as u16;
    let msg = wparam.0 as u32;
    let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;

    // Suppress Win keys while locked
    if vk == VK_LWIN.0 || vk == VK_RWIN.0 {
        return LRESULT(1);
    }
    // Pass modifier transitions through so OS tracks Shift / Ctrl / AltGr state
    if matches!(
        vk,
        x if x == VK_SHIFT.0
            || x == 0xA0
            || x == 0xA1
            || x == 0x11
            || x == 0xA2
            || x == 0xA3
            || x == VK_MENU.0
            || x == VK_LMENU.0
            || x == VK_RMENU.0
            || x == 0x14 // VK_CAPITAL (Caps Lock)
    ) {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let lalt_down = (GetAsyncKeyState(VK_LMENU.0 as i32) as u16 & 0x8000) != 0;
    let ralt_down = (GetAsyncKeyState(VK_RMENU.0 as i32) as u16 & 0x8000) != 0;
    let alt_down = (GetAsyncKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0 || lalt_down || ralt_down;
    let ctrl_down = (GetAsyncKeyState(0x11) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(0xA2) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(0xA3) as u16 & 0x8000) != 0;

    // Note: Right-Alt (`AltGr`) on Turkish / European keyboards sets `ctrl_down && ralt_down`.
    // Block system Alt shortcuts (`Alt+Tab`, `Alt+Esc`, `Alt+F4`, `Alt+Space`), or any Left-Alt combo!
    let is_altgr = ralt_down && !lalt_down;
    if alt_down && (!is_altgr || matches!(vk, x if x == VK_TAB.0 || x == VK_ESCAPE.0 || x == 0x73 || x == VK_SPACE.0)) {
        return LRESULT(1);
    }

    if is_down {
        let hwnd_val = LOCK_HWND.load(Ordering::SeqCst);
        if hwnd_val != 0 {
            let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);

            // Handle Ctrl+A / Ctrl+Backspace / Ctrl+W / Ctrl+U when NOT pressing AltGr
            if ctrl_down && !is_altgr {
                if !matches!(vk, 0x11 | 0xA2 | 0xA3) {
                    let _ = PostMessageW(hwnd, WM_KEYDOWN, WPARAM(vk as usize), LPARAM(1));
                }
                return LRESULT(1);
            }

            if matches!(vk, x if x == VK_RETURN.0 || x == VK_ESCAPE.0 || x == VK_BACK.0 || x == 0x2E) {
                let _ = PostMessageW(hwnd, WM_KEYDOWN, WPARAM(vk as usize), LPARAM(0));
                return LRESULT(1);
            }

            // Build 256-byte keyboard state for `ToUnicodeEx` respecting Shift, CapsLock, and AltGr
            let mut kbd_state = [0u8; 256];
            let shift = (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0
                || (GetAsyncKeyState(0xA0) as u16 & 0x8000) != 0
                || (GetAsyncKeyState(0xA1) as u16 & 0x8000) != 0;
            if shift {
                kbd_state[VK_SHIFT.0 as usize] = 0x80;
                kbd_state[0xA0] = 0x80;
            }
            if (GetKeyState(0x14) & 1) != 0 {
                kbd_state[0x14] = 0x01; // VK_CAPITAL toggled
            }
            if is_altgr {
                kbd_state[0x11] = 0x80; // VK_CONTROL
                kbd_state[0xA2] = 0x80;
                kbd_state[VK_MENU.0 as usize] = 0x80; // VK_MENU
                kbd_state[VK_RMENU.0 as usize] = 0x80;
            }

            let fg = GetForegroundWindow();
            let tid = if !fg.0.is_null() {
                GetWindowThreadProcessId(fg, core::ptr::null_mut())
            } else {
                0
            };
            let mut hkl = GetKeyboardLayout(tid);
            if hkl == 0 {
                hkl = GetKeyboardLayout(0);
            }

            let mut out_utf16 = [0u16; 8];
            // Flag 0x04 (Windows 10 1607+): do not modify internal dead-key state of keyboard driver
            let count = ToUnicodeEx(
                vk as u32,
                kb.scanCode,
                kbd_state.as_ptr(),
                out_utf16.as_mut_ptr(),
                out_utf16.len() as i32,
                0x04,
                hkl,
            );
            if count > 0 {
                let s = String::from_utf16_lossy(&out_utf16[..count as usize]);
                for ch in s.chars() {
                    if ch >= ' ' && ch != '\x7f' {
                        let _ = PostMessageW(hwnd, WM_CHAR, WPARAM(ch as u32 as usize), LPARAM(0));
                    }
                }
                return LRESULT(1);
            }
        }
    }

    // Swallow all other keys while locked so nothing leaks down the hook chain to the desktop!
    LRESULT(1)
}

unsafe extern "system" fn lock_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            let mx = (lparam.0 & 0xFFFF) as i16 as i32;
            let my = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut changed = false;
            if let Some(st_mutex) = STATE.get() {
                if let Ok(mut st) = st_mutex.lock() {
                    let hit = ui::hit_test_lock_button_with_mode(
                        st.model.width,
                        st.model.height,
                        mx,
                        my,
                        st.model.power_menu_mode,
                    );
                    if st.model.hovered_button != hit {
                        st.model.hovered_button = hit;
                        changed = true;
                    }
                }
            }
            if changed {
                refresh_lockscreen(hwnd);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let mx = (lparam.0 & 0xFFFF) as i16 as i32;
            let my = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut action = 0u8;
            if let Some(st_mutex) = STATE.get() {
                if let Ok(mut st) = st_mutex.lock() {
                    let hit = ui::hit_test_lock_button_with_mode(
                        st.model.width,
                        st.model.height,
                        mx,
                        my,
                        st.model.power_menu_mode,
                    );
                    if st.model.power_menu_mode {
                        match hit {
                            1 => action = 1, // Shut Down
                            2 => action = 2, // Reboot
                            3 => action = 3, // Sleep
                            5 => {
                                // Switch from Power Menu mode into normal Lock Screen mode
                                st.model.power_menu_mode = false;
                                st.model.hovered_button = 0;
                            }
                            _ => {
                                // Clicking outside the power buttons cancels the shutdown menu
                                st.model.unlocking = true;
                                action = 99;
                            }
                        }
                    } else {
                        match hit {
                            1 => action = 1, // Shut Down
                            2 => action = 2, // Restart
                            3 => action = 3, // Sleep
                            4 => {
                                if st.model.input_len == 0 {
                                    st.model.show_hint_popover = !st.model.show_hint_popover;
                                } else {
                                    action = 4; // Submit
                                }
                            }
                            _ => {
                                if st.model.show_hint_popover {
                                    st.model.show_hint_popover = false;
                                }
                            }
                        }
                    }
                }
            }
            match action {
                1 => trigger_system_shutdown(),
                2 => trigger_system_restart(),
                3 => trigger_system_sleep(),
                4 => {
                    let _ = PostMessageW(hwnd, WM_KEYDOWN, WPARAM(VK_RETURN.0 as usize), LPARAM(0));
                }
                99 => {
                    let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
                    return LRESULT(0);
                }
                _ => {}
            }
            refresh_lockscreen(hwnd);
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_ID_CLOCK {
                let (time_str, date_str) = query_time_and_date();
                let new_wp = resolve_normal_wallpaper_path();
                let mut changed = false;
                if let Some(st_mutex) = STATE.get() {
                    if let Ok(mut st) = st_mutex.lock() {
                        if st.model.time_str != time_str
                            || st.model.date_str != date_str
                            || st.model.wallpaper_path != new_wp
                        {
                            st.model.time_str = time_str;
                            st.model.date_str = date_str;
                            st.model.wallpaper_path = new_wp;
                            changed = true;
                        }
                    }
                }
                if changed {
                    refresh_lockscreen(hwnd);
                }
            } else if wparam.0 == TIMER_ID_ANIM {
                let mut needs_repaint = false;
                let mut unlock_done = false;
                let mut slide_info: Option<(i32, i32, f32)> = None;
                if let Some(st_mutex) = STATE.get() {
                    if let Ok(mut st) = st_mutex.lock() {
                        let now = Instant::now();
                        let dt = now
                            .duration_since(st.last_anim_tick)
                            .as_secs_f32()
                            .clamp(0.001, 0.045);
                        st.last_anim_tick = now;
                        let (rep, done) = ui::step_lock_animations(&mut st.model, dt);
                        needs_repaint = rep;
                        unlock_done = done;
                        if st.model.unlocking {
                            slide_info = Some((
                                st.model.width,
                                st.model.height,
                                st.model.unlock_progress,
                            ));
                        }
                    }
                }
                if let Some((sw, sh, prog)) = slide_info {
                    let ease = ui::ease_in_out_cubic(prog);
                    let y_offset = -(ease * sh as f32).round() as i32;
                    let _ = SetWindowPos(
                        hwnd,
                        HWND_TOPMOST,
                        0,
                        y_offset,
                        sw,
                        sh,
                        SWP_NOACTIVATE,
                    );
                }
                if needs_repaint {
                    refresh_lockscreen(hwnd);
                }
                if unlock_done {
                    let _ = KillTimer(hwnd, TIMER_ID_ANIM);
                    let _ = KillTimer(hwnd, TIMER_ID_CLOCK);
                    let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let vk = wparam.0 as u16;
            let ctrl_down = lparam.0 == 1;

            if ctrl_down {
                if let Some(st_mutex) = STATE.get() {
                    if let Ok(mut st) = st_mutex.lock() {
                        if st.model.unlocking {
                            return LRESULT(0);
                        }
                        match vk {
                            0x41 => {
                                st.model.input_selected = !st.buffer.is_empty();
                                st.model.typing_pulse = 0.7;
                            }
                            x if x == VK_BACK.0 || x == 0x57 || x == 0x55 => {
                                st.buffer.clear();
                                st.model.raw_text.clear();
                                st.model.input_len = 0;
                                st.model.dot_scales.clear();
                                st.model.input_selected = false;
                                st.model.auth_failed = false;
                                st.model.typing_pulse = 0.85;
                            }
                            _ => {}
                        }
                    }
                }
                refresh_lockscreen(hwnd);
                return LRESULT(0);
            }

            if vk == VK_ESCAPE.0 {
                let mut close_power_menu = false;
                if let Some(st_mutex) = STATE.get() {
                    if let Ok(mut st) = st_mutex.lock() {
                        if st.model.power_menu_mode {
                            st.model.unlocking = true;
                            close_power_menu = true;
                        } else {
                            if st.model.unlocking {
                                return LRESULT(0);
                            }
                            if st.buffer.is_empty() {
                                if matches!(st.auth_state, LockAuthState::ConfirmNew(_)) {
                                    st.auth_state = LockAuthState::CreateNew;
                                    st.model.setup_step = 1;
                                }
                            }
                            st.buffer.clear();
                            st.model.raw_text.clear();
                            st.model.input_len = 0;
                            st.model.dot_scales.clear();
                            st.model.input_selected = false;
                            st.model.auth_failed = false;
                            st.model.typing_pulse = 0.8;
                        }
                    }
                }
                if close_power_menu {
                    let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
                    return LRESULT(0);
                }
                refresh_lockscreen(hwnd);
                return LRESULT(0);
            }

            if vk == VK_BACK.0 || vk == 0x2E {
                if let Some(st_mutex) = STATE.get() {
                    if let Ok(mut st) = st_mutex.lock() {
                        if st.model.unlocking {
                            return LRESULT(0);
                        }
                        if st.model.input_selected {
                            st.buffer.clear();
                            st.model.dot_scales.clear();
                            st.model.input_selected = false;
                        } else if vk == VK_BACK.0 {
                            st.buffer.pop();
                            st.model.dot_scales.pop();
                        }
                        st.model.raw_text = st.buffer.clone();
                        st.model.input_len = st.buffer.chars().count();
                        st.model.auth_failed = false;
                        st.model.typing_pulse = 0.75;
                    }
                }
                refresh_lockscreen(hwnd);
                return LRESULT(0);
            }

            if vk == VK_RETURN.0 {
                let mut cmd_action = 0u8;
                if let Some(st_mutex) = STATE.get() {
                    if let Ok(mut st) = st_mutex.lock() {
                        if st.model.unlocking {
                            return LRESULT(0);
                        }
                        let submitted = st.buffer.clone();
                        let trimmed_cmd = submitted.trim();
                        if trimmed_cmd.eq_ignore_ascii_case(">restart") {
                            cmd_action = 2;
                        } else if trimmed_cmd.eq_ignore_ascii_case(">shutdown")
                            || trimmed_cmd.eq_ignore_ascii_case(">poweroff")
                        {
                            cmd_action = 1;
                        } else if trimmed_cmd.eq_ignore_ascii_case(">sleep")
                            || trimmed_cmd.eq_ignore_ascii_case(">suspend")
                        {
                            cmd_action = 3;
                        }
                        if cmd_action != 0 {
                            st.buffer.clear();
                            st.model.raw_text.clear();
                            st.model.input_len = 0;
                            st.model.dot_scales.clear();
                            st.model.input_selected = false;
                            st.model.auth_failed = false;
                        } else {
                        match st.auth_state.clone() {
                            LockAuthState::CreateNew => {
                                if submitted.is_empty() {
                                    st.model.auth_failed = true;
                                    st.model.fail_shake = 1.0;
                                    st.model.typing_pulse = 1.0;
                                    st.last_anim_tick = Instant::now();
                                } else {
                                    st.auth_state = LockAuthState::ConfirmNew(submitted);
                                    st.model.setup_step = 2;
                                    st.buffer.clear();
                                    st.model.raw_text.clear();
                                    st.model.input_len = 0;
                                    st.model.dot_scales.clear();
                                    st.model.input_selected = false;
                                    st.model.auth_failed = false;
                                    st.model.typing_pulse = 1.0;
                                    st.last_anim_tick = Instant::now();
                                }
                            }
                            LockAuthState::ConfirmNew(first_pw) => {
                                if !submitted.is_empty() && submitted == first_pw {
                                    let _ = encrypt_and_save_pass_txt(&submitted);
                                    st.auth_state = LockAuthState::Unlock(submitted);
                                    st.model.unlocking = true;
                                    st.model.unlock_progress = 0.0;
                                    st.model.auth_failed = false;
                                    st.model.typing_pulse = 1.0;
                                    st.last_anim_tick = Instant::now();
                                    ui::set_overlay_lock_cutout(
                                        st.model.width,
                                        st.model.height,
                                        false,
                                    );
                                } else {
                                    st.auth_state = LockAuthState::CreateNew;
                                    st.model.setup_step = 1;
                                    st.buffer.clear();
                                    st.model.raw_text.clear();
                                    st.model.input_len = 0;
                                    st.model.dot_scales.clear();
                                    st.model.input_selected = false;
                                    st.model.auth_failed = true;
                                    st.model.fail_shake = 1.0;
                                    st.model.typing_pulse = 1.0;
                                    st.last_anim_tick = Instant::now();
                                }
                            }
                            LockAuthState::Unlock(saved_pw) => {
                                if !submitted.is_empty() && submitted == saved_pw {
                                    st.model.unlocking = true;
                                    st.model.unlock_progress = 0.0;
                                    st.model.auth_failed = false;
                                    st.model.typing_pulse = 1.0;
                                    st.last_anim_tick = Instant::now();
                                    ui::set_overlay_lock_cutout(
                                        st.model.width,
                                        st.model.height,
                                        false,
                                    );
                                } else {
                                    st.buffer.clear();
                                    st.model.raw_text.clear();
                                    st.model.input_len = 0;
                                    st.model.dot_scales.clear();
                                    st.model.input_selected = false;
                                    st.model.auth_failed = true;
                                    st.model.fail_shake = 1.0;
                                    st.model.typing_pulse = 1.0;
                                    st.last_anim_tick = Instant::now();
                                }
                            }
                        }
                        }
                    }
                }
                match cmd_action {
                    1 => trigger_system_shutdown(),
                    2 => trigger_system_restart(),
                    3 => trigger_system_sleep(),
                    _ => {}
                }
                refresh_lockscreen(hwnd);
                return LRESULT(0);
            }

            LRESULT(0)
        }
        WM_CHAR => {
            if let Some(ch) = char::from_u32(wparam.0 as u32) {
                if ch >= ' ' && ch != '\x7f' {
                    if let Some(st_mutex) = STATE.get() {
                        if let Ok(mut st) = st_mutex.lock() {
                            if st.model.unlocking || st.model.power_menu_mode {
                                return LRESULT(0);
                            }
                            if st.model.input_selected {
                                st.buffer.clear();
                                st.model.dot_scales.clear();
                                st.model.input_selected = false;
                            }
                            st.buffer.push(ch);
                            st.model.raw_text = st.buffer.clone();
                            st.model.input_len = st.buffer.chars().count();
                            if st.model.dot_scales.len() < 20 {
                                st.model.dot_scales.push(0.15);
                            }
                            st.model.auth_failed = false;
                            st.model.typing_pulse = 1.0;
                            st.last_anim_tick = Instant::now();
                        }
                    }
                    refresh_lockscreen(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            refresh_lockscreen(hwnd);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_CLOSE => {
            // Only allow closing `hyprlock` if password authentication succeeded (`unlocking == true`)!
            let can_close = STATE
                .get()
                .and_then(|m| m.lock().ok().map(|st| st.model.unlocking))
                .unwrap_or(false);
            if can_close {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
