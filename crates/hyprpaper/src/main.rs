#![windows_subsystem = "windows"]
//! `hyprpaper` — Minimalist Wallpaper & Desktop Backdrop Daemon for `Hyprland-Win`.
//!
//! Creates a bottommost desktop backdrop window (`HWND_BOTTOM`, `WS_POPUP | WS_VISIBLE`,
//! `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`) spanning `(0, 0, screen_w, screen_h)` so when
//! `explorer.exe` is hollowed/terminated, tiled windows sit over a clean minimalist dark-slate
//! or custom `.bmp` wallpaper backdrop instead of a raw black void.

mod ui;

use anyhow::Result;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, SYSTEMTIME, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, UpdateWindow, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowW, GetClientRect, GetMessageW,
    GetSystemMetrics, GetWindow, IsWindowVisible, KillTimer, LoadCursorW, PostQuitMessage,
    RegisterClassW, SetTimer, SetWindowPos, ShowWindow, TranslateMessage, WINDOWPOS, CS_HREDRAW,
    CS_VREDRAW, GW_HWNDNEXT, HWND_BOTTOM, IDC_ARROW, MSG, SM_CXSCREEN, SM_CXVIRTUALSCREEN,
    SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SW_SHOWNOACTIVATE, WM_APP, WM_DESTROY, WM_DISPLAYCHANGE, WM_ERASEBKGND,
    WM_PAINT, WM_TIMER, WM_WINDOWPOSCHANGING, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_POPUP, WS_VISIBLE,
};

use ui::PaperRenderModel;

const TIMER_ID_BOTTOM: usize = 1;
const TIMER_ID_FADE: usize = 2;
const WM_PAPER_SET_STAGE: u32 = WM_APP + 50;

static PAPER_MODEL: OnceLock<Mutex<PaperRenderModel>> = OnceLock::new();
static OVERRIDE_MOJAVE_STAGE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static DISCONNECTED_TICKS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static SEEN_COMPOSITOR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn is_hyprland_win_compositor_alive() -> bool {
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
        for cls_name in ["HyprlandWinDCompOverlayClass\0", "HyprlandWinNativeDCompOverlay\0"] {
            let cls: Vec<u16> = cls_name.encode_utf16().collect();
            if let Ok(h) = FindWindowW(PCWSTR(cls.as_ptr()), PCWSTR::null()) {
                if !h.0.is_null() {
                    return true;
                }
            }
        }
    }
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("\\\\.\\pipe\\hyprland-win-ipc")
    {
        Ok(_) => true,
        Err(e) => e.raw_os_error() == Some(5) || e.raw_os_error() == Some(231),
    }
}

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

fn main() -> Result<()> {
    attach_current_thread_to_default_desktop();

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let cls: Vec<u16> = "HyprPaperBackdropClass\0".encode_utf16().collect();
        if let Ok(existing) = FindWindowW(PCWSTR(cls.as_ptr()), PCWSTR::null()) {
            if !existing.0.is_null() {
                return Ok(());
            }
        }
    }

    let virt_x = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let virt_y = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let virt_w = unsafe {
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        if vw > 0 { vw } else { GetSystemMetrics(SM_CXSCREEN).max(1920) }
    };
    let virt_h = unsafe {
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if vh > 0 { vh } else { GetSystemMetrics(SM_CYSCREEN).max(1080) }
    };

    let wallpaper_path = parse_hyprpaper_config();
    let _ = PAPER_MODEL.set(Mutex::new(PaperRenderModel::new(
        virt_w,
        virt_h,
        wallpaper_path,
    )));

    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let class_name: Vec<u16> = "HyprPaperBackdropClass\0".encode_utf16().collect();
        let window_title: Vec<u16> = "HyprPaper\0".encode_utf16().collect();

        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(paper_wnd_proc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(window_title.as_ptr()),
            WS_POPUP | WS_VISIBLE,
            virt_x,
            virt_y,
            virt_w,
            virt_h,
            HWND::default(),
            None,
            hinstance,
            None,
        )?;

        let _ = SetWindowPos(
            hwnd,
            HWND_BOTTOM,
            virt_x,
            virt_y,
            virt_w,
            virt_h,
            SWP_NOACTIVATE,
        );
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetTimer(hwnd, TIMER_ID_BOTTOM, 1000, None);

        // Allow unelevated `hyprctl --loop-dynbg` to send `WM_PAPER_SET_STAGE` across UIPI
        #[link(name = "user32")]
        extern "system" {
            fn ChangeWindowMessageFilter(message: u32, dw_flag: u32) -> i32;
        }
        let _ = ChangeWindowMessageFilter(WM_PAPER_SET_STAGE, 1); // MSGFLT_ADD = 1

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

unsafe extern "system" fn paper_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_WINDOWPOSCHANGING => {
            // Force `hyprpaper` to remain strictly at `HWND_BOTTOM` and locked to the virtual desktop bounds
            let wp = &mut *(lparam.0 as *mut WINDOWPOS);
            wp.hwndInsertAfter = HWND_BOTTOM;
            wp.x = GetSystemMetrics(SM_XVIRTUALSCREEN);
            wp.y = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
            wp.cx = if vw > 0 { vw } else { GetSystemMetrics(SM_CXSCREEN).max(1920) };
            wp.cy = if vh > 0 { vh } else { GetSystemMetrics(SM_CYSCREEN).max(1080) };
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_ID_FADE {
                let mut is_done = false;
                if let Some(m) = PAPER_MODEL.get() {
                    if let Ok(mut model) = m.lock() {
                        let elapsed = model.fade_start.elapsed().as_secs_f32();
                        let progress = (elapsed / model.fade_duration_secs.max(0.05)).clamp(0.0, 1.0);
                        model.fade_progress = progress;
                        if progress >= 1.0 {
                            model.prev_wallpaper_path = None;
                            is_done = true;
                        }
                    }
                }
                let _ = InvalidateRect(hwnd, None, false);
                let _ = UpdateWindow(hwnd);
                if is_done {
                    let _ = KillTimer(hwnd, TIMER_ID_FADE);
                }
                return LRESULT(0);
            }

            if wparam.0 == TIMER_ID_BOTTOM {
                if is_hyprland_win_compositor_alive() {
                    SEEN_COMPOSITOR.store(true, std::sync::atomic::Ordering::SeqCst);
                    DISCONNECTED_TICKS.store(0, std::sync::atomic::Ordering::SeqCst);
                } else if SEEN_COMPOSITOR.load(std::sync::atomic::Ordering::SeqCst) {
                    let prev = DISCONNECTED_TICKS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if prev + 1 >= 2 {
                        PostQuitMessage(0);
                        return LRESULT(0);
                    }
                }

                let mut has_visible_behind = false;
                let mut cur = GetWindow(hwnd, GW_HWNDNEXT).unwrap_or_default();
                let mut steps = 0;
                while !cur.0.is_null() && steps < 256 {
                    if IsWindowVisible(cur).as_bool() {
                        has_visible_behind = true;
                        break;
                    }
                    cur = GetWindow(cur, GW_HWNDNEXT).unwrap_or_default();
                    steps += 1;
                }
                if has_visible_behind {
                    let _ = SetWindowPos(
                        hwnd,
                        HWND_BOTTOM,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }

                // Dynamic Daytime Wallpaper Check & Live `<userfolder>/.config/hyprwin/hyprpaper.conf` Hot-Reload
                static LAST_PAPER_CFG_HASH: std::sync::atomic::AtomicU64 =
                    std::sync::atomic::AtomicU64::new(0);
                let cur_cfg_hash = current_paper_config_hash();
                let prev_cfg_hash =
                    LAST_PAPER_CFG_HASH.swap(cur_cfg_hash, std::sync::atomic::Ordering::SeqCst);
                let cfg_edited = prev_cfg_hash != 0 && cur_cfg_hash != prev_cfg_hash;
                if cfg_edited {
                    OVERRIDE_MOJAVE_STAGE.store(0, std::sync::atomic::Ordering::SeqCst);
                }

                if cfg_edited || OVERRIDE_MOJAVE_STAGE.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                    let new_wp = parse_hyprpaper_config();
                    let fade_secs = if cfg_edited { 0.55 } else { 15.0 };
                    let mut should_start_fade = false;
                    let mut cur_w = 1920;
                    let mut cur_h = 1080;
                    if let Some(m) = PAPER_MODEL.get() {
                        if let Ok(mut model) = m.lock() {
                            cur_w = model.width;
                            cur_h = model.height;
                            if model.transition_to(new_wp.clone(), fade_secs) {
                                should_start_fade = true;
                            }
                        }
                    }
                    if should_start_fade {
                        ui::preload_wallpaper_cache(hwnd, cur_w, cur_h, new_wp.as_deref());
                        let _ = SetTimer(hwnd, TIMER_ID_FADE, 14, None);
                        let _ = InvalidateRect(hwnd, None, false);
                        let _ = UpdateWindow(hwnd);
                    }
                }
            }
            LRESULT(0)
        }
        WM_PAPER_SET_STAGE => {
            let stage = (wparam.0 as u8).min(16);
            let fade_secs = if lparam.0 > 0 {
                (lparam.0 as f32 / 1000.0).clamp(0.10, 120.0)
            } else {
                15.0
            };
            OVERRIDE_MOJAVE_STAGE.store(stage, std::sync::atomic::Ordering::SeqCst);
            let new_wp = if stage >= 1 {
                resolve_mojave_stage_wallpaper(stage).or_else(parse_hyprpaper_config)
            } else {
                parse_hyprpaper_config()
            };
            let mut should_start_fade = false;
            let mut cur_w = 1920;
            let mut cur_h = 1080;
            if let Some(m) = PAPER_MODEL.get() {
                if let Ok(mut model) = m.lock() {
                    cur_w = model.width;
                    cur_h = model.height;
                    if model.transition_to(new_wp.clone(), fade_secs) {
                        should_start_fade = true;
                    }
                }
            }
            if should_start_fade {
                ui::preload_wallpaper_cache(hwnd, cur_w, cur_h, new_wp.as_deref());
                let _ = SetTimer(hwnd, TIMER_ID_FADE, 14, None);
                let _ = InvalidateRect(hwnd, None, false);
                let _ = UpdateWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let sw = {
                let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
                if vw > 0 { vw } else { GetSystemMetrics(SM_CXSCREEN).max(1920) }
            };
            let sh = {
                let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
                if vh > 0 { vh } else { GetSystemMetrics(SM_CYSCREEN).max(1080) }
            };
            if let Some(m) = PAPER_MODEL.get() {
                if let Ok(mut model) = m.lock() {
                    model.width = sw;
                    model.height = sh;
                }
            }
            let _ = SetWindowPos(hwnd, HWND_BOTTOM, vx, vy, sw, sh, SWP_NOACTIVATE);
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        0x0317 | 0x0318 => {
            // WM_PRINT (0x0317) / WM_PRINTCLIENT (0x0318): render wallpaper directly into caller HDC
            // so `hyprland-win`'s DirectComposition corner mask texture captures the live wallpaper cleanly!
            let hdc = windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut core::ffi::c_void);
            if !hdc.is_invalid() && !hdc.0.is_null() {
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                if let Some(m) = PAPER_MODEL.get() {
                    if let Ok(mut model) = m.lock() {
                        model.width = (rc.right - rc.left).max(1);
                        model.height = (rc.bottom - rc.top).max(1);
                        ui::render_wallpaper(hdc, &model);
                    }
                }
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            if let Some(m) = PAPER_MODEL.get() {
                if let Ok(mut model) = m.lock() {
                    model.width = (rc.right - rc.left).max(1);
                    model.height = (rc.bottom - rc.top).max(1);
                    ui::render_wallpaper(hdc, &model);
                }
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Maps local time of day (`00:00..23:59`) to Apple's 16-stage macOS Mojave Dynamic Desktop
/// (`mojave_dynamic_1.jpeg` .. `mojave_dynamic_16.jpeg`) solar schedule:
/// - `1`:  `05:00 - 06:14` (Early Dawn)
/// - `2`:  `06:15 - 07:14` (Sunrise / First Light)
/// - `3`:  `07:15 - 08:29` (Early Morning)
/// - `4`:  `08:30 - 09:44` (Morning)
/// - `5`:  `09:45 - 11:14` (Late Morning)
/// - `6`:  `11:15 - 12:44` (Solar Noon / High Day)
/// - `7`:  `12:45 - 14:14` (Early Afternoon)
/// - `8`:  `14:15 - 15:29` (Mid Afternoon)
/// - `9`:  `15:30 - 16:29` (Late Afternoon)
/// - `10`: `16:30 - 17:14` (Golden Hour)
/// - `11`: `17:15 - 17:59` (Early Sunset)
/// - `12`: `18:00 - 18:44` (Sunset / Horizon Glow)
/// - `13`: `18:45 - 19:44` (Dusk / Civil Twilight)
/// - `14`: `19:45 - 21:29` (Evening / Blue Hour)
/// - `15`: `21:30 - 00:59` (Night)
/// - `16`: `01:00 - 04:59` (Deep Night / Pre-Dawn)
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

fn resolve_mojave_stage_wallpaper(stage: u8) -> Option<String> {
    let stage = stage.clamp(1, 16);
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

fn resolve_mojave_dynamic_wallpaper() -> Option<String> {
    let ov = OVERRIDE_MOJAVE_STAGE.load(std::sync::atomic::Ordering::SeqCst);
    let stage = if (1..=16).contains(&ov) {
        ov
    } else {
        mojave_stage_for_local_time()
    };
    resolve_mojave_stage_wallpaper(stage)
}

fn user_hyprland_win_config_dir() -> PathBuf {
    let base = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join(".config").join("hyprwin");
    if !dir.exists() {
        let _ = std::fs::create_dir_all(&dir);
    }
    dir
}

fn current_paper_config_hash() -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let dir = user_hyprland_win_config_dir();
    let mut h = DefaultHasher::new();
    if let Ok(b1) = std::fs::read(dir.join("hyprpaper.conf")) {
        b1.hash(&mut h);
    }
    if let Ok(b2) = std::fs::read(dir.join("hyprland.conf")) {
        b2.hash(&mut h);
    }
    h.finish().max(1)
}

fn resolve_wallpaper_value(wp: &str) -> Option<String> {
    let trimmed = wp.trim().trim_matches('"').trim_matches('\'').trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.eq_ignore_ascii_case("dynamic:mojave")
        || trimmed.eq_ignore_ascii_case("mojave")
        || trimmed.eq_ignore_ascii_case("dynamic")
    {
        return resolve_mojave_dynamic_wallpaper();
    }
    let p = std::path::Path::new(trimmed);
    if p.is_file() {
        return Some(trimmed.to_string());
    }
    let cfg_dir = user_hyprland_win_config_dir();
    let rel = trimmed
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
    None
}

fn parse_hyprpaper_config() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    for i in 0..args.len() {
        if args[i] == "--wallpaper" {
            if let Some(p) = args.get(i + 1) {
                if let Some(resolved) = resolve_wallpaper_value(p) {
                    return Some(resolved);
                }
                return Some(p.clone());
            }
        }
    }

    let cfg_dir = user_hyprland_win_config_dir();
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
                        if let Some(resolved) = resolve_wallpaper_value(wp) {
                            return Some(resolved);
                        }
                    }
                }
            }
        }
    }

    if let Some(dyn_wp) = resolve_mojave_dynamic_wallpaper() {
        return Some(dyn_wp);
    }

    let fallback_wallpapers = [
        cfg_dir.join("wallpaper.bmp"),
        cfg_dir.join("wallpaper.png"),
        PathBuf::from(r"c:\Users\mbura\Projects\Other\Hyprland-Win\config\wallpaper.bmp"),
        PathBuf::from("config/wallpaper.bmp"),
    ];
    for fb in fallback_wallpapers {
        if fb.exists() {
            return Some(fb.to_string_lossy().to_string());
        }
    }
    None
}
