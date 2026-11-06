//! Visual layout, styling, JetBrainsMono Nerd Font (`JetBrainsMono NF`) typography,
//! per-pixel SDF anti-aliased rounded rects, `UpdateLayeredWindow` (`ULW_ALPHA`) transparency,
//! active window icons, `atif-1402/minimal-waybar-themes` V2 modules (`custom/omarchy`, `workspaces`,
//! `mpris`, `window`, `group/tray-expander`, `group/ctl`, `clock`), and hover calendar popup for `hyprbar`.

use crate::theme::*;
use crate::widgets::{
    format_v2_window_title, AudioDeviceEntry, BluetoothDeviceEntry, MediaPlaybackStatus,
    NetworkKind, SystemWidgetsSnapshot,
};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use windows::core::{s, PCWSTR};
use windows::Win32::Foundation::{BOOL, COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWMWCP_DONOTROUND,
};
use windows::Win32::Graphics::Gdi::{
    AddFontResourceExW, BitBlt, CombineRgn, CreateCompatibleDC, CreateDIBSection,
    CreateFontIndirectW, CreateFontW, CreateRectRgn, CreateRoundRectRgn, DeleteDC, DeleteObject,
    DrawTextW, GetCurrentObject, GetDC, GetDIBits, GetObjectW, GetTextExtentPoint32W, InvalidateRect,
    ReleaseDC, SelectObject, SetBkMode, SetBrushOrgEx, SetStretchBltMode, SetTextColor, SetWindowRgn,
    StretchBlt, AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, DRAW_TEXT_FORMAT, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, FR_PRIVATE, HALFTONE, HBITMAP, HDC, HFONT, HRGN,
    LOGFONTW, OBJ_BITMAP, RGN_OR, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYEVENTF_KEYUP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DrawIconEx, FindWindowW, GetClassLongPtrW, GetCursorPos, GetIconInfo,
    GetSystemMetrics, GetWindowRect, IsWindowVisible, LoadCursorW, RegisterClassW, SendMessageTimeoutW, SetWindowPos,
    ShowWindow, UpdateLayeredWindow, CS_HREDRAW, CS_VREDRAW, DI_NORMAL, GCLP_HICON, GCLP_HICONSM,
    HICON, ICONINFO, HWND_TOPMOST, IDC_ARROW, ICON_BIG, ICON_SMALL, ICON_SMALL2, SMTO_ABORTIFHUNG,
    SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE,
    ULW_ALPHA, WM_ERASEBKGND, WM_GETICON, WM_PAINT, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

// ----------------------------------------------------------------------------
// Public Types & Hit-Test Contracts (V2 Full Widget Set)
// ----------------------------------------------------------------------------

pub const MONITOR_BORDER_PX: f32 = 9.0;

pub const TRAY_COLLAPSED_WIDTH: f32 = 26.0;
pub const TRAY_TOGGLE_WIDTH: f32 = 22.0;
pub const TRAY_ITEM_STRIDE: f32 = 22.0;
pub const TRAY_LOCK_WIDTH: f32 = 24.0;
pub const TRAY_PADDING_X: f32 = 4.0;
pub const TRAY_GAP_AFTER_TOGGLE: f32 = 2.0;

#[inline]
pub fn compute_expanded_tray_width(tray_item_count: usize) -> f32 {
    (TRAY_PADDING_X + TRAY_TOGGLE_WIDTH + TRAY_GAP_AFTER_TOGGLE)
        + (tray_item_count as f32) * TRAY_ITEM_STRIDE
        + (TRAY_LOCK_WIDTH + TRAY_PADDING_X)
}

#[inline]
pub fn compute_target_tray_width(expanded: bool, tray_item_count: usize) -> f32 {
    if expanded {
        compute_expanded_tray_width(tray_item_count)
    } else {
        TRAY_COLLAPSED_WIDTH
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarClickTarget {
    OmarchyLauncher,
    Workspace(u32),
    WorkspacesContainer,
    MprisPlayPause,
    MprisBody,
    FocusedWindow,
    TrayExpanderToggle,
    TrayApp(usize),
    TrayShot,
    TrayLock,
    TrayTiling,
    CtlIdleInhibit,
    CtlDndSilence,
    CtlBluetooth,
    CtlNetwork,
    CtlPulseAudio,
    CtlCpu,
    CtlBattery,
    Clock,
    SystemMenu,
    ToggleLayout,
    ToggleTiling,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePopover {
    None,
    Calendar { year: i32, month: u32 },
    Volume,
    Bluetooth,
    Network,
    SystemMenu,
    AboutHyprlandWin,
}

#[derive(Debug, Clone)]
pub struct BarRenderModel {
    pub bar_width: i32,
    pub bar_height: i32,
    pub active_workspace: u32,
    pub occupied_workspaces: [u16; 10],
    pub workspace_hwnds: [Vec<u64>; 10],
    pub layout_mode: String,
    pub tiling_active: bool,
    pub active_class: String,
    pub active_title: String,
    pub active_floating: bool,
    pub active_fullscreen: bool,
    pub cpu_usage_pct: u32,
    pub ram_used_gb: f32,
    pub ram_total_gb: f32,
    pub clock_text: String,
    pub clock_alt_text: String,
    pub clock_alt_mode: bool,
    pub tray_expanded: bool,
    pub active_popover: ActivePopover,
    pub cur_year: i32,
    pub cur_month: u32,
    pub cur_day: u32,
    pub cur_weekday: u32,
    pub widgets: SystemWidgetsSnapshot,

    // Interactive Hit-Test Rectangles (x, y, w, h)
    pub omarchy_rect: (i32, i32, i32, i32),
    pub ws_container_rect: (i32, i32, i32, i32),
    pub ws_pill_rects: [(i32, i32, i32, i32); 10],
    pub mpris_rect: (i32, i32, i32, i32),
    pub window_rect: (i32, i32, i32, i32),
    pub tray_toggle_rect: (i32, i32, i32, i32),
    pub tray_app_rects: Vec<(i32, i32, i32, i32)>,
    pub tray_shot_rect: (i32, i32, i32, i32),
    pub tray_lock_rect: (i32, i32, i32, i32),
    pub tray_tiling_rect: (i32, i32, i32, i32),
    pub ctl_idle_rect: (i32, i32, i32, i32),
    pub ctl_dnd_rect: (i32, i32, i32, i32),
    pub ctl_bt_rect: (i32, i32, i32, i32),
    pub ctl_net_rect: (i32, i32, i32, i32),
    pub ctl_vol_rect: (i32, i32, i32, i32),
    pub ctl_cpu_rect: (i32, i32, i32, i32),
    pub ctl_bat_rect: (i32, i32, i32, i32),
    pub layout_pill_rect: (i32, i32, i32, i32),
    pub tiling_pill_rect: (i32, i32, i32, i32),
    pub clock_rect: (i32, i32, i32, i32),
    pub menu_rect: (i32, i32, i32, i32),

    pub hover_pos: Option<(i32, i32)>,
    pub anim_pill_widths: [f32; 10],
    pub anim_highlight_x: f32,
    pub anim_highlight_w: f32,
    pub anim_hover_t: [f32; 10],
    pub anim_tray_width: f32,
    pub anim_dashboard_morph: f32,
    pub anim_marquee_phase: f32,
    pub target_window_w: f32,
    pub anim_window_w: f32,
    pub target_window_h: f32,
    pub anim_window_h: f32,
    pub theme: HyprBarTheme,
}

impl BarRenderModel {
    pub fn default_with_width(width: i32) -> Self {
        let theme = HyprBarTheme::load();
        Self {
            bar_width: width.max(640),
            bar_height: theme.bar_height,
            active_workspace: 1,
            occupied_workspaces: [0; 10],
            workspace_hwnds: Default::default(),
            layout_mode: "DWINDLE".to_string(),
            tiling_active: true,
            active_class: String::new(),
            active_title: String::new(),
            active_floating: false,
            active_fullscreen: false,
            cpu_usage_pct: 0,
            ram_used_gb: 0.0,
            ram_total_gb: 16.0,
            clock_text: "12:00".to_string(),
            clock_alt_text: "01 November 2026 | Sunday".to_string(),
            clock_alt_mode: false,
            tray_expanded: false,
            active_popover: ActivePopover::None,
            cur_year: 2026,
            cur_month: 11,
            cur_day: 1,
            cur_weekday: 0,
            widgets: SystemWidgetsSnapshot::default(),

            omarchy_rect: (0, 0, 0, 0),
            ws_container_rect: (0, 0, 0, 0),
            ws_pill_rects: [(0, 0, 0, 0); 10],
            mpris_rect: (0, 0, 0, 0),
            window_rect: (0, 0, 0, 0),
            tray_toggle_rect: (0, 0, 0, 0),
            tray_app_rects: Vec::new(),
            tray_shot_rect: (0, 0, 0, 0),
            tray_lock_rect: (0, 0, 0, 0),
            tray_tiling_rect: (0, 0, 0, 0),
            ctl_idle_rect: (0, 0, 0, 0),
            ctl_dnd_rect: (0, 0, 0, 0),
            ctl_bt_rect: (0, 0, 0, 0),
            ctl_net_rect: (0, 0, 0, 0),
            ctl_vol_rect: (0, 0, 0, 0),
            ctl_cpu_rect: (0, 0, 0, 0),
            ctl_bat_rect: (0, 0, 0, 0),
            layout_pill_rect: (0, 0, 0, 0),
            tiling_pill_rect: (0, 0, 0, 0),
            clock_rect: (0, 0, 0, 0),
            menu_rect: (0, 0, 0, 0),

            hover_pos: None,
            anim_pill_widths: [
                26.0,
                theme.ws_empty_dot_width,
                theme.ws_empty_dot_width,
                theme.ws_empty_dot_width,
                theme.ws_empty_dot_width,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
            ],
            anim_highlight_x: 54.0,
            anim_highlight_w: 26.0,
            anim_hover_t: [0.0; 10],
            anim_tray_width: 26.0,
            anim_dashboard_morph: 0.0,
            anim_marquee_phase: 0.0,
            target_window_w: 120.0,
            anim_window_w: 120.0,
            target_window_h: 20.0,
            anim_window_h: 20.0,
            theme,
        }
    }
}

#[inline]
pub fn pt_in_rect(px: i32, py: i32, (rx, ry, rw, rh): (i32, i32, i32, i32)) -> bool {
    rw > 0 && rh > 0 && px >= rx && px < rx + rw && py >= ry && py < ry + rh
}

pub fn hit_test_bar(model: &BarRenderModel, x: i32, y: i32) -> Option<BarClickTarget> {
    if pt_in_rect(x, y, model.omarchy_rect) {
        return Some(BarClickTarget::OmarchyLauncher);
    }
    for (i, &rect) in model.ws_pill_rects.iter().enumerate() {
        if pt_in_rect(x, y, rect) {
            return Some(BarClickTarget::Workspace((i + 1) as u32));
        }
    }
    if pt_in_rect(x, y, model.ws_container_rect) {
        return Some(BarClickTarget::WorkspacesContainer);
    }
    if pt_in_rect(x, y, model.mpris_rect) {
        if x < model.mpris_rect.0 + 28 {
            return Some(BarClickTarget::MprisPlayPause);
        }
        return Some(BarClickTarget::MprisBody);
    }
    if pt_in_rect(x, y, model.window_rect) {
        return Some(BarClickTarget::FocusedWindow);
    }
    if pt_in_rect(x, y, model.tray_toggle_rect) {
        return Some(BarClickTarget::TrayExpanderToggle);
    }
    if model.tray_expanded {
        for (idx, &r) in model.tray_app_rects.iter().enumerate() {
            if pt_in_rect(x, y, r) {
                return Some(BarClickTarget::TrayApp(idx));
            }
        }
        if pt_in_rect(x, y, model.tray_lock_rect) {
            return Some(BarClickTarget::TrayLock);
        }
    }
    if pt_in_rect(x, y, model.ctl_idle_rect) {
        return Some(BarClickTarget::CtlIdleInhibit);
    }
    if pt_in_rect(x, y, model.ctl_dnd_rect) {
        return Some(BarClickTarget::CtlDndSilence);
    }
    if pt_in_rect(x, y, model.ctl_bt_rect) {
        return Some(BarClickTarget::CtlBluetooth);
    }
    if pt_in_rect(x, y, model.ctl_net_rect) {
        return Some(BarClickTarget::CtlNetwork);
    }
    if pt_in_rect(x, y, model.ctl_vol_rect) {
        return Some(BarClickTarget::CtlPulseAudio);
    }
    if pt_in_rect(x, y, model.ctl_cpu_rect) {
        return Some(BarClickTarget::CtlCpu);
    }
    if pt_in_rect(x, y, model.ctl_bat_rect) {
        return Some(BarClickTarget::CtlBattery);
    }
    if pt_in_rect(x, y, model.clock_rect) {
        return Some(BarClickTarget::Clock);
    }
    if pt_in_rect(x, y, model.menu_rect) {
        return Some(BarClickTarget::SystemMenu);
    }
    if pt_in_rect(x, y, model.layout_pill_rect) {
        return Some(BarClickTarget::ToggleLayout);
    }
    if pt_in_rect(x, y, model.tiling_pill_rect) {
        return Some(BarClickTarget::ToggleTiling);
    }
    None
}

pub fn is_point_in_clock(model: &BarRenderModel, x: i32, y: i32) -> bool {
    let (cx, _cy, cw, _ch) = model.clock_rect;
    cw > 0 && x >= cx - 6 && x <= cx + cw + 6 && y >= 0 && y <= model.bar_height + 4
}

pub fn is_point_in_window_pill(model: &BarRenderModel, x: i32, y: i32) -> bool {
    pt_in_rect(x, y, model.window_rect)
}

// ----------------------------------------------------------------------------
// JetBrainsMono Nerd Font (`JetBrainsMono NF` / `JetBrainsMono NFP`) Loader
// ----------------------------------------------------------------------------

static FONTS_LOADED: OnceLock<()> = OnceLock::new();
static ICON_CACHE: OnceLock<Mutex<HashMap<u64, Option<usize>>>> = OnceLock::new();

pub unsafe fn ensure_private_fonts_loaded() {
    FONTS_LOADED.get_or_init(|| {
        let userprofile = std::env::var("USERPROFILE").unwrap_or_default();
        let localappdata = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let mut font_dirs = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                font_dirs.push(dir.join("assets\\fonts").to_string_lossy().into_owned());
                if let Some(p1) = dir.parent() {
                    if let Some(p2) = p1.parent() {
                        font_dirs.push(p2.join("assets\\fonts").to_string_lossy().into_owned());
                    }
                }
            }
        }
        font_dirs.push("assets\\fonts".to_string());
        font_dirs.push("C:\\Users\\mbura\\Projects\\Other\\hyprland-win\\assets\\fonts".to_string());
        font_dirs.push("C:\\Users\\mbura\\Projects\\Other\\hyprland-win\\assets\\fonts".to_string());
        font_dirs.push(format!("{localappdata}\\Microsoft\\Windows\\Fonts"));
        font_dirs.push(format!("{userprofile}\\Downloads\\JetBrainsMono"));
        font_dirs.push("C:\\Windows\\Fonts".to_string());

        let font_files = [
            "SF-Pro-Display-Regular.otf",
            "SF-Pro-Display-Medium.otf",
            "SF-Pro-Display-Semibold.otf",
            "SF-Pro-Display-Bold.otf",
            "SF-Pro-Text-Regular.otf",
            "SF-Pro-Text-Medium.otf",
            "SF-Pro-Text-Semibold.otf",
            "JetBrainsMonoNerdFont-Regular.ttf",
            "JetBrainsMonoNerdFont-Medium.ttf",
            "JetBrainsMonoNerdFont-SemiBold.ttf",
            "JetBrainsMonoNerdFont-Bold.ttf",
            "JetBrainsMonoNerdFont-ExtraBold.ttf",
            "JetBrainsMonoNerdFontPropo-Regular.ttf",
            "JetBrainsMonoNerdFontPropo-Medium.ttf",
            "JetBrainsMonoNerdFontPropo-SemiBold.ttf",
            "JetBrainsMonoNerdFontPropo-Bold.ttf",
            "JetBrainsMonoNerdFontPropo-ExtraBold.ttf",
            "JetBrainsMonoNerdFontMono-Regular.ttf",
            "JetBrainsMonoNerdFontMono-Bold.ttf",
        ];

        for dir in &font_dirs {
            for file in &font_files {
                let full_path = format!("{dir}\\{file}");
                if std::path::Path::new(&full_path).is_file() {
                    let wide: Vec<u16> = full_path.encode_utf16().chain(Some(0)).collect();
                    let _ = AddFontResourceExW(PCWSTR(wide.as_ptr()), FR_PRIVATE, None);
                }
            }
        }
    });
}

static EXE_ICON_CACHE: OnceLock<Mutex<HashMap<String, Option<usize>>>> = OnceLock::new();

#[link(name = "shell32")]
extern "system" {
    fn ExtractIconExW(
        lpszfile: *const u16,
        niconindex: i32,
        phiconlarge: *mut HICON,
        phiconsmall: *mut HICON,
        nicons: u32,
    ) -> u32;
}

pub unsafe fn extract_icon_from_exe_path(exe_path: &str) -> Option<usize> {
    if exe_path.is_empty() {
        return None;
    }
    let key = exe_path.to_ascii_lowercase();
    let exe_cache = EXE_ICON_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = exe_cache.try_lock() {
        if let Some(&cached) = guard.get(&key) {
            return cached;
        }
    }

    // If this is Steam's CEF helper (`...\Steam\bin\cef\cef.win64\steamwebhelper.exe`),
    // prefer the main `steam.exe` icon if present
    let mut candidate_paths = vec![exe_path.to_string()];
    if key.ends_with("steamwebhelper.exe") {
        if let Some(idx) = key.find("\\bin\\cef\\") {
            let root = &exe_path[..idx];
            candidate_paths.insert(0, format!("{root}\\steam.exe"));
        }
    }

    let mut resolved: usize = 0;
    for path in &candidate_paths {
        let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let mut h_large = HICON::default();
        let mut h_small = HICON::default();
        let count = ExtractIconExW(wide.as_ptr(), 0, &mut h_large, &mut h_small, 1);
        if count > 0 {
            if !h_small.is_invalid() && !h_small.0.is_null() {
                resolved = h_small.0 as usize;
                if !h_large.is_invalid() && !h_large.0.is_null() && h_large.0 != h_small.0 {
                    let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(h_large);
                }
                break;
            } else if !h_large.is_invalid() && !h_large.0.is_null() {
                resolved = h_large.0 as usize;
                break;
            }
        }
    }

    let res_opt = if resolved != 0 { Some(resolved) } else { None };
    if let Ok(mut guard) = exe_cache.try_lock() {
        guard.insert(key, res_opt);
    }
    res_opt
}

unsafe fn query_hwnd_exe_path(hwnd: HWND) -> Option<String> {
    let mut pid: u32 = 0;
    windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == 0 {
        return None;
    }
    let hproc = windows::Win32::System::Threading::OpenProcess(
        windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION,
        false,
        pid,
    )
    .ok()?;
    let mut buf = [0u16; 512];
    let mut len = buf.len() as u32;
    let ok = windows::Win32::System::Threading::QueryFullProcessImageNameW(
        hproc,
        windows::Win32::System::Threading::PROCESS_NAME_WIN32,
        windows::core::PWSTR(buf.as_mut_ptr()),
        &mut len,
    )
    .is_ok();
    let _ = windows::Win32::Foundation::CloseHandle(hproc);
    if ok && len > 0 {
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    } else {
        None
    }
}

pub unsafe fn get_tray_item_hicon(item: &crate::widgets::TrayAppItem) -> Option<HICON> {
    if !item.exe_path.is_empty() {
        if let Some(ptr) = extract_icon_from_exe_path(&item.exe_path) {
            return Some(HICON(ptr as *mut core::ffi::c_void));
        }
    }
    get_window_hicon(item.hwnd)
}

pub unsafe fn get_window_hicon(hwnd_val: u64) -> Option<HICON> {
    if hwnd_val == 0 {
        return None;
    }
    let cache = ICON_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.try_lock() {
        if let Some(&cached) = guard.get(&hwnd_val) {
            return cached.map(|ptr| HICON(ptr as *mut core::ffi::c_void));
        }
    }

    let def_icon_ptr = windows::Win32::UI::WindowsAndMessaging::LoadIconW(
        None,
        windows::Win32::UI::WindowsAndMessaging::IDI_APPLICATION,
    )
    .map(|ic| ic.0 as usize)
    .unwrap_or(0);

    // 1. Fast non-blocking in-memory class icon lookup (ignoring generic IDI_APPLICATION)
    let h = windows::Win32::Foundation::HWND(hwnd_val as *mut core::ffi::c_void);
    let mut fast_icon = GetClassLongPtrW(h, GCLP_HICONSM);
    if fast_icon == 0 || fast_icon == def_icon_ptr {
        fast_icon = GetClassLongPtrW(h, GCLP_HICON);
    }
    if fast_icon == def_icon_ptr {
        fast_icon = 0;
    }

    // 2. Direct PE resource icon extraction from the process executable (`ExtractIconExW`, < 0.2ms, 0 IPC/COM)
    if fast_icon == 0 {
        if let Some(exe_path) = query_hwnd_exe_path(h) {
            if let Some(exe_icon) = extract_icon_from_exe_path(&exe_path) {
                fast_icon = exe_icon;
            }
        }
    }

    if fast_icon == 0 && def_icon_ptr != 0 {
        fast_icon = def_icon_ptr;
    }

    let initial_opt = if fast_icon != 0 { Some(fast_icon) } else { None };
    if let Ok(mut guard) = cache.try_lock() {
        guard.insert(hwnd_val, initial_opt);
    }

    // 3. Also query per-window `WM_GETICON` asynchronously on a background thread
    std::thread::spawn(move || unsafe {
        let h = windows::Win32::Foundation::HWND(hwnd_val as *mut core::ffi::c_void);
        let mut icon_result: usize = 0;

        let _ = SendMessageTimeoutW(
            h,
            WM_GETICON,
            WPARAM(ICON_SMALL2 as usize),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            20,
            Some(&mut icon_result),
        );
        if icon_result == 0 || icon_result == def_icon_ptr {
            icon_result = 0;
            let _ = SendMessageTimeoutW(
                h,
                WM_GETICON,
                WPARAM(ICON_SMALL as usize),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                20,
                Some(&mut icon_result),
            );
        }
        if icon_result == 0 || icon_result == def_icon_ptr {
            icon_result = 0;
            let _ = SendMessageTimeoutW(
                h,
                WM_GETICON,
                WPARAM(ICON_BIG as usize),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                20,
                Some(&mut icon_result),
            );
        }
        if icon_result != 0 && icon_result != def_icon_ptr {
            if let Some(c) = ICON_CACHE.get() {
                if let Ok(mut guard) = c.lock() {
                    guard.insert(hwnd_val, Some(icon_result));
                }
            }
        }
    });

    initial_opt.map(|ptr| HICON(ptr as *mut core::ffi::c_void))
}

// ----------------------------------------------------------------------------
// Signed Distance Field (SDF) Sub-Pixel Anti-Aliased Rounded Rect (Layered Alpha)
// ----------------------------------------------------------------------------

#[inline]
fn alpha_blend_straight_bgra(bg: u32, fg_rgb: COLORREF, alpha_u8: u8) -> u32 {
    if alpha_u8 == 0 {
        return bg;
    }
    let fg_r = (fg_rgb.0 & 0xFF) as u32;
    let fg_g = ((fg_rgb.0 >> 8) & 0xFF) as u32;
    let fg_b = ((fg_rgb.0 >> 16) & 0xFF) as u32;
    let fg_a = alpha_u8 as u32;

    let bg_a = (bg >> 24) & 0xFF;
    if bg_a == 0 {
        return (fg_a << 24) | (fg_r << 16) | (fg_g << 8) | fg_b;
    }

    let bg_r = (bg >> 16) & 0xFF;
    let bg_g = (bg >> 8) & 0xFF;
    let bg_b = bg & 0xFF;

    let inv_a = 255 - fg_a;
    let out_a = (fg_a + (bg_a * inv_a) / 255).min(255);
    let out_r = (fg_r * fg_a + bg_r * inv_a) / 255;
    let out_g = (fg_g * fg_a + bg_g * inv_a) / 255;
    let out_b = (fg_b * fg_a + bg_b * inv_a) / 255;

    (out_a << 24) | (out_r << 16) | (out_g << 8) | out_b
}

#[inline]
pub fn lerp_color(c1: COLORREF, c2: COLORREF, t: f32) -> COLORREF {
    let t = t.clamp(0.0, 1.0);
    let r1 = (c1.0 & 0xFF) as f32;
    let g1 = ((c1.0 >> 8) & 0xFF) as f32;
    let b1 = ((c1.0 >> 16) & 0xFF) as f32;

    let r2 = (c2.0 & 0xFF) as f32;
    let g2 = ((c2.0 >> 8) & 0xFF) as f32;
    let b2 = ((c2.0 >> 16) & 0xFF) as f32;

    let r = (r1 + (r2 - r1) * t).round() as u8;
    let g = (g1 + (g2 - g1) * t).round() as u8;
    let b = (b1 + (b2 - b1) * t).round() as u8;

    rgb(r, g, b)
}

pub fn fill_aa_round_rect_f32(
    pixels: &mut [u32],
    stride: usize,
    max_w: i32,
    max_h: i32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    fill_rgb: COLORREF,
    border_rgb: COLORREF,
    border_thickness: f32,
    opacity: f32,
) {
    if w <= 0.0 || h <= 0.0 || opacity <= 0.001 {
        return;
    }

    let half_w = w * 0.5;
    let half_h = h * 0.5;
    let center_x = x + half_w;
    let center_y = y + half_h;
    let r = radius.min(half_w).min(half_h).max(0.0);

    let min_x = ((x - 1.0).floor() as i32).max(0);
    let max_x = ((x + w + 1.0).ceil() as i32).min(max_w);
    let min_y = ((y - 1.0).floor() as i32).max(0);
    let max_y = ((y + h + 1.0).ceil() as i32).min(max_h);
    let inner_thresh = -(border_thickness.max(0.0) + 0.5);
    let solid_fill_a = (opacity * 255.0).clamp(0.0, 255.0) as u8;

    for py in min_y..max_y {
        let row_offset = (py as usize) * stride;
        let py_f = py as f32 + 0.5;
        let qy = (py_f - center_y).abs() - half_h + r;

        for px in min_x..max_x {
            let px_f = px as f32 + 0.5;
            let qx = (px_f - center_x).abs() - half_w + r;

            let idx = row_offset + (px as usize);
            if idx >= pixels.len() {
                continue;
            }

            if qx <= inner_thresh && qy <= inner_thresh {
                pixels[idx] = alpha_blend_straight_bgra(pixels[idx], fill_rgb, solid_fill_a);
                continue;
            }

            let qx_clamped = qx.max(0.0);
            let qy_clamped = qy.max(0.0);
            let dist =
                qx.max(qy).min(0.0) + (qx_clamped * qx_clamped + qy_clamped * qy_clamped).sqrt() - r;

            let outer_cov = (0.5 - dist).clamp(0.0, 1.0);
            if outer_cov <= 0.001 {
                continue;
            }

            let bg = pixels[idx];

            let final_pixel = if border_thickness > 0.0 {
                let inner_dist = dist + border_thickness;
                let inner_cov = (0.5 - inner_dist).clamp(0.0, 1.0);
                let border_cov = (outer_cov - inner_cov).max(0.0);

                let fill_a = (inner_cov * opacity * 255.0).clamp(0.0, 255.0) as u8;
                let border_a = (border_cov * opacity * 255.0).clamp(0.0, 255.0) as u8;

                let with_border = alpha_blend_straight_bgra(bg, border_rgb, border_a);
                alpha_blend_straight_bgra(with_border, fill_rgb, fill_a)
            } else {
                let a = (outer_cov * opacity * 255.0).clamp(0.0, 255.0) as u8;
                alpha_blend_straight_bgra(bg, fill_rgb, a)
            };

            pixels[idx] = final_pixel;
        }
    }
}

pub fn fill_aa_round_rect(
    pixels: &mut [u32],
    stride: usize,
    max_w: i32,
    max_h: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    radius: f32,
    fill_rgb: COLORREF,
    border_rgb: COLORREF,
    border_thickness: f32,
    opacity: f32,
) {
    fill_aa_round_rect_f32(
        pixels,
        stride,
        max_w,
        max_h,
        x as f32,
        y as f32,
        w as f32,
        h as f32,
        radius,
        fill_rgb,
        border_rgb,
        border_thickness,
        opacity,
    );
}

// ----------------------------------------------------------------------------
// Signed Distance Field (SDF) Top-Edge Notch Renderer
// Flush with screen top edge (y = 0.0), with concave top fillet "ears" and
// rounded bottom corners. Subtle 1px border along active boundaries only.
// ----------------------------------------------------------------------------

#[inline]
fn compute_notch_sdf(
    px_f: f32,
    py_f: f32,
    x0: f32,
    x1: f32,
    h: f32,
    r_corner: f32,
    r_ear: f32,
    has_left_ear: bool,
    has_right_ear: bool,
) -> f32 {
    let y_top = MONITOR_BORDER_PX;
    let max_r = ((h - y_top).max(2.0)) * 0.5;
    let r_ear = r_ear.min(max_r);
    let r_corner = r_corner.min(max_r);
    if py_f <= y_top {
        return py_f - h;
    }

    if px_f < x0 {
        if !has_left_ear {
            return x0 - px_f;
        }
        if py_f < y_top + r_ear {
            let dx = px_f - (x0 - r_ear);
            let dy = (y_top + r_ear) - py_f;
            if dx < 0.0 {
                py_f - y_top
            } else {
                r_ear - (dx * dx + dy * dy).sqrt()
            }
        } else if py_f > h - r_corner {
            let dx = x0 + r_corner - px_f;
            let dy = py_f - (h - r_corner);
            (dx * dx + dy * dy).sqrt() - r_corner
        } else {
            x0 - px_f
        }
    } else if px_f > x1 {
        if !has_right_ear {
            return px_f - x1;
        }
        if py_f < y_top + r_ear {
            let dx = (x1 + r_ear) - px_f;
            let dy = (y_top + r_ear) - py_f;
            if dx < 0.0 {
                py_f - y_top
            } else {
                r_ear - (dx * dx + dy * dy).sqrt()
            }
        } else if py_f > h - r_corner {
            let dx = px_f - (x1 - r_corner);
            let dy = py_f - (h - r_corner);
            (dx * dx + dy * dy).sqrt() - r_corner
        } else {
            px_f - x1
        }
    } else if py_f > h {
        if has_left_ear && px_f < x0 + r_corner {
            let dx = x0 + r_corner - px_f;
            let dy = py_f - (h - r_corner);
            (dx * dx + dy * dy).sqrt() - r_corner
        } else if has_right_ear && px_f > x1 - r_corner {
            let dx = px_f - (x1 - r_corner);
            let dy = py_f - (h - r_corner);
            (dx * dx + dy * dy).sqrt() - r_corner
        } else {
            py_f - h
        }
    } else {
        // Point is inside the bounding rectangle [x0, x1] x [y_top, h]
        if has_left_ear && px_f < x0 + r_corner && py_f > h - r_corner {
            let dx = x0 + r_corner - px_f;
            let dy = py_f - (h - r_corner);
            (dx * dx + dy * dy).sqrt() - r_corner
        } else if has_right_ear && px_f > x1 - r_corner && py_f > h - r_corner {
            let dx = px_f - (x1 - r_corner);
            let dy = py_f - (h - r_corner);
            (dx * dx + dy * dy).sqrt() - r_corner
        } else {
            let mut dist = py_f - h;
            if has_left_ear {
                let d_left = if py_f < y_top + r_ear {
                    let dx = px_f - (x0 - r_ear);
                    let dy = (y_top + r_ear) - py_f;
                    r_ear - (dx * dx + dy * dy).sqrt()
                } else {
                    x0 - px_f
                };
                dist = dist.max(d_left);
            }
            if has_right_ear {
                let d_right = if py_f < y_top + r_ear {
                    let dx = (x1 + r_ear) - px_f;
                    let dy = (y_top + r_ear) - py_f;
                    r_ear - (dx * dx + dy * dy).sqrt()
                } else {
                    px_f - x1
                };
                dist = dist.max(d_right);
            }
            dist
        }
    }
}

pub fn draw_aa_notch(
    pixels: &mut [u32],
    stride: usize,
    max_w: i32,
    max_h: i32,
    x: f32,
    w: f32,
    h: f32,
    r_corner: f32,
    r_ear: f32,
    has_left_ear: bool,
    has_right_ear: bool,
    fill_rgb: COLORREF,
    _border_rgb: COLORREF,
    fill_opacity: f32,
    _border_opacity: f32,
) {
    if w <= 0.0 || h <= 0.0 || max_w <= 0 || max_h <= 0 {
        return;
    }

    let x0 = x;
    let x1 = x + w;

    let r_corner = r_corner.min(w * 0.48).min(h * 0.48).max(1.0);
    let r_ear = r_ear.min(h * 0.48).max(1.0);

    let min_x = if has_left_ear {
        ((x0 - r_ear - 1.0).floor() as i32).clamp(0, max_w)
    } else {
        (x0.floor() as i32).clamp(0, max_w)
    };

    let max_x = if has_right_ear {
        ((x1 + r_ear + 2.0).ceil() as i32).clamp(0, max_w)
    } else {
        ((x1 + 1.0).ceil() as i32).clamp(0, max_w)
    };

    let min_y = 0_i32;
    let max_y = ((h + 2.0).ceil() as i32).clamp(0, max_h);

    for py in min_y..max_y {
        let row_offset = (py as usize) * stride;
        let py_f = py as f32 + 0.5;

        for px in min_x..max_x {
            let px_f = px as f32 + 0.5;
            let idx = row_offset + (px as usize);
            if idx >= pixels.len() {
                continue;
            }

            let dist = compute_notch_sdf(
                px_f, py_f, x0, x1, h, r_corner, r_ear, has_left_ear, has_right_ear,
            );

            let outer_cov = (0.5 - dist).clamp(0.0, 1.0);
            if outer_cov <= 0.001 {
                continue;
            }

            let a = (outer_cov * fill_opacity * 255.0).round().clamp(0.0, 255.0) as u8;
            pixels[idx] = alpha_blend_straight_bgra(pixels[idx], fill_rgb, a);
        }
    }
}

// ----------------------------------------------------------------------------
// Internal Typography Helpers (Apple SF Pro & Nerd Font Selection)
// ----------------------------------------------------------------------------

#[inline]
pub fn is_nerd_font_glyph(c: char) -> bool {
    let u = c as u32;
    (0xE000..=0xF8FF).contains(&u) || (0xF0000..=0xFFFFD).contains(&u)
}

fn split_icon_chunks(text: &str) -> Vec<(bool, String)> {
    let mut chunks: Vec<(bool, String)> = Vec::new();
    let mut cur_icon = false;
    let mut cur_str = String::new();

    for c in text.chars() {
        let is_icon = is_nerd_font_glyph(c);
        if cur_str.is_empty() {
            cur_icon = is_icon;
            cur_str.push(c);
        } else if is_icon == cur_icon {
            cur_str.push(c);
        } else {
            chunks.push((cur_icon, cur_str));
            cur_icon = is_icon;
            cur_str = c.to_string();
        }
    }
    if !cur_str.is_empty() {
        chunks.push((cur_icon, cur_str));
    }
    chunks
}

unsafe fn create_font(name: PCWSTR, height: i32, weight: i32) -> HFONT {
    CreateFontW(
        height,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        1, // DEFAULT_CHARSET
        7, // OUT_TT_ONLY_PRECIS (force TrueType/OpenType outline vector rendering)
        0,
        ANTIALIASED_QUALITY.0 as u32,
        0,
        name,
    )
}

pub unsafe fn create_icon_font(height: i32, weight: i32) -> HFONT {
    let icon_candidates = [
        "JetBrainsMono NFP",
        "JetBrainsMono NF",
        "JetBrainsMono NFM",
        "Segoe Fluent Icons",
        "Segoe MDL2 Assets",
    ];
    for name in icon_candidates {
        let name_u16: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let hfont = create_font(PCWSTR(name_u16.as_ptr()), height, weight);
        if !hfont.is_invalid() && !hfont.0.is_null() {
            return hfont;
        }
    }
    create_font(PCWSTR::null(), height, weight)
}

pub unsafe fn create_apple_font(height: i32, weight: i32) -> HFONT {
    let primary = if height.abs() >= 18 { "Segoe UI Variable Display" } else { "Segoe UI Variable Text" };
    let candidates = [
        primary,
        "Segoe UI Variable Text",
        "Segoe UI Variable Display",
        "Segoe UI",
        "SF Pro Text",
        "SF Pro Display",
        "Arial",
    ];
    for name in candidates {
        let name_u16: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let hfont = create_font(PCWSTR(name_u16.as_ptr()), height, weight);
        if !hfont.is_invalid() && !hfont.0.is_null() {
            return hfont;
        }
    }
    create_font(PCWSTR::null(), height, weight)
}

unsafe fn create_font_with_fallback(primary_name: &str, height: i32, weight: i32) -> HFONT {
    if primary_name.eq_ignore_ascii_case("JetBrainsMono NF")
        || primary_name.eq_ignore_ascii_case("JetBrainsMono Nerd Font")
        || primary_name.eq_ignore_ascii_case("JetBrains Mono")
    {
        create_icon_font(height, weight)
    } else if primary_name.eq_ignore_ascii_case("applefont")
        || primary_name.eq_ignore_ascii_case("apple font")
        || primary_name.eq_ignore_ascii_case("SF Pro")
        || primary_name.eq_ignore_ascii_case("San Francisco")
        || primary_name.is_empty()
    {
        create_apple_font(height, weight)
    } else {
        let candidates = [
            primary_name,
            if height.abs() >= 18 { "SF Pro Display" } else { "SF Pro Text" },
            "SF Pro Text",
            "SF Pro Display",
            "Segoe UI Variable Text",
            "Segoe UI",
        ];
        for name in candidates {
            let name_u16: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            let hfont = create_font(PCWSTR(name_u16.as_ptr()), height, weight);
            if !hfont.is_invalid() && !hfont.0.is_null() {
                return hfont;
            }
        }
        create_apple_font(height, weight)
    }
}

// ----------------------------------------------------------------------------
// Native Refresh-Rate Smooth Workspace Pill & Highlight Interpolator
// ----------------------------------------------------------------------------

pub fn step_bar_animations(model: &mut BarRenderModel, dt_sec: f32) -> bool {
    let mut any_changed = false;

    if model.theme.check_and_reload() {
        model.bar_height = model.theme.bar_height;
        any_changed = true;
    }

    let highest_used = (1..=10)
        .filter(|&ws| {
            let idx = (ws - 1) as usize;
            model.occupied_workspaces[idx] > 0
                || !model.workspace_hwnds[idx].is_empty()
                || ws == model.active_workspace
        })
        .max()
        .unwrap_or(1)
        .max(5);

    let mut target_widths = [0.0_f32; 10];
    for idx in 0..10 {
        let ws_id = (idx + 1) as u32;
        let is_active = ws_id == model.active_workspace;
        let hwnds_len = model.workspace_hwnds[idx].len();
        let is_occupied = model.occupied_workspaces[idx] > 0 || hwnds_len > 0;
        let is_shown = (idx + 1) <= (highest_used as usize) || is_active || is_occupied;

        let mut icon_count = 0;
        for &hwnd_val in model.workspace_hwnds[idx].iter() {
            if unsafe { get_window_hicon(hwnd_val).is_some() } {
                icon_count += 1;
                if icon_count >= 4 {
                    break;
                }
            }
        }

        target_widths[idx] = if !is_shown {
            0.0
        } else if is_active {
            if icon_count > 0 {
                ((icon_count as f32 * 23.0) + 14.0).max(model.theme.ws_active_width)
            } else if is_occupied {
                model.theme.ws_active_width
            } else {
                32.0
            }
        } else if is_occupied {
            if icon_count > 0 {
                (icon_count as f32 * 23.0) + 14.0
            } else {
                model.theme.ws_occupied_width
            }
        } else {
            model.theme.ws_empty_dot_width
        };
    }

    // Smooth 144Hz+ critically-damped exponential interpolation (~180ms transition)
    let spd = model.theme.ws_anim_speed.clamp(8.0, 18.0);
    let alpha = (1.0 - (-dt_sec * spd).exp()).clamp(0.01, 1.0);

    for idx in 0..10 {
        let diff = (target_widths[idx] - model.anim_pill_widths[idx]).abs();
        if diff > 0.02 {
            any_changed = true;
        }
        model.anim_pill_widths[idx] += (target_widths[idx] - model.anim_pill_widths[idx]) * alpha;
    }

    // ws_container_x(49) + ws_inner_pad(5.0) = 54.0
    let start_x = 54.0_f32;
    let pill_gap = model.theme.pill_gap;

    let mut cur_x = start_x;
    let mut target_hl_x = cur_x;
    let mut target_hl_w = 16.0;

    for idx in 0..10 {
        let w = model.anim_pill_widths[idx];
        if (idx + 1) as u32 == model.active_workspace {
            target_hl_x = cur_x;
            target_hl_w = w.max(10.0);
        }
        if w > 0.5 {
            cur_x += w + pill_gap;
        }
    }

    if model.anim_highlight_x == 0.0 {
        model.anim_highlight_x = target_hl_x;
        model.anim_highlight_w = target_hl_w;
    }

    let diff_hl_x = (target_hl_x - model.anim_highlight_x).abs();
    let diff_hl_w = (target_hl_w - model.anim_highlight_w).abs();
    if diff_hl_x > 0.02 || diff_hl_w > 0.02 {
        any_changed = true;
    }
    model.anim_highlight_x += (target_hl_x - model.anim_highlight_x) * alpha;
    model.anim_highlight_w += (target_hl_w - model.anim_highlight_w) * alpha;

    // Smooth 144Hz interpolation for `#group-tray-expander` pill width (100% dynamic)
    let target_tray_w = compute_target_tray_width(model.tray_expanded, model.widgets.tray_items.len());
    if model.anim_tray_width < TRAY_COLLAPSED_WIDTH {
        model.anim_tray_width = target_tray_w;
    }
    if (target_tray_w - model.anim_tray_width).abs() > 0.1 {
        any_changed = true;
    }
    model.anim_tray_width += (target_tray_w - model.anim_tray_width) * alpha;

    let hover_spd = 36.0_f32;
    let hover_alpha = (1.0 - (-dt_sec * hover_spd).exp()).clamp(0.01, 1.0);

    for idx in 0..10 {
        let is_hovered = if let Some((hx, hy)) = model.hover_pos {
            let rect = model.ws_pill_rects[idx];
            pt_in_rect(hx, hy, rect)
        } else {
            false
        };
        let target_hover = if is_hovered { 1.0 } else { 0.0 };
        let diff_hover = (target_hover - model.anim_hover_t[idx]).abs();
        if diff_hover > 0.02 {
            any_changed = true;
        }
        model.anim_hover_t[idx] += (target_hover - model.anim_hover_t[idx]) * hover_alpha;
    }

    let target_dash = if matches!(model.active_popover, ActivePopover::Calendar { .. }) {
        1.0_f32
    } else {
        0.0_f32
    };
    let dash_alpha = (1.0 - (-dt_sec * 22.0).exp()).clamp(0.02, 1.0);
    if (target_dash - model.anim_dashboard_morph).abs() > 0.002 {
        model.anim_dashboard_morph += (target_dash - model.anim_dashboard_morph) * dash_alpha;
        any_changed = true;
    } else if model.anim_dashboard_morph != target_dash {
        model.anim_dashboard_morph = target_dash;
        any_changed = true;
    }

    // Smooth 100ms interpolation for top-center island width & height
    let win_alpha = (1.0 - (-dt_sec * 35.0).exp()).clamp(0.02, 1.0);
    if model.anim_window_w <= 1.0 {
        model.anim_window_w = model.target_window_w;
    }
    if model.anim_window_h <= 1.0 {
        model.anim_window_h = model.target_window_h;
    }
    let diff_win_w = (model.target_window_w - model.anim_window_w).abs();
    let diff_win_h = (model.target_window_h - model.anim_window_h).abs();
    if diff_win_w > 0.05 || diff_win_h > 0.05 {
        model.anim_window_w += (model.target_window_w - model.anim_window_w) * win_alpha;
        model.anim_window_h += (model.target_window_h - model.anim_window_h) * win_alpha;
        any_changed = true;
    } else {
        model.anim_window_w = model.target_window_w;
        model.anim_window_h = model.target_window_h;
    }

    model.anim_marquee_phase = (model.anim_marquee_phase + dt_sec) % 10000.0;
    if model.active_title.chars().count() > 60 || matches!(model.active_popover, ActivePopover::Calendar { .. }) {
        any_changed = true;
    }

    any_changed
}

// ----------------------------------------------------------------------------
// Native Windows DWM Acrylic Blur & Composition Attributes
// ----------------------------------------------------------------------------

#[repr(C)]
struct AccentPolicy {
    accent_state: u32,
    accent_flags: u32,
    gradient_color: u32,
    animation_id: u32,
}

#[repr(C)]
struct WindowCompositionAttribData {
    attrib: u32,
    pv_data: *mut core::ffi::c_void,
    cb_data: usize,
}

type SetWindowCompositionAttributeFn =
    unsafe extern "system" fn(HWND, *mut WindowCompositionAttribData) -> BOOL;

/// Enable native DWM hardware-accelerated Acrylic blur behind popovers (`hwnd`).
pub unsafe fn enable_window_acrylic(hwnd: HWND, gradient_abgr: u32) {
    if let Ok(user32) = GetModuleHandleW(PCWSTR(
        "user32.dll\0".encode_utf16().collect::<Vec<u16>>().as_ptr(),
    )) {
        if let Some(proc) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) {
            let set_wca: SetWindowCompositionAttributeFn = std::mem::transmute(proc);
            let mut policy = AccentPolicy {
                accent_state: 4, // ACCENT_ENABLE_ACRYLICBLURBEHIND
                accent_flags: 2,
                gradient_color: gradient_abgr,
                animation_id: 0,
            };
            let mut data = WindowCompositionAttribData {
                attrib: 19, // WCA_ACCENT_POLICY
                pv_data: &mut policy as *mut _ as *mut core::ffi::c_void,
                cb_data: std::mem::size_of::<AccentPolicy>(),
            };
            let _ = set_wca(hwnd, &mut data);
        }
    }
}

/// Disables DWM hardware Acrylic blur and window region clipping on `hwnd`.
pub unsafe fn disable_window_acrylic(hwnd: HWND) {
    if let Ok(user32) = GetModuleHandleW(PCWSTR(
        "user32.dll\0".encode_utf16().collect::<Vec<u16>>().as_ptr(),
    )) {
        if let Some(proc) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) {
            let set_wca: SetWindowCompositionAttributeFn = std::mem::transmute(proc);
            let mut policy = AccentPolicy {
                accent_state: 0, // ACCENT_DISABLED
                accent_flags: 0,
                gradient_color: 0,
                animation_id: 0,
            };
            let mut data = WindowCompositionAttribData {
                attrib: 19, // WCA_ACCENT_POLICY
                pv_data: &mut policy as *mut _ as *mut core::ffi::c_void,
                cb_data: std::mem::size_of::<AccentPolicy>(),
            };
            let _ = set_wca(hwnd, &mut data);
        }
    }

    #[repr(C)]
    struct DwmBlurBehind {
        dw_flags: u32,
        f_enable: BOOL,
        h_rgn_blur: HRGN,
        f_transition_on_maximized: BOOL,
    }

    if let Ok(dwmapi) = GetModuleHandleW(PCWSTR(
        "dwmapi.dll\0".encode_utf16().collect::<Vec<u16>>().as_ptr(),
    )) {
        if let Some(proc) = GetProcAddress(dwmapi, s!("DwmEnableBlurBehindWindow")) {
            type DwmEnableBlurBehindWindowFn =
                unsafe extern "system" fn(HWND, *const DwmBlurBehind) -> windows::core::HRESULT;
            let dwm_eb: DwmEnableBlurBehindWindowFn = std::mem::transmute(proc);
            let bb = DwmBlurBehind {
                dw_flags: 1, // DWM_BB_ENABLE
                f_enable: BOOL(0),
                h_rgn_blur: HRGN::default(),
                f_transition_on_maximized: BOOL(0),
            };
            let _ = dwm_eb(hwnd, &bb);
        }
    }

    let _ = SetWindowRgn(hwnd, HRGN::default(), false);
}

// ----------------------------------------------------------------------------
// Per-Notch Cached Backdrop Blur & Mojave Dynamic Wallpaper Integration
// ----------------------------------------------------------------------------

#[repr(C)]
struct GdiplusStartupInput {
    gdiplus_version: u32,
    debug_event_callback: usize,
    suppress_background_thread: i32,
    suppress_external_codecs: i32,
}

#[link(name = "gdiplus")]
extern "system" {
    fn GdiplusStartup(
        token: *mut usize,
        input: *const GdiplusStartupInput,
        output: *mut usize,
    ) -> i32;
    fn GdipCreateBitmapFromFile(filename: *const u16, bitmap: *mut usize) -> i32;
    fn GdipCreateHBITMAPFromBitmap(
        bitmap: usize,
        hbm_return: *mut HBITMAP,
        background: u32,
    ) -> i32;
    fn GdipDisposeImage(image: usize) -> i32;
}

fn mojave_stage_for_local_time() -> u8 {
    let st = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let mins = (st.wHour as u32) * 60 + (st.wMinute as u32);
    match mins {
        60..=299 => 16,    // 01:00 - 04:59
        300..=374 => 1,    // 05:00 - 06:14
        375..=434 => 2,    // 06:15 - 07:14
        435..=509 => 3,    // 07:15 - 08:29
        510..=584 => 4,    // 08:30 - 09:44
        585..=674 => 5,    // 09:45 - 11:14
        675..=764 => 6,    // 11:15 - 12:44
        765..=854 => 7,    // 12:45 - 14:14
        855..=929 => 8,    // 14:15 - 15:29
        930..=989 => 9,    // 15:30 - 16:29
        990..=1034 => 10,  // 16:30 - 17:14
        1035..=1079 => 11, // 17:15 - 17:59
        1080..=1124 => 12, // 18:00 - 18:44
        1125..=1184 => 13, // 18:45 - 19:44
        1185..=1289 => 14, // 19:45 - 21:29
        _ => 15,           // 21:30 - 00:59
    }
}

fn resolve_active_wallpaper_path(stage: u8) -> Option<PathBuf> {
    let file_name = format!("mojave_dynamic_{stage}.jpeg");
    let home = std::env::var("USERPROFILE").unwrap_or_default();

    let mut dirs = vec![
        PathBuf::from(r"c:\Users\mbura\Projects\Other\hyprland-win\assets\wallpapers\mojave"),
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
        dirs.push(PathBuf::from(&home).join(r".config\hyprland-win\wallpapers\mojave"));
        dirs.push(PathBuf::from(&home).join(r".config\Hyprwin\wallpapers\mojave"));
        dirs.push(PathBuf::from(&home).join(r".config\hypr\wallpapers\mojave"));
    }

    for dir in dirs {
        let candidate = dir.join(&file_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

unsafe fn load_wallpaper_top_strip_gdiplus(
    path: &Path,
    target_w: i32,
    target_h: i32,
) -> Option<Vec<u32>> {
    static GDIP_TOKEN: OnceLock<usize> = OnceLock::new();
    let _ = GDIP_TOKEN.get_or_init(|| {
        let mut token: usize = 0;
        let input = GdiplusStartupInput {
            gdiplus_version: 1,
            debug_event_callback: 0,
            suppress_background_thread: 0,
            suppress_external_codecs: 0,
        };
        let _ = GdiplusStartup(&mut token, &input, core::ptr::null_mut());
        token
    });

    let path_str = path.to_str()?;
    let path_u16: Vec<u16> = path_str.encode_utf16().chain(Some(0)).collect();
    let mut gp_bmp: usize = 0;
    if GdipCreateBitmapFromFile(path_u16.as_ptr(), &mut gp_bmp) != 0 || gp_bmp == 0 {
        return None;
    }

    let mut hbmp = HBITMAP::default();
    let status = GdipCreateHBITMAPFromBitmap(gp_bmp, &mut hbmp, 0xFF0D_0E13);
    let _ = GdipDisposeImage(gp_bmp);
    if status != 0 || hbmp.is_invalid() || hbmp.0.is_null() {
        return None;
    }

    let mut bm = BITMAP::default();
    GetObjectW(
        hbmp,
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut bm as *mut _ as *mut _),
    );
    let bm_w = bm.bmWidth;
    let bm_h = bm.bmHeight;
    if bm_w <= 0 || bm_h <= 0 {
        let _ = DeleteObject(hbmp);
        return None;
    }

    let screen_dc = GetDC(HWND::default());
    if screen_dc.is_invalid() {
        let _ = DeleteObject(hbmp);
        return None;
    }
    let src_dc = CreateCompatibleDC(screen_dc);
    let dst_dc = CreateCompatibleDC(screen_dc);
    if src_dc.is_invalid() || dst_dc.is_invalid() {
        if !src_dc.is_invalid() {
            let _ = DeleteDC(src_dc);
        }
        if !dst_dc.is_invalid() {
            let _ = DeleteDC(dst_dc);
        }
        let _ = ReleaseDC(HWND::default(), screen_dc);
        let _ = DeleteObject(hbmp);
        return None;
    }

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: target_w,
            biHeight: -target_h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (target_w * target_h * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
    let dib = match CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
        Ok(b) if !b.is_invalid() && !bits.is_null() => b,
        _ => {
            let _ = DeleteDC(src_dc);
            let _ = DeleteDC(dst_dc);
            let _ = ReleaseDC(HWND::default(), screen_dc);
            let _ = DeleteObject(hbmp);
            return None;
        }
    };

    let old_src = SelectObject(src_dc, hbmp);
    let old_dst = SelectObject(dst_dc, dib);

    let screen_h = windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
        windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN,
    )
    .max(720);
    let src_h_top = ((target_h as f64 / screen_h as f64) * (bm_h as f64))
        .round()
        .max(1.0) as i32;

    SetStretchBltMode(dst_dc, HALFTONE);
    let _ = SetBrushOrgEx(dst_dc, 0, 0, None);
    let _ = StretchBlt(
        dst_dc,
        0,
        0,
        target_w,
        target_h,
        src_dc,
        0,
        0,
        bm_w,
        src_h_top.min(bm_h),
        SRCCOPY,
    );

    let pixels_slice =
        std::slice::from_raw_parts(bits as *const u32, (target_w * target_h) as usize);
    let out = pixels_slice.to_vec();

    SelectObject(src_dc, old_src);
    SelectObject(dst_dc, old_dst);
    let _ = DeleteObject(dib);
    let _ = DeleteDC(src_dc);
    let _ = DeleteDC(dst_dc);
    let _ = ReleaseDC(HWND::default(), screen_dc);
    let _ = DeleteObject(hbmp);

    Some(out)
}

unsafe fn capture_screen_strip_below_bar(width: i32, height: i32) -> Option<Vec<u32>> {
    let screen_dc = GetDC(HWND::default());
    if screen_dc.is_invalid() {
        return None;
    }
    let mem_dc = CreateCompatibleDC(screen_dc);
    if mem_dc.is_invalid() {
        let _ = ReleaseDC(HWND::default(), screen_dc);
        return None;
    }

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (width * height * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
    let dib = match CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
        Ok(b) if !b.is_invalid() && !bits.is_null() => b,
        _ => {
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(HWND::default(), screen_dc);
            return None;
        }
    };

    let old = SelectObject(mem_dc, dib);
    let _ = BitBlt(mem_dc, 0, 0, width, height, screen_dc, 0, 36, SRCCOPY);

    let pixels_slice = std::slice::from_raw_parts(bits as *const u32, (width * height) as usize);
    let out = pixels_slice.to_vec();

    SelectObject(mem_dc, old);
    let _ = DeleteObject(dib);
    let _ = DeleteDC(mem_dc);
    let _ = ReleaseDC(HWND::default(), screen_dc);

    Some(out)
}

fn fallback_charcoal_gradient(width: i32, height: i32) -> Vec<u32> {
    let mut out = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        let t = (y as f32) / (height as f32).max(1.0);
        let r = (24.0 + 7.0 * t).round() as u32;
        let g = (25.0 + 7.0 * t).round() as u32;
        let b = (32.0 + 8.0 * t).round() as u32;
        let px = 0xFF00_0000 | (r << 16) | (g << 8) | b;
        for _ in 0..width {
            out.push(px);
        }
    }
    out
}

fn box_blur_horizontal(src: &[u32], dst: &mut [u32], w: i32, h: i32, r: i32) {
    let r = r.max(1);
    let div = 2 * r + 1;
    let inv_div = 1.0 / (div as f32);

    for y in 0..h {
        let row = (y * w) as usize;
        let mut sum_r = 0u32;
        let mut sum_g = 0u32;
        let mut sum_b = 0u32;

        let first = src[row];
        let last = src[row + (w - 1) as usize];

        for x in -r..=r {
            let px = if x < 0 {
                first
            } else if x >= w {
                last
            } else {
                src[row + x as usize]
            };
            sum_r += (px >> 16) & 0xFF;
            sum_g += (px >> 8) & 0xFF;
            sum_b += px & 0xFF;
        }

        for x in 0..w {
            let avg_r = (sum_r as f32 * inv_div).round() as u32;
            let avg_g = (sum_g as f32 * inv_div).round() as u32;
            let avg_b = (sum_b as f32 * inv_div).round() as u32;
            dst[row + x as usize] = 0xFF00_0000 | (avg_r << 16) | (avg_g << 8) | avg_b;

            let add_x = x + r + 1;
            let add_px = if add_x >= w { last } else { src[row + add_x as usize] };
            let sub_x = x - r;
            let sub_px = if sub_x < 0 { first } else { src[row + sub_x as usize] };

            sum_r += ((add_px >> 16) & 0xFF).wrapping_sub((sub_px >> 16) & 0xFF);
            sum_g += ((add_px >> 8) & 0xFF).wrapping_sub((sub_px >> 8) & 0xFF);
            sum_b += (add_px & 0xFF).wrapping_sub(sub_px & 0xFF);
        }
    }
}

fn box_blur_vertical(src: &[u32], dst: &mut [u32], w: i32, h: i32, r: i32) {
    let r = r.max(1);
    let div = 2 * r + 1;
    let inv_div = 1.0 / (div as f32);

    for x in 0..w {
        let mut sum_r = 0u32;
        let mut sum_g = 0u32;
        let mut sum_b = 0u32;

        let first = src[x as usize];
        let last = src[((h - 1) * w + x) as usize];

        for y in -r..=r {
            let px = if y < 0 {
                first
            } else if y >= h {
                last
            } else {
                src[(y * w + x) as usize]
            };
            sum_r += (px >> 16) & 0xFF;
            sum_g += (px >> 8) & 0xFF;
            sum_b += px & 0xFF;
        }

        for y in 0..h {
            let avg_r = (sum_r as f32 * inv_div).round() as u32;
            let avg_g = (sum_g as f32 * inv_div).round() as u32;
            let avg_b = (sum_b as f32 * inv_div).round() as u32;
            dst[(y * w + x) as usize] = 0xFF00_0000 | (avg_r << 16) | (avg_g << 8) | avg_b;

            let add_y = y + r + 1;
            let add_px = if add_y >= h { last } else { src[(add_y * w + x) as usize] };
            let sub_y = y - r;
            let sub_px = if sub_y < 0 { first } else { src[(sub_y * w + x) as usize] };

            sum_r += ((add_px >> 16) & 0xFF).wrapping_sub((sub_px >> 16) & 0xFF);
            sum_g += ((add_px >> 8) & 0xFF).wrapping_sub((sub_px >> 8) & 0xFF);
            sum_b += (add_px & 0xFF).wrapping_sub(sub_px & 0xFF);
        }
    }
}

fn fast_separable_gaussian_blur(buffer: &mut [u32], w: i32, h: i32, radius: i32) {
    if w <= 0 || h <= 0 || buffer.len() < (w * h) as usize {
        return;
    }
    let mut tmp = vec![0u32; (w * h) as usize];
    for _ in 0..3 {
        box_blur_horizontal(buffer, &mut tmp, w, h, radius);
        box_blur_vertical(&tmp, buffer, w, h, radius);
    }
}

fn apply_acrylic_tint(buffer: &mut [u32], tint_rgb: (u8, u8, u8), tint_opacity: f32) {
    let (tr, tg, tb) = (tint_rgb.0 as f32, tint_rgb.1 as f32, tint_rgb.2 as f32);
    let alpha = tint_opacity.clamp(0.0, 1.0);
    let inv_alpha = 1.0 - alpha;

    for px in buffer.iter_mut() {
        let p = *px;
        let r = ((p >> 16) & 0xFF) as f32;
        let g = ((p >> 8) & 0xFF) as f32;
        let b = (p & 0xFF) as f32;

        let out_r = (r * inv_alpha + tr * alpha).round().clamp(0.0, 255.0) as u32;
        let out_g = (g * inv_alpha + tg * alpha).round().clamp(0.0, 255.0) as u32;
        let out_b = (b * inv_alpha + tb * alpha).round().clamp(0.0, 255.0) as u32;

        *px = 0xFF00_0000 | (out_r << 16) | (out_g << 8) | out_b;
    }
}

fn load_raw_backdrop_strip(width: i32, height: i32, stage: u8) -> Vec<u32> {
    if let Some(path) = resolve_active_wallpaper_path(stage) {
        if let Some(strip) = unsafe { load_wallpaper_top_strip_gdiplus(&path, width, height) } {
            return strip;
        }
    }
    if let Some(screen_strip) = unsafe { capture_screen_strip_below_bar(width, height) } {
        return screen_strip;
    }
    fallback_charcoal_gradient(width, height)
}

struct CachedBarBackdrop {
    width: i32,
    height: i32,
    stage: u8,
    last_update: Instant,
    pixels: Vec<u32>,
}

static BAR_BACKDROP_CACHE: OnceLock<Mutex<CachedBarBackdrop>> = OnceLock::new();

pub fn sample_blurred_bar_backdrop(px: i32, py: i32, max_w: i32) -> u32 {
    let current_stage = mojave_stage_for_local_time();
    let cache_mutex = BAR_BACKDROP_CACHE.get_or_init(|| {
        let initial_w = max_w.max(640);
        let initial_h = 36;
        let mut pixels = load_raw_backdrop_strip(initial_w, initial_h, current_stage);
        fast_separable_gaussian_blur(&mut pixels, initial_w, initial_h, 14);
        apply_acrylic_tint(&mut pixels, (24, 25, 32), 0.74);
        Mutex::new(CachedBarBackdrop {
            width: initial_w,
            height: initial_h,
            stage: current_stage,
            last_update: Instant::now(),
            pixels,
        })
    });

    if let Ok(mut guard) = cache_mutex.lock() {
        let now = Instant::now();
        let target_w = max_w.max(640);
        let target_h = 36;
        let should_refresh = guard.width != target_w
            || guard.height != target_h
            || guard.stage != current_stage
            || now.duration_since(guard.last_update) >= std::time::Duration::from_secs(2);

        if should_refresh {
            let mut new_pixels = load_raw_backdrop_strip(target_w, target_h, current_stage);
            fast_separable_gaussian_blur(&mut new_pixels, target_w, target_h, 14);
            apply_acrylic_tint(&mut new_pixels, (24, 25, 32), 0.74);
            guard.width = target_w;
            guard.height = target_h;
            guard.stage = current_stage;
            guard.last_update = now;
            guard.pixels = new_pixels;
        }

        let clamp_x = px.clamp(0, guard.width - 1);
        let clamp_y = py.clamp(0, guard.height - 1);
        let idx = (clamp_y * guard.width + clamp_x) as usize;
        if idx < guard.pixels.len() {
            return guard.pixels[idx];
        }
    }

    0xFF18_1920
}

#[inline]
pub fn make_premul_pixel(color: COLORREF, opacity: f32) -> u32 {
    let a = (opacity.clamp(0.0, 1.0) * 255.0).round() as u32;
    if a == 0 {
        return 0;
    }
    let r = (color.0 & 0xFF) as u32;
    let g = ((color.0 >> 8) & 0xFF) as u32;
    let b = ((color.0 >> 16) & 0xFF) as u32;

    let pr = (r * a + 127) / 255;
    let pg = (g * a + 127) / 255;
    let pb = (b * a + 127) / 255;

    (a << 24) | (pr << 16) | (pg << 8) | pb
}

static SCREEN_FRAME_HWND: OnceLock<Mutex<Option<(isize, i32, i32, i32)>>> = OnceLock::new();
static SCREEN_FRAME_REGISTERED: OnceLock<()> = OnceLock::new();

unsafe fn paint_screen_frame_overlay(hwnd: HWND, width: i32, height: i32) {
    if width <= 0 || height <= 0 {
        return;
    }
    let screen_dc = GetDC(HWND::default());
    if screen_dc.is_invalid() {
        return;
    }
    let mem_dc = CreateCompatibleDC(screen_dc);
    if mem_dc.is_invalid() {
        let _ = ReleaseDC(HWND::default(), screen_dc);
        return;
    }

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (width * height * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
    let Ok(dib_bmp) = CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(HWND::default(), screen_dc);
        return;
    };

    let pixels: &mut [u32] =
        std::slice::from_raw_parts_mut(bits as *mut u32, (width * height) as usize);
    pixels.fill(0x0000_0000);

    let fill_rgb = rgb(18, 19, 24);
    let fill_opacity = 1.0_f32;
    let r_fillet = 22.0_f32;
    let border_t = MONITOR_BORDER_PX; // 9.0px

    let w_f = width as f32;
    let h_f = height as f32;
    let border_zone = 35_i32;

    for py in 0..height {
        let py_f = py as f32 + 0.5;
        let is_bottom_zone = py >= height - border_zone;
        let row_offset = (py as usize) * (width as usize);

        if is_bottom_zone {
            for px in 0..width {
                let px_f = px as f32 + 0.5;
                let dist = if px_f < border_t + r_fillet && py_f < r_fillet {
                    let dx = (border_t + r_fillet) - px_f;
                    let dy = r_fillet - py_f;
                    r_fillet - (dx * dx + dy * dy).sqrt()
                } else if px_f > w_f - border_t - r_fillet && py_f < r_fillet {
                    let dx = px_f - (w_f - border_t - r_fillet);
                    let dy = r_fillet - py_f;
                    r_fillet - (dx * dx + dy * dy).sqrt()
                } else if px_f < border_t + r_fillet && py_f > h_f - border_t - r_fillet {
                    let dx = (border_t + r_fillet) - px_f;
                    let dy = py_f - (h_f - border_t - r_fillet);
                    r_fillet - (dx * dx + dy * dy).sqrt()
                } else if px_f > w_f - border_t - r_fillet && py_f > h_f - border_t - r_fillet {
                    let dx = px_f - (w_f - border_t - r_fillet);
                    let dy = py_f - (h_f - border_t - r_fillet);
                    r_fillet - (dx * dx + dy * dy).sqrt()
                } else {
                    (px_f - border_t).min(w_f - border_t - px_f).min(h_f - border_t - py_f)
                };

                let cov = (0.5 - dist).clamp(0.0, 1.0);
                if cov > 0.001 {
                    let idx = row_offset + (px as usize);
                    if idx < pixels.len() {
                        pixels[idx] = make_premul_pixel(fill_rgb, fill_opacity * cov);
                    }
                }
            }
        } else {
            for px in 0..border_zone.min(width) {
                let px_f = px as f32 + 0.5;
                let dist = if px_f < border_t + r_fillet && py_f < r_fillet {
                    let dx = (border_t + r_fillet) - px_f;
                    let dy = r_fillet - py_f;
                    r_fillet - (dx * dx + dy * dy).sqrt()
                } else {
                    (px_f - border_t).min(w_f - border_t - px_f).min(h_f - border_t - py_f)
                };

                let cov = (0.5 - dist).clamp(0.0, 1.0);
                if cov > 0.001 {
                    let idx = row_offset + (px as usize);
                    if idx < pixels.len() {
                        pixels[idx] = make_premul_pixel(fill_rgb, fill_opacity * cov);
                    }
                }
            }
            for px in (width - border_zone).max(border_zone)..width {
                let px_f = px as f32 + 0.5;
                let dist = if px_f > w_f - border_t - r_fillet && py_f < r_fillet {
                    let dx = px_f - (w_f - border_t - r_fillet);
                    let dy = r_fillet - py_f;
                    r_fillet - (dx * dx + dy * dy).sqrt()
                } else {
                    (px_f - border_t).min(w_f - border_t - px_f).min(h_f - border_t - py_f)
                };

                let cov = (0.5 - dist).clamp(0.0, 1.0);
                if cov > 0.001 {
                    let idx = row_offset + (px as usize);
                    if idx < pixels.len() {
                        pixels[idx] = make_premul_pixel(fill_rgb, fill_opacity * cov);
                    }
                }
            }
        }
    }

    let old_bmp = SelectObject(mem_dc, dib_bmp);
    let mut blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let mut pt_src = POINT { x: 0, y: 0 };
    let mut sz = SIZE { cx: width, cy: height };

    let _ = UpdateLayeredWindow(
        hwnd,
        screen_dc,
        None,
        Some(&mut sz),
        mem_dc,
        Some(&mut pt_src),
        COLORREF(0),
        Some(&mut blend),
        ULW_ALPHA,
    );

    SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(dib_bmp);
    let _ = DeleteDC(mem_dc);
    let _ = ReleaseDC(HWND::default(), screen_dc);
}

unsafe extern "system" fn screen_frame_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

pub unsafe fn is_lockscreen_active() -> bool {
    let class_name: Vec<u16> = "HyprLockOverlayClass\0".encode_utf16().collect();
    let lock_hwnd = FindWindowW(
        PCWSTR(class_name.as_ptr()),
        PCWSTR::null(),
    ).unwrap_or_default();
    !lock_hwnd.0.is_null() && IsWindowVisible(lock_hwnd).as_bool()
}

pub unsafe fn hide_all_bar_windows(bar_hwnd: HWND) {
    let _ = ShowWindow(bar_hwnd, SW_HIDE);
    if let Some(frame_mutex) = SCREEN_FRAME_HWND.get() {
        if let Ok(guard) = frame_mutex.lock() {
            if let Some((hwnd_val, ..)) = *guard {
                let h = HWND(hwnd_val as *mut core::ffi::c_void);
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
    }
    if let Some(cal_mutex) = CALENDAR_HWND.get() {
        if let Ok(mut guard) = cal_mutex.lock() {
            if let Some((hwnd_val, _)) = *guard {
                let h = HWND(hwnd_val as *mut core::ffi::c_void);
                let _ = ShowWindow(h, SW_HIDE);
                *guard = Some((hwnd_val, false));
            }
        }
    }
}

pub unsafe fn ensure_screen_frame_overlay(
    screen_w: i32,
    screen_h: i32,
    bar_height: i32,
    is_fullscreen: bool,
) {
    let frame_mutex = SCREEN_FRAME_HWND.get_or_init(|| Mutex::new(None));
    let mut guard = frame_mutex.lock().unwrap();

    let ov_x = 0;
    let ov_y = bar_height;
    let ov_w = screen_w;
    let ov_h = (screen_h - bar_height).max(100);

    if is_fullscreen || is_lockscreen_active() {
        if let Some((hwnd_val, ..)) = *guard {
            let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        return;
    }

    let needs_recreate_or_redraw = match *guard {
        Some((_, w, h, b)) => w != ov_w || h != ov_h || b != bar_height,
        None => true,
    };

    let hwnd = match *guard {
        Some((hwnd_val, ..)) => HWND(hwnd_val as *mut core::ffi::c_void),
        None => {
            SCREEN_FRAME_REGISTERED.get_or_init(|| {
                let class_name: Vec<u16> = "HyprBarScreenFrameOverlay\0".encode_utf16().collect();
                let wc = WNDCLASSW {
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(screen_frame_wnd_proc),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    lpszClassName: PCWSTR(class_name.as_ptr()),
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);
            });

            let class_name: Vec<u16> = "HyprBarScreenFrameOverlay\0".encode_utf16().collect();
            let title: Vec<u16> = "HyprBar Screen Frame\0".encode_utf16().collect();
            let ex_style = WS_EX_TOPMOST
                | WS_EX_TOOLWINDOW
                | WS_EX_LAYERED
                | WS_EX_TRANSPARENT
                | WS_EX_NOACTIVATE;

            let Ok(h) = CreateWindowExW(
                ex_style,
                PCWSTR(class_name.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_POPUP,
                ov_x,
                ov_y,
                ov_w,
                ov_h,
                HWND::default(),
                None,
                None,
                None,
            ) else {
                return;
            };

            let round_pref = DWMWCP_DONOTROUND.0 as u32;
            let _ = DwmSetWindowAttribute(
                h,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &round_pref as *const u32 as *const _,
                4,
            );
            let border_none: u32 = 0xFFFF_FFFE;
            let _ = DwmSetWindowAttribute(
                h,
                DWMWA_BORDER_COLOR,
                &border_none as *const u32 as *const _,
                4,
            );

            h
        }
    };

    if needs_recreate_or_redraw {
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            ov_x,
            ov_y,
            ov_w,
            ov_h,
            SWP_NOACTIVATE,
        );

        paint_screen_frame_overlay(hwnd, ov_w, ov_h);
        *guard = Some((hwnd.0 as isize, ov_w, ov_h, bar_height));
    }

    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
}

// ----------------------------------------------------------------------------
// High-Quality Area-Integrated App Icon Renderer for HyprBar
// ----------------------------------------------------------------------------

struct RawHIconData {
    width: i32,
    height: i32,
    pixels: Vec<u32>,
}

unsafe fn extract_hicon_argb(hicon: HICON) -> Option<RawHIconData> {
    let mut info = ICONINFO::default();
    if GetIconInfo(hicon, &mut info).is_ok() {
        let hbm_color = info.hbmColor;
        let hbm_mask = info.hbmMask;

        if !hbm_color.is_invalid() && !hbm_color.0.is_null() {
            let mut bm = BITMAP::default();
            GetObjectW(
                hbm_color,
                std::mem::size_of::<BITMAP>() as i32,
                Some(&mut bm as *mut _ as *mut core::ffi::c_void),
            );
            let w = bm.bmWidth;
            let h = bm.bmHeight;

            if w > 0 && h > 0 {
                let screen_dc = GetDC(HWND::default());
                let mut bmi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: w,
                        biHeight: -h,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        biSizeImage: (w * h * 4) as u32,
                        ..Default::default()
                    },
                    ..Default::default()
                };

                let mut buffer = vec![0u32; (w * h) as usize];
                GetDIBits(
                    screen_dc,
                    hbm_color,
                    0,
                    h as u32,
                    Some(buffer.as_mut_ptr() as *mut _),
                    &mut bmi,
                    DIB_RGB_COLORS,
                );

                let has_alpha = buffer.iter().any(|&p| (p >> 24) > 0);

                if has_alpha {
                    for p in buffer.iter_mut() {
                        let a = (*p >> 24) & 0xFF;
                        if a == 0 {
                            *p = 0;
                        } else if a < 255 {
                            let r = (((*p >> 16) & 0xFF) * a + 127) / 255;
                            let g = (((*p >> 8) & 0xFF) * a + 127) / 255;
                            let b = ((*p & 0xFF) * a + 127) / 255;
                            *p = (a << 24) | (r << 16) | (g << 8) | b;
                        }
                    }
                } else if !hbm_mask.is_invalid() && !hbm_mask.0.is_null() {
                    let mut mask_buf = vec![0u32; (w * h) as usize];
                    GetDIBits(
                        screen_dc,
                        hbm_mask,
                        0,
                        h as u32,
                        Some(mask_buf.as_mut_ptr() as *mut _),
                        &mut bmi,
                        DIB_RGB_COLORS,
                    );
                    for i in 0..buffer.len() {
                        if (mask_buf[i] & 0x00FF_FFFF) != 0 {
                            buffer[i] = 0x0000_0000;
                        } else {
                            buffer[i] |= 0xFF00_0000;
                        }
                    }
                } else {
                    for p in buffer.iter_mut() {
                        *p |= 0xFF00_0000;
                    }
                }

                let _ = ReleaseDC(HWND::default(), screen_dc);
                let _ = DeleteObject(hbm_color);
                if !hbm_mask.is_invalid() && !hbm_mask.0.is_null() {
                    let _ = DeleteObject(hbm_mask);
                }

                return Some(RawHIconData {
                    width: w,
                    height: h,
                    pixels: buffer,
                });
            }

            let _ = DeleteObject(hbm_color);
        }

        if !hbm_mask.is_invalid() && !hbm_mask.0.is_null() {
            let _ = DeleteObject(hbm_mask);
        }
    }

    let sz = 48;
    let screen_dc = GetDC(HWND::default());
    if screen_dc.is_invalid() {
        return None;
    }
    let mem_dc = CreateCompatibleDC(screen_dc);
    if mem_dc.is_invalid() {
        let _ = ReleaseDC(HWND::default(), screen_dc);
        return None;
    }

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: sz,
            biHeight: -sz,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (sz * sz * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
    let Ok(dib) = CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(HWND::default(), screen_dc);
        return None;
    };

    let old_bmp = SelectObject(mem_dc, dib);
    let scratch_slice = std::slice::from_raw_parts_mut(bits as *mut u32, (sz * sz) as usize);
    scratch_slice.fill(0x0000_0000);

    let _ = DrawIconEx(mem_dc, 0, 0, hicon, sz, sz, 0, None, DI_NORMAL);

    let mut out = scratch_slice.to_vec();
    let has_alpha = out.iter().any(|&p| (p >> 24) > 0);
    if !has_alpha {
        for p in out.iter_mut() {
            if (*p & 0x00FF_FFFF) != 0 {
                *p |= 0xFF00_0000;
            }
        }
    }

    SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(dib);
    let _ = DeleteDC(mem_dc);
    let _ = ReleaseDC(HWND::default(), screen_dc);

    Some(RawHIconData {
        width: sz,
        height: sz,
        pixels: out,
    })
}

fn resample_hicon_premul(
    src: &[u32],
    src_w: i32,
    src_h: i32,
    dst_w: i32,
    dst_h: i32,
) -> Vec<u32> {
    let mut out = vec![0u32; (dst_w * dst_h) as usize];
    if src_w <= 0 || src_h <= 0 || dst_w <= 0 || dst_h <= 0 || src.is_empty() {
        return out;
    }

    if src_w == dst_w && src_h == dst_h {
        let copy_len = out.len().min(src.len());
        out[..copy_len].copy_from_slice(&src[..copy_len]);
        return out;
    }

    let x_scale = src_w as f32 / dst_w as f32;
    let y_scale = src_h as f32 / dst_h as f32;

    for dy in 0..dst_h {
        let sy_start = (dy as f32 * y_scale).floor() as i32;
        let sy_end = (((dy + 1) as f32 * y_scale).ceil() as i32).min(src_h);
        let row_dst = (dy * dst_w) as usize;

        for dx in 0..dst_w {
            let sx_start = (dx as f32 * x_scale).floor() as i32;
            let sx_end = (((dx + 1) as f32 * x_scale).ceil() as i32).min(src_w);

            let mut sum_a = 0.0f32;
            let mut sum_r = 0.0f32;
            let mut sum_g = 0.0f32;
            let mut sum_b = 0.0f32;
            let mut total_weight = 0.0f32;

            for sy in sy_start..sy_end {
                let y_weight = ((sy + 1) as f32).min((dy + 1) as f32 * y_scale)
                    - (sy as f32).max(dy as f32 * y_scale);
                if y_weight <= 0.0 {
                    continue;
                }
                let row_src = (sy * src_w) as usize;

                for sx in sx_start..sx_end {
                    let x_weight = ((sx + 1) as f32).min((dx + 1) as f32 * x_scale)
                        - (sx as f32).max(dx as f32 * x_scale);
                    if x_weight <= 0.0 {
                        continue;
                    }

                    let weight = x_weight * y_weight;
                    let p = src[row_src + sx as usize];
                    let a = ((p >> 24) & 0xFF) as f32;
                    let r = ((p >> 16) & 0xFF) as f32;
                    let g = ((p >> 8) & 0xFF) as f32;
                    let b = (p & 0xFF) as f32;

                    sum_a += a * weight;
                    sum_r += r * weight;
                    sum_g += g * weight;
                    sum_b += b * weight;
                    total_weight += weight;
                }
            }

            if total_weight > 0.0 {
                let inv = 1.0 / total_weight;
                let a = (sum_a * inv).round().clamp(0.0, 255.0) as u32;
                let r = (sum_r * inv).round().clamp(0.0, 255.0) as u32;
                let g = (sum_g * inv).round().clamp(0.0, 255.0) as u32;
                let b = (sum_b * inv).round().clamp(0.0, 255.0) as u32;
                out[row_dst + dx as usize] = (a << 24) | (r << 16) | (g << 8) | b;
            }
        }
    }

    out
}

static RESAMPLED_HICON_CACHE: OnceLock<Mutex<HashMap<(usize, i32), Vec<u32>>>> = OnceLock::new();

pub unsafe fn draw_hicon_aa(
    pixels: &mut [u32],
    stride: usize,
    max_w: i32,
    max_h: i32,
    hicon: HICON,
    dst_x: i32,
    dst_y: i32,
    dst_size: i32,
) {
    if dst_size <= 0 || hicon.0.is_null() {
        return;
    }

    let icon_key = (hicon.0 as usize, dst_size);
    let cache = RESAMPLED_HICON_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let cached_entry = if let Ok(guard) = cache.lock() {
        guard.get(&icon_key).cloned()
    } else {
        None
    };

    let resampled = if let Some(data) = cached_entry {
        data
    } else {
        let raw_opt = extract_hicon_argb(hicon);
        let res = if let Some(raw) = raw_opt {
            resample_hicon_premul(&raw.pixels, raw.width, raw.height, dst_size, dst_size)
        } else {
            Vec::new()
        };
        if let Ok(mut guard) = cache.lock() {
            guard.insert(icon_key, res.clone());
        }
        res
    };

    if resampled.is_empty() {
        return;
    }

    for iy in 0..dst_size {
        let py = dst_y + iy;
        if py < 0 || py >= max_h {
            continue;
        }
        let row_dst = (py as usize) * stride;
        let row_src = (iy * dst_size) as usize;

        for ix in 0..dst_size {
            let px = dst_x + ix;
            if px < 0 || px >= max_w {
                continue;
            }

            let src_p = resampled[row_src + ix as usize];
            let sa_u8 = (src_p >> 24) & 0xFF;
            if sa_u8 == 0 {
                continue;
            }

            let idx = row_dst + px as usize;
            if sa_u8 == 255 {
                pixels[idx] = src_p;
                continue;
            }

            let sa = sa_u8 as f32 / 255.0;
            let sr = ((src_p >> 16) & 0xFF) as f32 / 255.0;
            let sg = ((src_p >> 8) & 0xFF) as f32 / 255.0;
            let sb = (src_p & 0xFF) as f32 / 255.0;

            let dst_p = pixels[idx];
            let da = ((dst_p >> 24) & 0xFF) as f32 / 255.0;
            let dr = ((dst_p >> 16) & 0xFF) as f32 / 255.0;
            let dg = ((dst_p >> 8) & 0xFF) as f32 / 255.0;
            let db = (dst_p & 0xFF) as f32 / 255.0;

            let inv_sa = 1.0 - sa;
            let out_a = (sa + da * inv_sa).clamp(0.0, 1.0);
            let out_r = (sr + dr * inv_sa).clamp(0.0, 1.0);
            let out_g = (sg + dg * inv_sa).clamp(0.0, 1.0);
            let out_b = (sb + db * inv_sa).clamp(0.0, 1.0);

            let a_u8 = (out_a * 255.0).round() as u32;
            let r_u8 = (out_r * 255.0).round() as u32;
            let g_u8 = (out_g * 255.0).round() as u32;
            let b_u8 = (out_b * 255.0).round() as u32;
            pixels[idx] = (a_u8 << 24) | (r_u8 << 16) | (g_u8 << 8) | b_u8;
        }
    }
}

// ----------------------------------------------------------------------------
// Per-Pixel Alpha (`UpdateLayeredWindow` `ULW_ALPHA`) V2 Bar Renderer
// ----------------------------------------------------------------------------

pub unsafe fn paint_bar_layered(hwnd: HWND, model: &mut BarRenderModel) {
    if is_lockscreen_active() {
        hide_all_bar_windows(hwnd);
        return;
    }
    let width = model.bar_width.max(640);
    let height = model.bar_height.clamp(24, 64);
    let theme = model.theme.clone();

    ensure_private_fonts_loaded();

    let screen_dc = GetDC(HWND::default());
    if screen_dc.is_invalid() {
        return;
    }
    let mem_dc = CreateCompatibleDC(screen_dc);
    if mem_dc.is_invalid() {
        let _ = ReleaseDC(HWND::default(), screen_dc);
        return;
    }

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // top-down DIB
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (width * height * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
    let dib_bmp = match CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
        Ok(bmp) if !bmp.is_invalid() && !bits.is_null() => bmp,
        _ => {
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(HWND::default(), screen_dc);
            return;
        }
    };

    let pixels: &mut [u32] =
        std::slice::from_raw_parts_mut(bits as *mut u32, (width * height) as usize);

    // 1. Start with a 100% transparent canvas (`0x0000_0000`) — V2 `#waybar { background-color: transparent; }`
    // with ZERO outer bar box and ZERO border!
    pixels.fill(0x0000_0000);

    let screen_w = GetSystemMetrics(SM_CXSCREEN).max(width);
    let screen_h = GetSystemMetrics(SM_CYSCREEN).max(1080);
    ensure_screen_frame_overlay(screen_w, screen_h, height, model.active_fullscreen);
    let _ = SetWindowPos(
        hwnd,
        HWND_TOPMOST,
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
    );

    // Top-Edge Notch Dimensions & Styling:
    // 3 distinct notches flush with screen top edge (y = 0.0) with exact concave ear fillets and rounded corners
    let notch_h = height as f32;
    let pill_h = 30;
    let pill_y = (height - pill_h) / 2;
    let notch_corner_r = 14.0_f32;
    let notch_ear_r = 14.0_f32;
    let notch_fill_rgb = rgb(18, 19, 24);
    let notch_border_rgb = rgb(18, 19, 24);
    let notch_fill_opacity = 1.0_f32;
    let notch_border_opacity = 0.0_f32;

    let mut icons_to_draw: Vec<(HICON, i32, i32, i32)> = Vec::new();

    // Top continuous 9px monitor border
    let top_border_alpha = (notch_fill_opacity * 255.0).round() as u8;
    for py in 0..(MONITOR_BORDER_PX as i32) {
        let row = (py as usize) * (width as usize);
        for px in 0..(width as usize) {
            pixels[row + px] = alpha_blend_straight_bgra(pixels[row + px], notch_fill_rgb, top_border_alpha);
        }
    }

    let old_bmp = SelectObject(mem_dc, dib_bmp);
    SetBkMode(mem_dc, TRANSPARENT);

    let font_size = -theme.font_size.abs().max(16);
    let font_ui = create_apple_font(font_size, 500);
    let font_bold = create_apple_font(font_size, 600);
    let font_icon = create_icon_font(font_size, 400);

    // ========================================================================
    // TOP-EDGE NOTCH: LEFT NOTCH (Flush with left edge x = 0.0)
    // #custom-omarchy + #workspaces
    // ========================================================================
    let left_notch_x = 0.0_f32;
    let omarchy_w = 30;
    let omarchy_x = 12;
    model.omarchy_rect = (omarchy_x, pill_y, omarchy_w, pill_h);

    let div1_x = omarchy_x + omarchy_w + 4;
    let ws_container_x = div1_x + 3;
    let ws_inner_pad = 5.0_f32;
    let pill_gap = theme.pill_gap;

    let mut total_ws_inner_w = 0.0_f32;
    let mut visible_ws_count = 0;
    for idx in 0..10 {
        let w = model.anim_pill_widths[idx];
        if w > 0.5 {
            if visible_ws_count > 0 {
                total_ws_inner_w += pill_gap;
            }
            total_ws_inner_w += w;
            visible_ws_count += 1;
        }
    }
    let ws_container_w = (total_ws_inner_w + ws_inner_pad * 2.0).round() as i32;
    model.ws_container_rect = (ws_container_x, pill_y, ws_container_w, pill_h);

    model.mpris_rect = (0, 0, 0, 0);

    let left_notch_w = (ws_container_x + ws_container_w + 14 - left_notch_x as i32) as f32;
    let left_end = (left_notch_x + left_notch_w + notch_ear_r).round() as i32;

    // 1. Draw Left Top-Edge Notch (Flush with left edge x = 0.0)
    draw_aa_notch(
        pixels,
        width as usize,
        width,
        height,
        left_notch_x,
        left_notch_w,
        notch_h,
        notch_corner_r,
        notch_ear_r,
        false,
        true,
        notch_fill_rgb,
        notch_border_rgb,
        notch_fill_opacity,
        notch_border_opacity,
    );

    // 2. Draw subtle 1px vertical dividers (height = pill_h - 10, color = rgb(54, 56, 64))
    let div_y = pill_y + 5;
    let div_h = pill_h - 10;
    fill_aa_round_rect_f32(
        pixels,
        width as usize,
        width,
        height,
        div1_x as f32,
        div_y as f32,
        1.0,
        div_h as f32,
        0.5,
        rgb(54, 56, 64),
        COLORREF(0),
        0.0,
        0.63,
    );

    // 5. Workspaces inner animated pills & sliding active highlight
    let inner_pill_h = pill_h - 4;
    let inner_pill_y = pill_y + 2;
    let inner_pill_r = (inner_pill_h as f32) * 0.5;
    let mut cur_ws_x = (ws_container_x as f32) + ws_inner_pad;

    for idx in 0..10 {
        let w = model.anim_pill_widths[idx];
        if w <= 0.5 {
            model.ws_pill_rects[idx] = (0, 0, 0, 0);
            continue;
        }
        let rx = cur_ws_x.round() as i32;
        let rw = w.round() as i32;
        model.ws_pill_rects[idx] = (rx, inner_pill_y, rw, inner_pill_h);

        let is_active = (idx + 1) as u32 == model.active_workspace;
        let is_occupied =
            model.occupied_workspaces[idx] > 0 || !model.workspace_hwnds[idx].is_empty();

        if !is_active && is_occupied {
            let bg_col = rgb(40, 41, 47);
            fill_aa_round_rect_f32(
                pixels,
                width as usize,
                width,
                height,
                cur_ws_x,
                inner_pill_y as f32,
                w,
                inner_pill_h as f32,
                inner_pill_r,
                bg_col,
                rgb(56, 58, 66),
                0.8,
                0.92,
            );
        }
        cur_ws_x += w + pill_gap;
    }

    let active_ws_idx = (model.active_workspace.saturating_sub(1)) as usize;
    let active_is_occupied = if active_ws_idx < 10 {
        model.occupied_workspaces[active_ws_idx] > 0
            || !model.workspace_hwnds[active_ws_idx].is_empty()
    } else {
        false
    };

    // Active workspace highlight pill inside `#workspaces` (with subtle macOS Dark top highlight) - ONLY for occupied workspaces!
    if active_is_occupied {
        fill_aa_round_rect_f32(
            pixels,
            width as usize,
            width,
            height,
            model.anim_highlight_x,
            inner_pill_y as f32,
            model.anim_highlight_w,
            inner_pill_h as f32,
            inner_pill_r,
            rgb(74, 76, 86),
            rgb(94, 96, 108),
            1.0,
            0.96,
        );
    }

    // ========================================================================
    // UNIFIED RIGHT ISLAND: #group-tray-expander + #group-ctl + #clock + #menu
    // ========================================================================

    // 0. Tabular widths calculation for #group-ctl
    let vol_icon = if model.widgets.volume_muted || model.widgets.volume_pct == 0 {
        "\u{EEE8}" //  Muted
    } else if model.widgets.volume_pct < 35 {
        "\u{F026}" //  Low
    } else if model.widgets.volume_pct < 70 {
        "\u{F027}" //  Medium
    } else {
        "\u{F028}" //  High
    };
    let vol_num_str = if model.widgets.volume_muted {
        "Mute".to_string()
    } else {
        format!("{}%", model.widgets.volume_pct)
    };
    let net_icon = match model.widgets.network_kind {
        NetworkKind::Wifi => "\u{F1EB}",         // 
        NetworkKind::Ethernet => "\u{EB01}",     // 
        NetworkKind::Disconnected => "\u{F127}", // 
    };
    let bt_icon = "\u{F294}"; // 
    let idle_icon = "\u{F0F4}"; // 
    let dnd_icon = if model.widgets.dnd_silenced {
        "\u{F1F6}" //  Silenced
    } else {
        "\u{F0F3}" //  Normal
    };

    let bat_info = if model.widgets.has_battery {
        let b_icon = if model.widgets.battery_charging {
            "\u{F0E7}" // 
        } else if model.widgets.battery_pct >= 85 {
            "\u{F240}" // 
        } else if model.widgets.battery_pct >= 60 {
            "\u{F241}" // 
        } else if model.widgets.battery_pct >= 35 {
            "\u{F242}" // 
        } else if model.widgets.battery_pct >= 15 {
            "\u{F243}" // 
        } else {
            "\u{F244}" // 
        };
        Some((b_icon, format!("{}%", model.widgets.battery_pct)))
    } else {
        None
    };

    let item_pad = 8;
    let w_idle = 20;
    let w_dnd = 20;
    let w_bt = 20;
    let w_net = 24;
    let w_vol = 66; // Fixed tabular width for icon (18px) + "100%"/"Mute"
    let w_bat = if bat_info.is_some() { 66 + item_pad } else { 0 };

    let ctl_inner_w = w_idle
        + item_pad
        + w_dnd
        + item_pad
        + w_bt
        + item_pad
        + w_net
        + item_pad
        + w_vol
        + w_bat;
    let ctl_w = ctl_inner_w + 16;

    // ========================================================================
    // TOP-EDGE NOTCH: RIGHT NOTCH (Flush with top y=0 and right x=width)
    // #group-tray-expander + #group-ctl + #clock + #menu
    // ========================================================================
    let right_pad = 10;

    // Section D: #menu (Removed from right notch - now bound to top-left Windows button)
    model.menu_rect = (0, 0, 0, 0);

    // Section C: #clock (Anchored flush to right edge)
    let (clock_icon, clock_str) = if model.clock_alt_mode {
        ("\u{F073}", model.clock_alt_text.clone())
    } else {
        ("\u{F017}", model.clock_text.clone())
    };
    let clock_w = if model.clock_alt_mode {
        let clock_text_w = measure_text_width(mem_dc, &clock_str, font_bold);
        (clock_text_w + 38).max(168)
    } else {
        88
    };
    let clock_x = width - 12 - clock_w;
    model.clock_rect = (clock_x, pill_y, clock_w, pill_h);
    let div_clock_x = clock_x - 4;

    // Section B: #group-ctl
    let ctl_x = div_clock_x - ctl_w - 4;
    let div_ctl_x = ctl_x - 4;

    let mut cx = ctl_x + 8;
    model.ctl_idle_rect = (cx, pill_y, w_idle, pill_h);
    cx += w_idle + item_pad;
    model.ctl_dnd_rect = (cx, pill_y, w_dnd, pill_h);
    cx += w_dnd + item_pad;
    model.ctl_bt_rect = (cx, pill_y, w_bt, pill_h);
    cx += w_bt + item_pad;
    model.ctl_net_rect = (cx, pill_y, w_net, pill_h);
    cx += w_net + item_pad;
    model.ctl_vol_rect = (cx, pill_y, w_vol, pill_h);
    cx += w_vol;
    model.ctl_cpu_rect = (0, 0, 0, 0);
    if w_bat > 0 {
        cx += item_pad;
        model.ctl_bat_rect = (cx, pill_y, w_bat - item_pad, pill_h);
    } else {
        model.ctl_bat_rect = (0, 0, 0, 0);
    }

    // Section A: #group-tray-expander (100% dynamic width & item stride)
    let tray_app_count = model.widgets.tray_items.len();
    let target_tray_w = compute_target_tray_width(model.tray_expanded, tray_app_count);
    let tray_w = model.anim_tray_width.round().max(TRAY_COLLAPSED_WIDTH) as i32;
    let tray_x = div_ctl_x - tray_w - 4;

    model.tray_app_rects.clear();
    let tray_fully_open = model.tray_expanded && (tray_w as f32 >= target_tray_w - 6.0);
    if tray_fully_open {
        model.tray_toggle_rect = (tray_x + TRAY_PADDING_X as i32, pill_y, TRAY_TOGGLE_WIDTH as i32, pill_h);
        let mut tx = tray_x + (TRAY_PADDING_X + TRAY_TOGGLE_WIDTH + TRAY_GAP_AFTER_TOGGLE) as i32;
        for _ in 0..tray_app_count {
            model.tray_app_rects.push((tx, pill_y, TRAY_ITEM_STRIDE as i32, pill_h));
            tx += TRAY_ITEM_STRIDE as i32;
        }
        model.tray_shot_rect = (0, 0, 0, 0);
        model.tray_lock_rect = (tx, pill_y, TRAY_LOCK_WIDTH as i32, pill_h);
        model.tray_tiling_rect = (0, 0, 0, 0);
    } else {
        model.tray_toggle_rect = (tray_x, pill_y, tray_w.min(TRAY_COLLAPSED_WIDTH as i32), pill_h);
        model.tray_shot_rect = (0, 0, 0, 0);
        model.tray_lock_rect = (0, 0, 0, 0);
        model.tray_tiling_rect = (0, 0, 0, 0);
    }

    // Right Top-Edge Notch bounds (flush to right edge x = width)
    let right_notch_x = (tray_x - 14) as f32;
    let right_notch_w = (width as f32) - right_notch_x;
    let right_start = (right_notch_x - notch_ear_r).round() as i32;

    // 2. Draw Right Top-Edge Notch (Flush with right edge x = width)
    draw_aa_notch(
        pixels,
        width as usize,
        width,
        height,
        right_notch_x,
        right_notch_w,
        notch_h,
        notch_corner_r,
        notch_ear_r,
        true,
        false,
        notch_fill_rgb,
        notch_border_rgb,
        notch_fill_opacity,
        notch_border_opacity,
    );

    // 3. Draw subtle 1px vertical dividers (tray | ctl | clock)
    let div_y = pill_y + 5;
    let div_h = pill_h - 10;
    for &dx in &[div_ctl_x, div_clock_x] {
        fill_aa_round_rect_f32(
            pixels,
            width as usize,
            width,
            height,
            dx as f32,
            div_y as f32,
            1.0,
            div_h as f32,
            0.5,
            rgb(54, 56, 64),
            COLORREF(0),
            0.0,
            0.63,
        );
    }

    // ========================================================================
    // TOP-EDGE NOTCH: CENTER NOTCH (Flush with top y=0)
    // #window (Auto-Hidden When Desktop is Empty per V2 CSS)
    // ========================================================================
    let v2_win_title = format_v2_window_title(&model.active_title, &model.active_class);
    let active_ws_idx = (model.active_workspace.saturating_sub(1) as usize).min(9);
    let has_active_win = !v2_win_title.is_empty()
        && (model.occupied_workspaces[active_ws_idx] > 0
            || !model.workspace_hwnds[active_ws_idx].is_empty());

    let title_display = if has_active_win {
        v2_win_title.clone()
    } else {
        String::new()
    };
    let char_count = title_display.chars().count();
    let center_x = width / 2;
    let allowed_left = left_end + 10;
    let allowed_right = right_start - 10;
    let max_avail = (allowed_right - allowed_left).max(120);

    if has_active_win {
        let ref_text_w = if char_count <= 60 {
            measure_text_width(mem_dc, &title_display, font_bold)
        } else {
            let first_60: String = title_display.chars().take(60).collect();
            measure_text_width(mem_dc, &first_60, font_bold)
        };
        model.target_window_w = (ref_text_w + 36).clamp(96, max_avail) as f32;
        model.target_window_h = notch_h;
    } else {
        model.target_window_w = 120.0;
        model.target_window_h = 20.0;
    }

    if allowed_right > allowed_left + 80 {
        let win_pill_w_f = model.anim_window_w.clamp(60.0, max_avail as f32);
        let win_pill_h_f = model.anim_window_h.clamp(16.0, notch_h);
        let win_pill_x_f = (center_x as f32) - win_pill_w_f * 0.5;
        let win_pill_x = win_pill_x_f.round() as i32;
        let win_pill_w = win_pill_w_f.round() as i32;
        model.window_rect = (win_pill_x, pill_y, win_pill_w, pill_h);

        if model.anim_dashboard_morph <= 0.002 && !matches!(model.active_popover, ActivePopover::Calendar { .. }) {
            draw_aa_notch(
                pixels,
                width as usize,
                width,
                height,
                win_pill_x_f,
                win_pill_w_f,
                win_pill_h_f,
                notch_corner_r,
                notch_ear_r,
                true,
                true,
                notch_fill_rgb,
                notch_border_rgb,
                notch_fill_opacity,
                notch_border_opacity,
            );
        }
    } else {
        model.window_rect = (0, 0, 0, 0);
    }

    // Ensure DWM hardware Acrylic blur and window region clipping remain disabled on the main bar HWND
    // so empty space between notches is 100% transparent with zero blur.
    static ONCE_DISABLE_ACRYLIC: OnceLock<()> = OnceLock::new();
    ONCE_DISABLE_ACRYLIC.get_or_init(|| {
        disable_window_acrylic(hwnd);
    });

    // 2b. Draw empty workspace dots / active horizontal pills BEFORE saving pre-GDI pixels
    for idx in 0..10 {
        let rect = model.ws_pill_rects[idx];
        if rect.2 <= 0 {
            continue;
        }
        let (rx, ry, rw, rh) = rect;
        let is_occupied =
            model.occupied_workspaces[idx] > 0 || !model.workspace_hwnds[idx].is_empty();

        if !is_occupied {
            let is_active = (idx + 1) as u32 == model.active_workspace;
            let w_f32 = rw as f32;
            let morph = if is_active {
                ((w_f32 - 18.0) / (26.0 - 18.0)).clamp(0.0, 1.0).max(0.35)
            } else {
                ((w_f32 - 18.0) / (26.0 - 18.0)).clamp(0.0, 1.0)
            };
            let dot_w = 6.0 + 10.0 * morph;
            let dot_color = lerp_color(rgb(146, 148, 158), rgb(236, 237, 242), morph);
            let dot_opacity = 0.85 + 0.15 * morph;

            let cx = (rx as f32) + (rw as f32) * 0.5;
            let cy = (ry as f32) + (rh as f32) * 0.5;
            let dot_x = cx - dot_w * 0.5;
            let dot_y = cy - 3.0; // 6.0 height centered vertically at cy

            fill_aa_round_rect_f32(
                pixels,
                width as usize,
                width,
                height,
                dot_x,
                dot_y,
                dot_w,
                6.0,
                3.0,
                dot_color,
                COLORREF(0),
                0.0,
                dot_opacity,
            );
        }
    }

    // 2c. Draw standalone single-glyph icons (`#custom-omarchy` Windows icon, tray icons, ctl icons,
    // plus separated icons for `vol`, `cpu`, `bat`, and `#clock`) using exact ink-bounding-box
    // centering BEFORE snapshotting `pre_gdi_pixels` so every icon is 100% geometrically centered!
    draw_icon_ink_centered(
        pixels,
        width as usize,
        width,
        height,
        "\u{F17A}",
        model.omarchy_rect,
        theme.color_text_white,
        font_icon,
    );

    let chevron = if model.tray_expanded {
        "\u{F054}" // 
    } else {
        "\u{F053}" // 
    };
    draw_icon_ink_centered(
        pixels,
        width as usize,
        width,
        height,
        chevron,
        model.tray_toggle_rect,
        theme.color_text_white,
        font_icon,
    );
    if tray_fully_open {
        draw_icon_ink_centered(
            pixels,
            width as usize,
            width,
            height,
            "\u{F023}",
            model.tray_lock_rect,
            theme.color_text_white,
            font_icon,
        );
    }

    let idle_col = if model.widgets.idle_inhibited {
        rgb(82, 196, 126) // macOS soft green
    } else {
        rgb(152, 154, 164)
    };
    draw_icon_ink_centered(
        pixels,
        width as usize,
        width,
        height,
        idle_icon,
        model.ctl_idle_rect,
        idle_col,
        font_icon,
    );

    let dnd_col = if model.widgets.dnd_silenced {
        rgb(235, 106, 102) // macOS soft coral red
    } else {
        rgb(152, 154, 164)
    };
    draw_icon_ink_centered(
        pixels,
        width as usize,
        width,
        height,
        dnd_icon,
        model.ctl_dnd_rect,
        dnd_col,
        font_icon,
    );

    let bt_col = if model.widgets.bluetooth_enabled {
        rgb(94, 158, 248) // macOS system blue
    } else {
        rgb(152, 154, 164)
    };
    draw_icon_ink_centered(
        pixels,
        width as usize,
        width,
        height,
        bt_icon,
        model.ctl_bt_rect,
        bt_col,
        font_icon,
    );

    let net_col = if model.widgets.network_kind == NetworkKind::Disconnected {
        rgb(235, 106, 102) // macOS soft coral red
    } else {
        theme.color_text_white
    };
    draw_icon_ink_centered(
        pixels,
        width as usize,
        width,
        height,
        net_icon,
        model.ctl_net_rect,
        net_col,
        font_icon,
    );

    // Draw separated Nerd Font icons for Volume, CPU, Battery, and Clock in fixed 18px left boxes
    let vol_col = if model.widgets.volume_muted {
        theme.color_text_muted
    } else {
        theme.color_text_white
    };
    draw_icon_ink_centered(
        pixels,
        width as usize,
        width,
        height,
        vol_icon,
        (model.ctl_vol_rect.0, model.ctl_vol_rect.1, 18, model.ctl_vol_rect.3),
        vol_col,
        font_icon,
    );

    if let Some((b_icon, _)) = bat_info {
        let bat_col = if model.widgets.battery_charging {
            rgb(82, 196, 126) // macOS soft green
        } else if model.widgets.battery_pct <= 15 {
            rgb(235, 106, 102) // macOS soft coral red
        } else {
            theme.color_text_white
        };
        draw_icon_ink_centered(
            pixels,
            width as usize,
            width,
            height,
            b_icon,
            (model.ctl_bat_rect.0, model.ctl_bat_rect.1, 18, model.ctl_bat_rect.3),
            bat_col,
            font_icon,
        );
    }

    draw_icon_ink_centered(
        pixels,
        width as usize,
        width,
        height,
        clock_icon,
        (model.clock_rect.0 + 6, model.clock_rect.1, 18, model.clock_rect.3),
        theme.color_clock_text,
        font_icon,
    );



    // Snapshot pre-GDI pixel buffer so we can preserve translucent pill alphas while giving
    // GDI text & app icons crisp full-opacity premultiplied alpha!
    let pre_gdi_pixels: Vec<u32> = pixels.to_vec();

    // ========================================================================
    // TEXT & ICON RENDERING PASS (GDI DrawTextW + DrawIconEx over SDF Pills)
    // ========================================================================

    // 2. Draw `#workspaces` inner icons or numbers (ONLY for occupied workspaces)
    for idx in 0..10 {
        let rect = model.ws_pill_rects[idx];
        if rect.2 <= 0 {
            continue;
        }
        let (rx, ry, rw, rh) = rect;
        let is_active = (idx + 1) as u32 == model.active_workspace;
        let hwnds = &model.workspace_hwnds[idx];
        let is_occupied = model.occupied_workspaces[idx] > 0 || !hwnds.is_empty();
        let ws_num = idx + 1;

        if is_occupied {
            let mut icons: Vec<HICON> = Vec::new();
            for &hwnd_val in hwnds.iter() {
                if let Some(hicon) = get_window_hicon(hwnd_val) {
                    icons.push(hicon);
                    if icons.len() >= 4 {
                        break;
                    }
                }
            }

            if !icons.is_empty() {
                let icon_count = icons.len() as i32;
                let icon_sz = 18;
                let gap = 5;
                let total_w = icon_count * icon_sz + (icon_count - 1) * gap;
                let start_icon_x = rx + (rw - total_w) / 2;
                let icon_y = ry + (rh - icon_sz) / 2;

                let mut cur_icon_x = start_icon_x;
                for &hicon in &icons {
                    icons_to_draw.push((hicon, cur_icon_x, icon_y, icon_sz));
                    cur_icon_x += icon_sz + gap;
                }
            } else {
                let mut rc = RECT {
                    left: rx,
                    top: ry,
                    right: rx + rw,
                    bottom: ry + rh,
                };
                let ws_str = format!("{ws_num}");
                let text_color = if is_active {
                    theme.color_ws_active_text
                } else {
                    theme.color_ws_occupied_text
                };
                draw_text_centered(mem_dc, &ws_str, &mut rc, text_color, font_bold);
            }
        }
    }

    // 4. Draw `#window` text (when visible and not morphing into Dashboard)
    if model.window_rect.2 > 0
        && !title_display.is_empty()
        && model.anim_window_h > 30.0
        && model.anim_dashboard_morph <= 0.002
        && !matches!(model.active_popover, ActivePopover::Calendar { .. })
    {
        let mut win_rc = RECT {
            left: model.window_rect.0 + 14,
            top: model.window_rect.1,
            right: model.window_rect.0 + model.window_rect.2 - 14,
            bottom: model.window_rect.1 + model.window_rect.3,
        };
        if char_count > 60 || measure_text_width(mem_dc, &title_display, font_bold) > win_rc.right - win_rc.left {
            draw_text_marquee(
                mem_dc,
                &title_display,
                &mut win_rc,
                theme.color_text_white,
                font_bold,
                model.anim_marquee_phase,
            );
        } else {
            draw_text_centered(
                mem_dc,
                &title_display,
                &mut win_rc,
                theme.color_text_white,
                font_bold,
            );
        }
    }

    // 5. Draw `#group-tray-expander` live background application icons
    if tray_fully_open {
        for (i, item) in model.widgets.tray_items.iter().enumerate() {
            if let Some(&(rx, ry, rw, rh)) = model.tray_app_rects.get(i) {
                if let Some(hicon) = get_tray_item_hicon(item) {
                    let icon_sz = 18;
                    let ix = rx + (rw - icon_sz) / 2;
                    let iy = ry + (rh - icon_sz) / 2;
                    icons_to_draw.push((hicon, ix, iy, icon_sz));
                }
            }
        }
    }

    // 6. Draw `#group-ctl` tabular numeric right sub-boxes (volume, battery)
    let mut rc_vol = RECT {
        left: model.ctl_vol_rect.0 + 18,
        top: model.ctl_vol_rect.1,
        right: model.ctl_vol_rect.0 + model.ctl_vol_rect.2,
        bottom: model.ctl_vol_rect.1 + model.ctl_vol_rect.3,
    };
    draw_text_centered(mem_dc, &vol_num_str, &mut rc_vol, vol_col, font_bold);

    if let Some((_, ref b_num_str)) = bat_info {
        let mut rc_bat = RECT {
            left: model.ctl_bat_rect.0 + 18,
            top: model.ctl_bat_rect.1,
            right: model.ctl_bat_rect.0 + model.ctl_bat_rect.2,
            bottom: model.ctl_bat_rect.1 + model.ctl_bat_rect.3,
        };
        draw_text_centered(
            mem_dc,
            b_num_str,
            &mut rc_bat,
            theme.color_text_white,
            font_bold,
        );
    }

    // 7. Draw `#clock` right numeric sub-box
    let mut clock_rc = RECT {
        left: model.clock_rect.0 + 24,
        top: model.clock_rect.1,
        right: model.clock_rect.0 + model.clock_rect.2 - 4,
        bottom: model.clock_rect.1 + model.clock_rect.3,
    };
    draw_text_centered(
        mem_dc,
        &clock_str,
        &mut clock_rc,
        theme.color_clock_text,
        font_bold,
    );

    // 8. Convert straight BGRA + GDI text/icon pixels into premultiplied BGRA for `UpdateLayeredWindow`
    for (idx, p_Slot) in pixels.iter_mut().enumerate() {
        let base_p = pre_gdi_pixels[idx];
        let base_a = (base_p >> 24) & 0xFF;
        let cur_p = *p_Slot;

        if cur_p == base_p {
            if base_a == 0 {
                *p_Slot = 0;
            } else {
                let r = (((base_p >> 16) & 0xFF) * base_a) / 255;
                let g = (((base_p >> 8) & 0xFF) * base_a) / 255;
                let b = ((base_p & 0xFF) * base_a) / 255;
                *p_Slot = (base_a << 24) | (r << 16) | (g << 8) | b;
            }
        } else {
            // GDI DrawTextW or DrawIconEx painted onto this pixel
            let cur_r = (cur_p >> 16) & 0xFF;
            let cur_g = (cur_p >> 8) & 0xFF;
            let cur_b = cur_p & 0xFF;
            let cur_a = (cur_p >> 24) & 0xFF;

            let dr = cur_r.abs_diff((base_p >> 16) & 0xFF);
            let dg = cur_g.abs_diff((base_p >> 8) & 0xFF);
            let db = cur_b.abs_diff(base_p & 0xFF);
            let max_diff = dr.max(dg).max(db);

            let out_a = if cur_a > 0 {
                base_a.max(cur_a)
            } else {
                base_a.max((base_a + max_diff * 2).min(255)).max(230)
            };

            let r = (cur_r * out_a) / 255;
            let g = (cur_g * out_a) / 255;
            let b = (cur_b * out_a) / 255;
            *p_Slot = (out_a << 24) | (r << 16) | (g << 8) | b;
        }
    }

    // Composite smooth anti-aliased app and tray icons directly in premultiplied BGRA onto pixels
    for (hicon, ix, iy, isz) in icons_to_draw {
        draw_hicon_aa(pixels, width as usize, width, height, hicon, ix, iy, isz);
    }

    // 9. Present per-pixel translucent frame via `UpdateLayeredWindow` (`ULW_ALPHA`)
    let pt_pos = POINT { x: 0, y: 0 };
    let sz_win = SIZE {
        cx: width,
        cy: height,
    };
    let pt_src = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let _ = UpdateLayeredWindow(
        hwnd,
        screen_dc,
        Some(&pt_pos),
        Some(&sz_win),
        mem_dc,
        Some(&pt_src),
        COLORREF(0),
        Some(&blend),
        ULW_ALPHA,
    );
    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

    // 10. Clean up GDI handles
    SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(font_ui);
    let _ = DeleteObject(font_bold);
    let _ = DeleteObject(font_icon);
    let _ = DeleteObject(dib_bmp);
    let _ = DeleteDC(mem_dc);
    let _ = ReleaseDC(HWND::default(), screen_dc);
}

#[inline]
fn tuple_to_rect((x, y, w, h): (i32, i32, i32, i32)) -> RECT {
    RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

unsafe fn measure_text_width(hdc: HDC, text: &str, font: HFONT) -> i32 {
    if !text.chars().any(is_nerd_font_glyph) {
        let old_font = SelectObject(hdc, font);
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let mut sz = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &utf16, &mut sz);
        SelectObject(hdc, old_font);
        return sz.cx;
    }

    let mut lf = LOGFONTW::default();
    GetObjectW(
        font,
        std::mem::size_of::<LOGFONTW>() as i32,
        Some(&mut lf as *mut LOGFONTW as *mut core::ffi::c_void),
    );
    let icon_font = create_icon_font(lf.lfHeight, 400);

    let mut total_w = 0;
    for (is_icon, chunk) in split_icon_chunks(text) {
        let f = if is_icon { icon_font } else { font };
        let old_f = SelectObject(hdc, f);
        let utf16: Vec<u16> = chunk.encode_utf16().collect();
        let mut sz = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &utf16, &mut sz);
        SelectObject(hdc, old_f);
        let chunk_w = if is_icon { sz.cx + (sz.cy / 2).max(12) } else { sz.cx };
        total_w += chunk_w;
    }
    let _ = DeleteObject(icon_font);
    total_w
}

// ----------------------------------------------------------------------------
// Interactive Popover Window (`HyprBarCalendarPopup`: Calendar / Volume / Network)
// ----------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PopoverSharedState {
    kind: ActivePopover,
    bar_hwnd: isize,
    volume_pct: u32,
    volume_muted: bool,
    mic_volume_pct: u32,
    mic_muted: bool,
    audio_outputs: Vec<AudioDeviceEntry>,
    audio_inputs: Vec<AudioDeviceEntry>,
    network_kind: NetworkKind,
    network_adapter: String,
    network_speed_mbps: u32,
    bluetooth_enabled: bool,
    bluetooth_devices: Vec<BluetoothDeviceEntry>,
    cal_year: i32,
    cal_month: u32,
    today_year: i32,
    today_month: u32,
    today_day: u32,
    menu_hovered_idx: Option<usize>,
    dashboard_tab: usize,
    anim_dashboard_morph: f32,
    start_pill_w: f32,
    start_pill_h: f32,
    title_text: String,
    media_title: String,
    media_artist: String,
    media_album: String,
    media_status: MediaPlaybackStatus,
    media_position_sec: u32,
    media_duration_sec: u32,
    cpu_usage_pct: u32,
    ram_used_gb: f32,
    ram_total_gb: f32,
    brightness_pct: u32,
    os_version_label: String,
    weather_temp_c: i32,
    weather_desc: String,
    weather_location: String,
    weather_icon: &'static str,
    anim_marquee_phase: f32,
}

#[derive(Clone, Copy)]
enum SystemMenuItem {
    Action {
        id: usize,
        icon: &'static str,
        label: &'static str,
    },
    Separator,
}

const SYSTEM_MENU_ITEMS: &[SystemMenuItem] = &[
    SystemMenuItem::Action {
        id: 0,
        icon: "\u{F108}",
        label: "About Hyprland-Win",
    },
    SystemMenuItem::Separator,
    SystemMenuItem::Action {
        id: 1,
        icon: "\u{F013}",
        label: "System Settings...",
    },
    SystemMenuItem::Separator,
    SystemMenuItem::Action {
        id: 2,
        icon: "\u{F057}",
        label: "Force Quit...",
    },
    SystemMenuItem::Separator,
    SystemMenuItem::Action {
        id: 3,
        icon: "\u{F186}",
        label: "Sleep",
    },
    SystemMenuItem::Action {
        id: 4,
        icon: "\u{F021}",
        label: "Restart...",
    },
    SystemMenuItem::Action {
        id: 5,
        icon: "\u{F011}",
        label: "Shut Down...",
    },
    SystemMenuItem::Separator,
    SystemMenuItem::Action {
        id: 6,
        icon: "\u{F023}",
        label: "Lock Screen",
    },
    SystemMenuItem::Action {
        id: 7,
        icon: "\u{F08B}",
        label: "Log Out...",
    },
];

static CALENDAR_HWND: OnceLock<Mutex<Option<(isize, bool)>>> = OnceLock::new();
static CALENDAR_REGISTERED: OnceLock<()> = OnceLock::new();
static POPOVER_STATE: OnceLock<Mutex<PopoverSharedState>> = OnceLock::new();

fn get_popover_state() -> &'static Mutex<PopoverSharedState> {
    POPOVER_STATE.get_or_init(|| {
        let lt = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        Mutex::new(PopoverSharedState {
            kind: ActivePopover::None,
            bar_hwnd: 0,
            volume_pct: 65,
            volume_muted: false,
            mic_volume_pct: 80,
            mic_muted: false,
            audio_outputs: Vec::new(),
            audio_inputs: Vec::new(),
            network_kind: NetworkKind::Ethernet,
            network_adapter: "Ethernet Adapter".to_string(),
            network_speed_mbps: 1000,
            bluetooth_enabled: true,
            bluetooth_devices: Vec::new(),
            cal_year: lt.wYear as i32,
            cal_month: lt.wMonth as u32,
            today_year: lt.wYear as i32,
            today_month: lt.wMonth as u32,
            today_day: lt.wDay as u32,
            menu_hovered_idx: None,
            dashboard_tab: 0,
            anim_dashboard_morph: 1.0,
            start_pill_w: 160.0,
            start_pill_h: 40.0,
            title_text: String::new(),
            media_title: String::new(),
            media_artist: String::new(),
            media_album: String::new(),
            media_status: MediaPlaybackStatus::Stopped,
            media_position_sec: 0,
            media_duration_sec: 0,
            cpu_usage_pct: 0,
            ram_used_gb: 0.0,
            ram_total_gb: 16.0,
            brightness_pct: 80,
            os_version_label: "Windows 11".to_string(),
            weather_temp_c: 18,
            weather_desc: "Clear · Tuzla".to_string(),
            weather_location: "Istanbul, Tuzla".to_string(),
            weather_icon: "\u{F185}",
            anim_marquee_phase: 0.0,
        })
    })
}

/// Sakamoto's algorithm: returns 0 = Sunday .. 6 = Saturday for any Gregorian date.
fn day_of_week_gregorian(y: i32, m: u32, d: u32) -> usize {
    let t = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let m_idx = (m.clamp(1, 12) - 1) as usize;
    let y_adj = if m < 3 { y - 1 } else { y };
    ((y_adj + y_adj / 4 - y_adj / 100 + y_adj / 400 + t[m_idx] + d as i32)
        .rem_euclid(7)) as usize
}

pub unsafe fn update_calendar_popup(bar_hwnd: HWND, model: &BarRenderModel, hover_clock: bool) {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };

    let mut effective_kind = model.active_popover;

    if effective_kind == ActivePopover::None && model.anim_dashboard_morph > 0.002 {
        effective_kind = ActivePopover::Calendar {
            year: model.cur_year.max(2024),
            month: model.cur_month.clamp(1, 12),
        };
    }

    let show = effective_kind != ActivePopover::None;

    let (popup_w, popup_h, anchor_rect) = match effective_kind {
        ActivePopover::Calendar { .. } => (780, 414, model.window_rect),
        ActivePopover::Volume => {
            let out_rows = model.widgets.audio_outputs.len().clamp(1, 5) as i32;
            let in_rows = model.widgets.audio_inputs.len().clamp(1, 4) as i32;
            let h = 222 + out_rows * 28 + in_rows * 28;
            (328, h, model.ctl_vol_rect)
        }
        ActivePopover::Bluetooth => {
            let bt_rows = model.widgets.bluetooth_devices.len().clamp(1, 6) as i32;
            let h = 122 + bt_rows * 36;
            (320, h, model.ctl_bt_rect)
        }
        ActivePopover::Network => (286, 162, model.ctl_net_rect),
        ActivePopover::SystemMenu => (248, 268, model.omarchy_rect),
        ActivePopover::AboutHyprlandWin => (340, 210, model.omarchy_rect),
        ActivePopover::None => (780, 406, model.window_rect),
    };

    let (popup_x, popup_y) = if matches!(effective_kind, ActivePopover::Calendar { .. }) {
        let px = ((model.bar_width - 780) / 2).max(0);
        (px, 0)
    } else if matches!(
        effective_kind,
        ActivePopover::SystemMenu | ActivePopover::AboutHyprlandWin
    ) {
        (6.max(model.omarchy_rect.0 - 2), model.bar_height + 5)
    } else {
        let anchor_cx = if anchor_rect.2 > 0 {
            anchor_rect.0 + anchor_rect.2 / 2
        } else {
            model.bar_width - popup_w / 2 - 10
        };
        let px = (anchor_cx - popup_w / 2)
            .max(10)
            .min((model.bar_width - popup_w - 10).max(10));
        let py = model.bar_height + 6;
        (px, py)
    };

    if let Ok(mut pst) = get_popover_state().lock() {
        let prev_kind = pst.kind;
        pst.bar_hwnd = bar_hwnd.0 as isize;
        pst.volume_pct = model.widgets.volume_pct;
        pst.volume_muted = model.widgets.volume_muted;
        pst.mic_volume_pct = model.widgets.mic_volume_pct;
        pst.mic_muted = model.widgets.mic_muted;
        pst.audio_outputs = model.widgets.audio_outputs.clone();
        pst.audio_inputs = model.widgets.audio_inputs.clone();
        pst.network_kind = model.widgets.network_kind;
        pst.network_adapter = model.widgets.network_adapter.clone();
        pst.network_speed_mbps = model.widgets.network_speed_mbps;
        pst.bluetooth_enabled = model.widgets.bluetooth_enabled;
        pst.bluetooth_devices = model.widgets.bluetooth_devices.clone();
        pst.today_year = model.cur_year.max(2024);
        pst.today_month = model.cur_month.clamp(1, 12);
        pst.today_day = model.cur_day.max(1);
        pst.dashboard_tab = pst.dashboard_tab.min(2);
        pst.anim_dashboard_morph = model.anim_dashboard_morph;
        pst.start_pill_w = model.anim_window_w.max(96.0);
        pst.start_pill_h = model.anim_window_h.max(20.0);
        pst.title_text = format_v2_window_title(&model.active_title, &model.active_class);
        pst.media_position_sec = model.widgets.media.position_sec;
        pst.media_duration_sec = model.widgets.media.duration_sec.max(1);
        pst.os_version_label = model.widgets.os_version_label.clone();
        pst.weather_temp_c = model.widgets.weather_temp_c;
        pst.weather_desc = model.widgets.weather_desc.clone();
        pst.weather_location = model.widgets.weather_location.clone();
        pst.weather_icon = model.widgets.weather_icon;
        pst.anim_marquee_phase = model.anim_marquee_phase;
        if model.widgets.media.title.is_empty() {
            pst.media_title = "No Media Playing".to_string();
            pst.media_artist = String::new();
            pst.media_album = String::new();
        } else {
            pst.media_title = model.widgets.media.title.clone();
            pst.media_artist = model.widgets.media.artist.clone();
            pst.media_album = "Media".to_string();
        }
        pst.media_status = model.widgets.media.status.clone();
        pst.cpu_usage_pct = model.cpu_usage_pct;
        pst.ram_used_gb = model.ram_used_gb;
        pst.ram_total_gb = model.ram_total_gb;
        if pst.brightness_pct == 0 {
            pst.brightness_pct = 80;
        }
        if effective_kind == ActivePopover::SystemMenu {
            pst.menu_hovered_idx = None;
        }
        if let ActivePopover::Calendar { year, month } = effective_kind {
            if !matches!(prev_kind, ActivePopover::Calendar { .. }) {
                pst.cal_year = year;
                pst.cal_month = month;
            }
        }
        pst.kind = effective_kind;
    }

    let cal_mutex = CALENDAR_HWND.get_or_init(|| Mutex::new(None));
    let mut guard = cal_mutex.lock().unwrap();

    if show {
        if let Some((hwnd_val, is_shown)) = *guard {
            let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
            let is_dashboard = matches!(effective_kind, ActivePopover::Calendar { .. });
            let round_pref = if is_dashboard {
                DWMWCP_DONOTROUND.0 as u32
            } else {
                DWMWCP_ROUND.0 as u32
            };
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &round_pref as *const u32 as *const _,
                4,
            );
            let border_col: u32 = if is_dashboard {
                0xFFFF_FFFE
            } else {
                0x0048_3B38
            };
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR,
                &border_col as *const u32 as *const _,
                4,
            );
            disable_window_acrylic(hwnd);
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                popup_x,
                popup_y,
                popup_w,
                popup_h,
                SWP_NOACTIVATE,
            );
            paint_calendar_popup_layered(hwnd, popup_w, popup_h);
            if !is_shown {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                *guard = Some((hwnd_val, true));
            }
        } else {
            CALENDAR_REGISTERED.get_or_init(|| {
                let class_name: Vec<u16> = "HyprBarCalendarPopup\0".encode_utf16().collect();
                let wc = WNDCLASSW {
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(calendar_wnd_proc),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    lpszClassName: PCWSTR(class_name.as_ptr()),
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);
            });

            let class_name: Vec<u16> = "HyprBarCalendarPopup\0".encode_utf16().collect();
            let title: Vec<u16> = "HyprBar Popover\0".encode_utf16().collect();
            let ex_style = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;

            if let Ok(hwnd) = CreateWindowExW(
                ex_style,
                PCWSTR(class_name.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_POPUP,
                popup_x,
                popup_y,
                popup_w,
                popup_h,
                bar_hwnd,
                None,
                None,
                None,
            ) {
                let is_dashboard = matches!(effective_kind, ActivePopover::Calendar { .. });
                let round_pref = if is_dashboard {
                    DWMWCP_DONOTROUND.0 as u32
                } else {
                    DWMWCP_ROUND.0 as u32
                };
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_WINDOW_CORNER_PREFERENCE,
                    &round_pref as *const u32 as *const _,
                    4,
                );
                let border_col: u32 = if is_dashboard {
                    0xFFFF_FFFE
                } else {
                    0x0048_3B38
                };
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_BORDER_COLOR,
                    &border_col as *const u32 as *const _,
                    4,
                );
                disable_window_acrylic(hwnd);

                let _ = SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    popup_x,
                    popup_y,
                    popup_w,
                    popup_h,
                    SWP_NOACTIVATE,
                );
                paint_calendar_popup_layered(hwnd, popup_w, popup_h);
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                *guard = Some((hwnd.0 as isize, true));
            }
        }
    } else if let Some((hwnd_val, is_shown)) = *guard {
        if is_shown {
            let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
            let _ = ShowWindow(hwnd, SW_HIDE);
            *guard = Some((hwnd_val, false));
        }
    }
}

pub unsafe fn check_cursor_hover_leave(bar_hwnd: HWND, model: &mut BarRenderModel) {
    let mut pt = POINT::default();
    if GetCursorPos(&mut pt).is_ok() {
        let mut bar_rc = RECT::default();
        let _ = GetWindowRect(bar_hwnd, &mut bar_rc);

        let inside_bar = pt.x >= bar_rc.left
            && pt.x <= bar_rc.right
            && pt.y >= bar_rc.top
            && pt.y <= bar_rc.bottom + 6;

        let mut inside_popup = false;
        if let Some(cal_mutex) = CALENDAR_HWND.get() {
            if let Ok(guard) = cal_mutex.lock() {
                if let Some((hwnd_val, true)) = *guard {
                    let cal_hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
                    let mut cal_rc = RECT::default();
                    if GetWindowRect(cal_hwnd, &mut cal_rc).is_ok() {
                        inside_popup = pt.x >= cal_rc.left - 8
                            && pt.x <= cal_rc.right + 8
                            && pt.y >= cal_rc.top - 10
                            && pt.y <= cal_rc.bottom + 8;
                    }
                }
            }
        }

        if matches!(model.active_popover, ActivePopover::Calendar { .. }) {
            let client_x = pt.x - bar_rc.left;
            let client_y = pt.y - bar_rc.top;
            let inside_win = model.window_rect.2 > 0
                && client_x >= model.window_rect.0 - 8
                && client_x <= model.window_rect.0 + model.window_rect.2 + 8
                && client_y >= model.window_rect.1 - 8
                && client_y <= model.window_rect.1 + model.window_rect.3 + 8;
            if !inside_win && !inside_popup {
                model.active_popover = ActivePopover::None;
                update_calendar_popup(bar_hwnd, model, false);
            }
        }

        if !inside_bar && !inside_popup {
            model.hover_pos = None;
            model.active_popover = ActivePopover::None;
            update_calendar_popup(bar_hwnd, model, false);
            update_tooltip_popup(bar_hwnd, model, false);
        }
    }
}

pub fn draw_aa_dashboard_notch_straight(
    pixels: &mut [u32],
    stride: usize,
    max_w: i32,
    max_h: i32,
    x: f32,
    w: f32,
    h: f32,
    r_corner: f32,
    r_ear: f32,
    fill_rgb: COLORREF,
    _border_rgb: COLORREF,
    fill_opacity: f32,
    _border_opacity: f32,
) {
    if w <= 0.0 || h <= 0.0 || max_w <= 0 || max_h <= 0 {
        return;
    }
    let x0 = x;
    let x1 = x + w;
    let min_x = ((x0 - r_ear - 1.0).floor() as i32).max(0);
    let max_x = ((x1 + r_ear + 2.0).ceil() as i32).min(max_w);
    let min_y = 0_i32;
    let max_y = ((h + 2.0).ceil() as i32).min(max_h);

    for py in min_y..max_y {
        let row_offset = (py as usize) * stride;
        let py_f = py as f32 + 0.5;

        for px in min_x..max_x {
            let px_f = px as f32 + 0.5;
            let idx = row_offset + (px as usize);
            if idx >= pixels.len() {
                continue;
            }

            let dist = compute_notch_sdf(
                px_f, py_f, x0, x1, h, r_corner, r_ear, true, true,
            );

            let outer_cov = (0.5 - dist).clamp(0.0, 1.0);
            if outer_cov <= 0.001 {
                continue;
            }

            let a = (outer_cov * fill_opacity * 255.0).round().clamp(0.0, 255.0) as u8;
            pixels[idx] = alpha_blend_straight_bgra(pixels[idx], fill_rgb, a);
        }
    }

    // Top monitor border
    let alpha = (fill_opacity * 255.0).round().clamp(0.0, 255.0) as u8;
    for py in 0..(MONITOR_BORDER_PX as i32) {
        if py >= max_h { break; }
        let row = (py as usize) * stride;
        for px in 0..max_w {
            let idx = row + (px as usize);
            if idx < pixels.len() {
                pixels[idx] = alpha_blend_straight_bgra(pixels[idx], fill_rgb, alpha);
            }
        }
    }
}

pub fn draw_aa_progress_arc(
    pixels: &mut [u32],
    stride: usize,
    max_w: i32,
    max_h: i32,
    cx: f32,
    cy: f32,
    r_arc: f32,
    thickness: f32,
    start_deg: f32,
    sweep_deg: f32,
    color: COLORREF,
    opacity: f32,
) {
    if sweep_deg <= 0.0 || thickness <= 0.0 || opacity <= 0.001 {
        return;
    }
    let half_th = thickness * 0.5;
    let r_max = r_arc + half_th;
    let box_min_x = ((cx - r_max - 2.0).floor() as i32).max(0);
    let box_max_x = ((cx + r_max + 2.0).ceil() as i32).min(max_w);
    let box_min_y = ((cy - r_max - 2.0).floor() as i32).max(0);
    let box_max_y = ((cy + r_max + 2.0).ceil() as i32).min(max_h);

    for py in box_min_y..box_max_y {
        let row_offset = (py as usize) * stride;
        let py_f = py as f32 + 0.5;
        let dy = py_f - cy;

        for px in box_min_x..box_max_x {
            let px_f = px as f32 + 0.5;
            let dx = px_f - cx;
            let dist = (dx * dx + dy * dy).sqrt();

            let r_dist = (dist - r_arc).abs() - half_th;
            let radial_cov = (0.5 - r_dist).clamp(0.0, 1.0);
            if radial_cov <= 0.001 {
                continue;
            }

            let mut deg = dy.atan2(dx).to_degrees();
            if deg < 0.0 {
                deg += 360.0;
            }

            let s = (start_deg % 360.0 + 360.0) % 360.0;
            let d = (deg - s + 360.0) % 360.0;
            let in_arc = if d <= sweep_deg {
                let edge_dist = d.min(sweep_deg - d) * dist.max(1.0) * (std::f32::consts::PI / 180.0);
                (edge_dist + 0.5).clamp(0.0, 1.0)
            } else {
                0.0
            };

            let cov = radial_cov * in_arc;
            if cov > 0.001 {
                let idx = row_offset + (px as usize);
                if idx < pixels.len() {
                    let a = (cov * opacity * 255.0).clamp(0.0, 255.0) as u8;
                    pixels[idx] = alpha_blend_straight_bgra(pixels[idx], color, a);
                }
            }
        }
    }
}

pub fn draw_aa_bongo_cat(
    pixels: &mut [u32],
    stride: usize,
    max_w: i32,
    max_h: i32,
    cx: f32,
    cy: f32,
) {
    let body_col = rgb(246, 247, 252);
    let border_col = rgb(46, 48, 60);
    let pink_pad = rgb(242, 166, 196);

    // 1. Desk line underneath
    let desk_y = cy + 24.0;
    fill_aa_round_rect_f32(
        pixels, stride, max_w, max_h,
        cx - 40.0, desk_y, 80.0, 2.0, 1.0,
        rgb(66, 70, 86), COLORREF(0), 0.0, 0.9,
    );

    // 2. Body blob
    fill_aa_round_rect_f32(
        pixels, stride, max_w, max_h,
        cx - 22.0, cy - 14.0, 44.0, 36.0, 18.0,
        body_col, border_col, 1.0, 1.0,
    );

    // 3. Ears
    fill_aa_round_rect_f32(
        pixels, stride, max_w, max_h,
        cx - 16.0, cy - 22.0, 10.0, 14.0, 4.0,
        body_col, border_col, 1.0, 1.0,
    );
    fill_aa_round_rect_f32(
        pixels, stride, max_w, max_h,
        cx - 14.0, cy - 20.0, 6.0, 9.0, 2.5,
        pink_pad, COLORREF(0), 0.0, 0.85,
    );

    fill_aa_round_rect_f32(
        pixels, stride, max_w, max_h,
        cx + 6.0, cy - 22.0, 10.0, 14.0, 4.0,
        body_col, border_col, 1.0, 1.0,
    );
    fill_aa_round_rect_f32(
        pixels, stride, max_w, max_h,
        cx + 8.0, cy - 20.0, 6.0, 9.0, 2.5,
        pink_pad, COLORREF(0), 0.0, 0.85,
    );

    // 4. Closed happy eyes
    fill_aa_round_rect_f32(
        pixels, stride, max_w, max_h,
        cx - 10.0, cy - 5.0, 5.0, 2.5, 1.2,
        border_col, COLORREF(0), 0.0, 1.0,
    );
    fill_aa_round_rect_f32(
        pixels, stride, max_w, max_h,
        cx + 5.0, cy - 5.0, 5.0, 2.5, 1.2,
        border_col, COLORREF(0), 0.0, 1.0,
    );

    // Mouth
    fill_aa_round_rect_f32(
        pixels, stride, max_w, max_h,
        cx - 2.0, cy + 1.0, 4.0, 2.0, 1.0,
        pink_pad, COLORREF(0), 0.0, 1.0,
    );

    // 5. Paws resting on the desk
    fill_aa_round_rect_f32(
        pixels, stride, max_w, max_h,
        cx - 20.0, cy + 14.0, 14.0, 10.0, 5.0,
        body_col, border_col, 1.0, 1.0,
    );
    fill_aa_round_rect_f32(
        pixels, stride, max_w, max_h,
        cx - 17.0, cy + 16.0, 8.0, 6.0, 3.0,
        pink_pad, COLORREF(0), 0.0, 0.9,
    );

    fill_aa_round_rect_f32(
        pixels, stride, max_w, max_h,
        cx + 6.0, cy + 14.0, 14.0, 10.0, 5.0,
        body_col, border_col, 1.0, 1.0,
    );
    fill_aa_round_rect_f32(
        pixels, stride, max_w, max_h,
        cx + 9.0, cy + 16.0, 8.0, 6.0, 3.0,
        pink_pad, COLORREF(0), 0.0, 0.9,
    );
}

unsafe extern "system" fn calendar_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{WM_ERASEBKGND, WM_LBUTTONDOWN, WM_MOUSEMOVE};

    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONDOWN | WM_MOUSEMOVE => {
            let is_click = msg == WM_LBUTTONDOWN;
            let lbutton_held = (wparam.0 & 1) != 0;
            let mx = (lparam.0 & 0xFFFF) as i16 as i32;
            let my = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut rc = RECT::default();
            let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);
            let w = rc.right - rc.left;
            let h = rc.bottom - rc.top;

            let mut need_repaint = false;
            let mut bar_hwnd_val: isize = 0;

            if let Ok(mut pst) = get_popover_state().lock() {
                bar_hwnd_val = pst.bar_hwnd;

                if pst.kind == ActivePopover::SystemMenu {
                    let mut hovered_item = None;
                    let mut cur_y = 6;
                    for item in SYSTEM_MENU_ITEMS {
                        match item {
                            SystemMenuItem::Action { id, .. } => {
                                if mx >= 6 && mx <= w - 6 && my >= cur_y && my < cur_y + 28 {
                                    hovered_item = Some(*id);
                                    break;
                                }
                                cur_y += 28;
                            }
                            SystemMenuItem::Separator => {
                                cur_y += 9;
                            }
                        }
                    }
                    if pst.menu_hovered_idx != hovered_item {
                        pst.menu_hovered_idx = hovered_item;
                        need_repaint = true;
                    }

                    if !is_click && !lbutton_held {
                        drop(pst);
                        if need_repaint {
                            paint_calendar_popup_layered(hwnd, w, h);
                        }
                        return LRESULT(0);
                    }
                } else if !is_click && !lbutton_held {
                    return LRESULT(0);
                }
                match pst.kind {
                    ActivePopover::Calendar { .. } => {
                        let body_x = 24;
                        // 1. Calendar Month Navigation: y in 156..190, x in body_x + 112 .. body_x + 416
                        if is_click && my >= 156 && my <= 190 && mx >= body_x + 112 && mx <= body_x + 416 {
                            if mx < body_x + 160 {
                                // Prev Month
                                if pst.cal_month <= 1 {
                                    pst.cal_month = 12;
                                    pst.cal_year -= 1;
                                } else {
                                    pst.cal_month -= 1;
                                }
                                need_repaint = true;
                            } else if mx > body_x + 368 {
                                // Next Month
                                if pst.cal_month >= 12 {
                                    pst.cal_month = 1;
                                    pst.cal_year += 1;
                                } else {
                                    pst.cal_month += 1;
                                }
                                need_repaint = true;
                            } else {
                                // Reset to Today
                                pst.cal_year = pst.today_year;
                                pst.cal_month = pst.today_month;
                                need_repaint = true;
                            }
                        }

                        // 2. Vertical Sliders: y in 172..314 (Centered Volume & Mic)
                        if (is_click || lbutton_held) && my >= 172 && my <= 314 {
                            let pct = (((314 - my) * 100) / 142).clamp(0, 100) as u32;
                            // Track 1: Master Volume (x around body_x + 452)
                            if (body_x + 440..=body_x + 470).contains(&mx) {
                                crate::widgets::set_master_volume_pct(pct);
                                pst.volume_pct = pct;
                                if pct > 0 {
                                    pst.volume_muted = false;
                                }
                                need_repaint = true;
                            }
                            // Track 2: Mic Volume (x around body_x + 488)
                            else if (body_x + 476..=body_x + 506).contains(&mx) {
                                crate::widgets::set_mic_volume_pct(pct);
                                pst.mic_volume_pct = pct;
                                if pct > 0 {
                                    pst.mic_muted = false;
                                }
                                need_repaint = true;
                            }
                        }

                        // 3. Playback Transport Buttons: y in 250..280, cx = body_x + 626
                        if is_click && my >= 250 && my <= 280 {
                            let cx = body_x + 626;
                            if mx >= cx - 48 && mx <= cx - 18 {
                                crate::widgets::media_prev_track();
                                need_repaint = true;
                            } else if mx >= cx - 16 && mx <= cx + 16 {
                                crate::widgets::media_play_pause();
                                need_repaint = true;
                            } else if mx >= cx + 18 && mx <= cx + 48 {
                                crate::widgets::media_next_track();
                                need_repaint = true;
                            }
                        }
                    }
                    ActivePopover::Volume => {
                        // 1. Output volume slider track: y in 36..=56, x in 14..=(w - 14)
                        if my >= 36 && my <= 56 && mx >= 14 && mx <= w - 14 {
                            let track_w = (w - 36).max(1);
                            let pct = (((mx - 18) * 100) / track_w).clamp(0, 100) as u32;
                            crate::widgets::set_master_volume_pct(pct);
                            pst.volume_pct = pct;
                            if pct > 0 {
                                pst.volume_muted = false;
                            }
                            need_repaint = true;
                        } else if is_click && my >= 12 && my <= 36 && mx >= w - 74 && mx <= w - 14 {
                            // Output Mute button
                            crate::widgets::toggle_master_mute();
                            pst.volume_muted = !pst.volume_muted;
                            need_repaint = true;
                        } else {
                            let out_count = pst.audio_outputs.len().clamp(1, 5) as i32;
                            let out_end_y = 80 + out_count * 28;
                            let mic_sec_y = out_end_y + 10;
                            let in_count = pst.audio_inputs.len().clamp(1, 4) as i32;
                            let in_dev_start_y = mic_sec_y + 66;
                            let in_end_y = in_dev_start_y + in_count * 28;
                            let snd_btn_y = in_end_y + 10;

                            if is_click && my >= 80 && my < out_end_y && mx >= 14 && mx <= w - 14 {
                                // Output device selection
                                let idx = ((my - 80) / 28) as usize;
                                if let Some(dev) = pst.audio_outputs.get(idx) {
                                    let dev_id = dev.id.clone();
                                    crate::widgets::set_default_audio_device(&dev_id, false);
                                    for (i, d) in pst.audio_outputs.iter_mut().enumerate() {
                                        d.is_default = (i == idx);
                                    }
                                    need_repaint = true;
                                }
                            } else if is_click && my >= mic_sec_y && my <= mic_sec_y + 24 && mx >= w - 74 && mx <= w - 14 {
                                // Mic mute button
                                crate::widgets::toggle_mic_mute();
                                pst.mic_muted = !pst.mic_muted;
                                need_repaint = true;
                            } else if my >= mic_sec_y + 26 && my <= mic_sec_y + 46 && mx >= 14 && mx <= w - 14 {
                                // Mic volume slider track
                                let track_w = (w - 36).max(1);
                                let pct = (((mx - 18) * 100) / track_w).clamp(0, 100) as u32;
                                crate::widgets::set_mic_volume_pct(pct);
                                pst.mic_volume_pct = pct;
                                if pct > 0 {
                                    pst.mic_muted = false;
                                }
                                need_repaint = true;
                            } else if is_click && my >= in_dev_start_y && my < in_end_y && mx >= 14 && mx <= w - 14 {
                                // Input device selection
                                let idx = ((my - in_dev_start_y) / 28) as usize;
                                if let Some(dev) = pst.audio_inputs.get(idx) {
                                    let dev_id = dev.id.clone();
                                    crate::widgets::set_default_audio_device(&dev_id, true);
                                    for (i, d) in pst.audio_inputs.iter_mut().enumerate() {
                                        d.is_default = (i == idx);
                                    }
                                    need_repaint = true;
                                }
                            } else if is_click && my >= snd_btn_y && my <= snd_btn_y + 32 {
                                if mx >= 14 && mx <= w / 2 - 4 {
                                    crate::widgets::spawn_detached_command("sndvol.exe", &[]);
                                } else if mx >= w / 2 + 4 && mx <= w - 14 {
                                    crate::widgets::spawn_detached_command(
                                        "explorer.exe",
                                        &["ms-settings:sound"],
                                    );
                                }
                            }
                        }
                    }
                    ActivePopover::Bluetooth if is_click => {
                        let bt_count = pst.bluetooth_devices.len().clamp(1, 6) as i32;
                        let bt_dev_start_y = 66;
                        let bt_end_y = bt_dev_start_y + bt_count * 36;
                        let bt_btn_y = bt_end_y + 10;

                        if my >= 12 && my <= 36 && mx >= w - 116 && mx <= w - 66 {
                            // Scan / Refresh button
                            crate::widgets::refresh_bluetooth_devices();
                            need_repaint = true;
                        } else if my >= 12 && my <= 36 && mx >= w - 62 && mx <= w - 14 {
                            // Power toggle pill
                            pst.bluetooth_enabled = crate::widgets::toggle_bluetooth_enabled();
                            need_repaint = true;
                        } else if my >= bt_dev_start_y && my < bt_end_y && mx >= 14 && mx <= w - 14 {
                            // Device row click: toggle connection
                            let idx = ((my - bt_dev_start_y) / 36) as usize;
                            if let Some(dev) = pst.bluetooth_devices.get(idx) {
                                let addr = dev.address;
                                crate::widgets::toggle_bluetooth_device_connection(addr);
                                if let Some(d) = pst.bluetooth_devices.get_mut(idx) {
                                    d.connected = !d.connected;
                                }
                                need_repaint = true;
                            }
                        } else if my >= bt_btn_y && my <= bt_btn_y + 32 {
                            if mx >= 14 && mx <= w / 2 - 4 {
                                crate::widgets::spawn_detached_command(
                                    "explorer.exe",
                                    &["ms-settings:bluetooth"],
                                );
                            } else if mx >= w / 2 + 4 && mx <= w - 14 {
                                crate::widgets::spawn_detached_command(
                                    "control.exe",
                                    &["bthprops.cpl"],
                                );
                            }
                        }
                    }
                    ActivePopover::Network if is_click => {
                        // Bluetooth toggle row: y in 66..=104
                        if my >= 66 && my <= 104 && mx >= 14 && mx <= w - 14 {
                            pst.bluetooth_enabled = crate::widgets::toggle_bluetooth_enabled();
                            need_repaint = true;
                        } else if my >= 116 && my <= 150 {
                            if mx >= 14 && mx <= w / 2 - 4 {
                                crate::widgets::spawn_detached_command(
                                    "explorer.exe",
                                    &["ms-settings:network"],
                                );
                            } else if mx >= w / 2 + 4 && mx <= w - 14 {
                                crate::widgets::spawn_detached_command(
                                    "explorer.exe",
                                    &["ms-settings:bluetooth"],
                                );
                            }
                        }
                    }
                    ActivePopover::SystemMenu if is_click => {
                        let mut cur_y = 6;
                        let mut clicked_id = None;
                        for item in SYSTEM_MENU_ITEMS {
                            match item {
                                SystemMenuItem::Action { id, .. } => {
                                    if mx >= 6 && mx <= w - 6 && my >= cur_y && my < cur_y + 28 {
                                        clicked_id = Some(*id);
                                        break;
                                    }
                                    cur_y += 28;
                                }
                                SystemMenuItem::Separator => {
                                    cur_y += 9;
                                }
                            }
                        }

                        if let Some(id) = clicked_id {
                            match id {
                                0 => {
                                    // About Hyprland-Win
                                    pst.kind = ActivePopover::AboutHyprlandWin;
                                    pst.menu_hovered_idx = None;
                                    need_repaint = true;
                                    let mut wrc = RECT::default();
                                    let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut wrc);
                                    let cur_x = wrc.left;
                                    let cur_y = wrc.top;
                                    let _ = SetWindowPos(
                                        hwnd,
                                        HWND_TOPMOST,
                                        cur_x,
                                        cur_y,
                                        340,
                                        210,
                                        SWP_NOACTIVATE,
                                    );
                                }
                                1 => {
                                    // System Settings...
                                    pst.kind = ActivePopover::None;
                                    pst.menu_hovered_idx = None;
                                    let _ = ShowWindow(hwnd, SW_HIDE);
                                    crate::widgets::spawn_detached_command("explorer.exe", &["ms-settings:"]);
                                }
                                2 => {
                                    // Force Quit... (Task Manager)
                                    pst.kind = ActivePopover::None;
                                    pst.menu_hovered_idx = None;
                                    let _ = ShowWindow(hwnd, SW_HIDE);
                                    crate::widgets::spawn_detached_command("taskmgr.exe", &[]);
                                }
                                3 => {
                                    // Sleep
                                    pst.kind = ActivePopover::None;
                                    pst.menu_hovered_idx = None;
                                    let _ = ShowWindow(hwnd, SW_HIDE);
                                    crate::widgets::spawn_detached_command(
                                        "rundll32.exe",
                                        &["powrprof.dll,SetSuspendState", "0,1,0"],
                                    );
                                }
                                4 => {
                                    // Restart...
                                    pst.kind = ActivePopover::None;
                                    pst.menu_hovered_idx = None;
                                    let _ = ShowWindow(hwnd, SW_HIDE);
                                    crate::widgets::spawn_detached_command("shutdown.exe", &["/r", "/t", "0"]);
                                }
                                5 => {
                                    // Shut Down...
                                    pst.kind = ActivePopover::None;
                                    pst.menu_hovered_idx = None;
                                    let _ = ShowWindow(hwnd, SW_HIDE);
                                    crate::widgets::spawn_detached_command("shutdown.exe", &["/s", "/t", "0"]);
                                }
                                6 => {
                                    // Lock Screen
                                    pst.kind = ActivePopover::None;
                                    pst.menu_hovered_idx = None;
                                    let _ = ShowWindow(hwnd, SW_HIDE);
                                    crate::widgets::launch_hyprlock();
                                }
                                7 => {
                                    // Log Out...
                                    pst.kind = ActivePopover::None;
                                    pst.menu_hovered_idx = None;
                                    let _ = ShowWindow(hwnd, SW_HIDE);
                                    crate::widgets::spawn_detached_command("shutdown.exe", &["/l"]);
                                }
                                _ => {}
                            }
                        }
                    }
                    ActivePopover::AboutHyprlandWin if is_click => {
                        if my >= 166 && my <= 196 && mx >= 14 && mx <= w - 14 {
                            pst.kind = ActivePopover::None;
                            let _ = ShowWindow(hwnd, SW_HIDE);
                        }
                    }
                    _ => {}
                }
            }

            if need_repaint {
                let mut rc2 = RECT::default();
                let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc2);
                paint_calendar_popup_layered(
                    hwnd,
                    (rc2.right - rc2.left).max(100),
                    (rc2.bottom - rc2.top).max(80),
                );
                if bar_hwnd_val != 0 {
                    let bh = HWND(bar_hwnd_val as *mut core::ffi::c_void);
                    let _ = InvalidateRect(bh, None, false);
                }
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
            let _ = windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);
            let w = (rc.right - rc.left).max(100);
            let h = (rc.bottom - rc.top).max(80);
            paint_calendar_popup_layered(hwnd, w, h);
            let _ = windows::Win32::Graphics::Gdi::EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn paint_calendar_popup_layered(hwnd: HWND, w: i32, h: i32) {
    let w = w.max(100);
    let h = h.max(80);
    let hdc = GetDC(HWND::default());
    if hdc.is_invalid() {
        return;
    }

    let mem_dc = CreateCompatibleDC(hdc);
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (w * h * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
    if let Ok(dib_bmp) = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
        let pixels: &mut [u32] =
            std::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize);

                let pst = get_popover_state()
                    .lock()
                    .map(|g| g.clone())
                    .unwrap_or_else(|_| PopoverSharedState {
                        kind: ActivePopover::None,
                        bar_hwnd: 0,
                        volume_pct: 65,
                        volume_muted: false,
                        mic_volume_pct: 80,
                        mic_muted: false,
                        audio_outputs: Vec::new(),
                        audio_inputs: Vec::new(),
                        network_kind: NetworkKind::Ethernet,
                        network_adapter: "Ethernet".to_string(),
                        network_speed_mbps: 1000,
                        bluetooth_enabled: true,
                        bluetooth_devices: Vec::new(),
                        cal_year: 2026,
                        cal_month: 11,
                        today_year: 2026,
                        today_month: 11,
                        today_day: 1,
                        menu_hovered_idx: None,
                        dashboard_tab: 0,
                        anim_dashboard_morph: 0.0,
                        start_pill_w: 120.0,
                        start_pill_h: 20.0,
                        title_text: String::new(),
                        media_title: String::new(),
                        media_artist: String::new(),
                        media_album: String::new(),
                        media_status: MediaPlaybackStatus::Stopped,
                        media_position_sec: 0,
                        media_duration_sec: 0,
                        cpu_usage_pct: 0,
                        ram_used_gb: 0.0,
                        ram_total_gb: 16.0,
                        brightness_pct: 80,
                        os_version_label: "Windows 11".to_string(),
                        weather_temp_c: 18,
                        weather_desc: "Clear · Tuzla".to_string(),
                        weather_location: "Istanbul, Tuzla".to_string(),
                        weather_icon: "\u{F185}",
                        anim_marquee_phase: 0.0,
                    });

                pixels.fill(0x0000_0000);
                let is_dashboard = matches!(pst.kind, ActivePopover::Calendar { .. } | ActivePopover::None);
                let (cur_x, cur_w, cur_h, cur_r_corner, cur_r_ear) = if is_dashboard {
                    let t = pst.anim_dashboard_morph.clamp(0.0, 1.0);
                    let ease = 1.0 - (1.0 - t).powi(3);
                    let target_w = (w as f32 - 48.0).max(100.0);
                    let target_h = (h as f32 - 2.0).max(50.0);
                    let cur_w = pst.start_pill_w + (target_w - pst.start_pill_w) * ease;
                    let cur_h = pst.start_pill_h + (target_h - pst.start_pill_h) * ease;
                    let cur_x = ((w as f32) - cur_w) * 0.5;
                    let cur_r_corner = 14.0 + (22.0 - 14.0) * ease;
                    let cur_r_ear = 14.0 + (22.0 - 14.0) * ease;
                    (cur_x, cur_w, cur_h, cur_r_corner, cur_r_ear)
                } else {
                    (0.0, 0.0, 0.0, 0.0, 0.0)
                };

                if is_dashboard {
                    draw_aa_dashboard_notch_straight(
                        pixels,
                        w as usize,
                        w,
                        h,
                        cur_x,
                        cur_w,
                        cur_h,
                        cur_r_corner,
                        cur_r_ear,
                        rgb(18, 19, 24),
                        rgb(18, 19, 24),
                        1.0,
                        0.0,
                    );
                } else {
                    fill_aa_round_rect_f32(
                        pixels,
                        w as usize,
                        w,
                        h,
                        0.0,
                        0.0,
                        w as f32,
                        h as f32,
                        12.0,
                        rgb(32, 33, 38),
                        rgb(68, 70, 78),
                        1.0,
                        1.0,
                    );
                }

                let bg_pixels = if is_dashboard {
                    pixels.to_vec()
                } else {
                    Vec::new()
                };

                let old_bmp = SelectObject(mem_dc, dib_bmp);
                SetBkMode(mem_dc, TRANSPARENT);

                ensure_private_fonts_loaded();
                let font_hdr = create_apple_font(-17, 700);
                let font_grid = create_apple_font(-15, 500);
                let font_sec = create_apple_font(-14, 700);
                let font_menu_icon = create_icon_font(-16, 400);
                let font_large_clock = create_apple_font(-30, 700);
                let font_weather_temp = create_apple_font(-28, 700);
                let font_weather_icon = create_icon_font(-34, 400);
                let font_tab = create_apple_font(-16, 600);
                let font_tab_icon = create_icon_font(-19, 400);

                match pst.kind {
                    ActivePopover::Calendar { .. } | ActivePopover::None => {
                        let body_x = 24;
                        let _body_w = (w - 48).max(100);

                        // DASHBOARD (Centered vertically with tabs removed)
                        let card_bg = rgb(30, 31, 40);
                        let card_border = rgb(44, 46, 58);

                        // --- Top-Left Weather Card (x = body_x + 14, y = 44, w = 192, h = 100) ---
                        fill_aa_round_rect_f32(
                            pixels,
                            w as usize,
                            w,
                            h,
                            (body_x + 14) as f32,
                            44.0,
                            192.0,
                            100.0,
                            16.0,
                            card_bg,
                            card_border,
                            1.0,
                            0.95,
                        );
                        let mut w_ic = RECT {
                            left: body_x + 20,
                            top: 60,
                            right: body_x + 76,
                            bottom: 132,
                        };
                        draw_text_centered(mem_dc, pst.weather_icon, &mut w_ic, rgb(210, 214, 230), font_weather_icon);
                        let temp_str = format!("{}°C", pst.weather_temp_c);
                        let mut t_rc = RECT {
                            left: body_x + 80,
                            top: 52,
                            right: body_x + 196,
                            bottom: 90,
                        };
                        draw_text_left(mem_dc, &temp_str, &mut t_rc, rgb(238, 240, 248), font_weather_temp);
                        let mut nw_rc = RECT {
                            left: body_x + 82,
                            top: 92,
                            right: body_x + 196,
                            bottom: 114,
                        };
                        draw_text_left(mem_dc, &pst.weather_desc, &mut nw_rc, rgb(164, 168, 184), font_grid);
                        let mut loc_rc = RECT {
                            left: body_x + 82,
                            top: 114,
                            right: body_x + 196,
                            bottom: 134,
                        };
                        draw_text_left(mem_dc, &pst.weather_location, &mut loc_rc, rgb(148, 152, 168), font_sec);

                        // --- Top-Middle User & System Profile Card (x = body_x + 218, y = 44, w = 304, h = 100) ---
                        fill_aa_round_rect_f32(
                            pixels,
                            w as usize,
                            w,
                            h,
                            (body_x + 218) as f32,
                            44.0,
                            304.0,
                            100.0,
                            16.0,
                            card_bg,
                            card_border,
                            1.0,
                            0.95,
                        );
                        // Avatar Box
                        fill_aa_round_rect_f32(
                            pixels,
                            w as usize,
                            w,
                            h,
                            (body_x + 230) as f32,
                            54.0,
                            80.0,
                            80.0,
                            14.0,
                            rgb(38, 40, 50),
                            rgb(52, 54, 68),
                            1.0,
                            1.0,
                        );
                        // Silhouette Head & Shoulders
                        fill_aa_round_rect_f32(
                            pixels,
                            w as usize,
                            w,
                            h,
                            (body_x + 270 - 12) as f32,
                            68.0,
                            24.0,
                            24.0,
                            12.0,
                            rgb(230, 232, 240),
                            COLORREF(0),
                            0.0,
                            1.0,
                        );
                        fill_aa_round_rect_f32(
                            pixels,
                            w as usize,
                            w,
                            h,
                            (body_x + 270 - 24) as f32,
                            96.0,
                            48.0,
                            24.0,
                            12.0,
                            rgb(230, 232, 240),
                            COLORREF(0),
                            0.0,
                            1.0,
                        );
                        // 3 Info Rows
                        let mut r1 = RECT {
                            left: body_x + 322,
                            top: 54,
                            right: body_x + 512,
                            bottom: 76,
                        };
                        let os_label = format!("\u{F17A}  : {}", pst.os_version_label);
                        draw_text_marquee(mem_dc, &os_label, &mut r1, rgb(166, 172, 255), font_grid, pst.anim_marquee_phase);
                        let mut r2 = RECT {
                            left: body_x + 322,
                            top: 80,
                            right: body_x + 512,
                            bottom: 102,
                        };
                        draw_text_left(mem_dc, "\u{F2D0}  : Hyprland-Win", &mut r2, rgb(148, 178, 255), font_grid);

                        let uptime_sec = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() } / 1000;
                        let up_h = uptime_sec / 3600;
                        let up_m = (uptime_sec % 3600) / 60;
                        let up_str = format!("\u{F017}  : up {} hours, {} minutes", up_h, up_m);
                        let mut r3 = RECT {
                            left: body_x + 322,
                            top: 106,
                            right: body_x + 512,
                            bottom: 128,
                        };
                        draw_text_left(mem_dc, &up_str, &mut r3, rgb(242, 166, 212), font_grid);

                        // --- Bottom-Left Digital Clock Card (x = body_x + 14, y = 156, w = 86, h = 214) ---
                        fill_aa_round_rect_f32(
                            pixels,
                            w as usize,
                            w,
                            h,
                            (body_x + 14) as f32,
                            156.0,
                            86.0,
                            214.0,
                            16.0,
                            card_bg,
                            card_border,
                            1.0,
                            0.95,
                        );
                        let lt = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
                        let hr_str = format!("{:02}", lt.wHour);
                        let mut hr_rc = RECT {
                            left: body_x + 14,
                            top: 176,
                            right: body_x + 100,
                            bottom: 220,
                        };
                        draw_text_centered(mem_dc, &hr_str, &mut hr_rc, rgb(220, 224, 238), font_large_clock);

                        fill_aa_round_rect_f32(pixels, w as usize, w, h, (body_x + 57 - 12) as f32, 232.0, 4.0, 4.0, 2.0, rgb(200, 204, 220), COLORREF(0), 0.0, 1.0);
                        fill_aa_round_rect_f32(pixels, w as usize, w, h, (body_x + 57 - 2) as f32, 232.0, 4.0, 4.0, 2.0, rgb(166, 172, 255), COLORREF(0), 0.0, 1.0);
                        fill_aa_round_rect_f32(pixels, w as usize, w, h, (body_x + 57 + 8) as f32, 232.0, 4.0, 4.0, 2.0, rgb(200, 204, 220), COLORREF(0), 0.0, 1.0);

                        let min_str = format!("{:02}", lt.wMinute);
                        let mut min_rc = RECT {
                            left: body_x + 14,
                            top: 244,
                            right: body_x + 100,
                            bottom: 288,
                        };
                        draw_text_centered(mem_dc, &min_str, &mut min_rc, rgb(220, 224, 238), font_large_clock);

                        let dow_names = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
                        let dow_str = dow_names.get(lt.wDayOfWeek as usize).unwrap_or(&"");
                        let date_str = format!("{}, {}", dow_str, lt.wDay);
                        let mut dt_rc = RECT {
                            left: body_x + 14,
                            top: 310,
                            right: body_x + 100,
                            bottom: 336,
                        };
                        draw_text_centered(mem_dc, &date_str, &mut dt_rc, rgb(236, 138, 162), font_hdr);

                        // --- Bottom-Middle Monthly Calendar Card (x = body_x + 112, y = 156, w = 304, h = 214) ---
                        fill_aa_round_rect_f32(
                            pixels,
                            w as usize,
                            w,
                            h,
                            (body_x + 112) as f32,
                            156.0,
                            304.0,
                            214.0,
                            16.0,
                            card_bg,
                            card_border,
                            1.0,
                            0.95,
                        );
                        let mut p_rc = RECT {
                            left: body_x + 118,
                            top: 160,
                            right: body_x + 146,
                            bottom: 178,
                        };
                        draw_text_centered(mem_dc, "\u{F053}", &mut p_rc, rgb(164, 168, 184), font_grid);
                        let mut n_rc = RECT {
                            left: body_x + 382,
                            top: 160,
                            right: body_x + 410,
                            bottom: 178,
                        };
                        draw_text_centered(mem_dc, "\u{F054}", &mut n_rc, rgb(164, 168, 184), font_grid);

                        let month_names = ["", "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
                        let cur_month_name = month_names.get(pst.cal_month.clamp(1, 12) as usize).unwrap_or(&"");
                        let cal_hdr = format!("{} {}", cur_month_name, pst.cal_year);
                        let mut ch_rc = RECT {
                            left: body_x + 146,
                            top: 160,
                            right: body_x + 382,
                            bottom: 178,
                        };
                        draw_text_centered(mem_dc, &cal_hdr, &mut ch_rc, rgb(228, 230, 242), font_hdr);

                        let days_header = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
                        let col_w = 40;
                        let start_cal_x = body_x + 120;
                        let start_cal_y = 180;
                        for (c, name) in days_header.iter().enumerate() {
                            let mut d_rc = RECT {
                                left: start_cal_x + (c as i32) * col_w,
                                top: start_cal_y,
                                right: start_cal_x + ((c + 1) as i32) * col_w,
                                bottom: start_cal_y + 18,
                            };
                            draw_text_centered(mem_dc, name, &mut d_rc, rgb(152, 154, 164), font_grid);
                        }

                        let year = pst.cal_year;
                        let month = pst.cal_month.clamp(1, 12) as usize;
                        let is_leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
                        let days_in_month = match month {
                            2 => if is_leap { 29 } else { 28 },
                            4 | 6 | 9 | 11 => 30,
                            _ => 31,
                        };

                        let prev_month = if month == 1 { 12 } else { month - 1 };
                        let prev_year = if month == 1 { year - 1 } else { year };
                        let prev_is_leap = (prev_year % 4 == 0 && prev_year % 100 != 0) || (prev_year % 400 == 0);
                        let prev_days_in_month = match prev_month {
                            2 => if prev_is_leap { 29 } else { 28 },
                            4 | 6 | 9 | 11 => 30,
                            _ => 31,
                        };

                        let first_dow = day_of_week_gregorian(year, month as u32, 1) as i32;
                        let row_h = 26;
                        for i in 0..42 {
                            let r = i / 7;
                            let c = i % 7;
                            let cell_x = start_cal_x + c * col_w;
                            let cell_y = start_cal_y + 18 + r * row_h;
                            let mut cell_rc = RECT {
                                left: cell_x,
                                top: cell_y,
                                right: cell_x + col_w,
                                bottom: cell_y + row_h,
                            };

                            if i < first_dow {
                                let d = prev_days_in_month - first_dow + 1 + i;
                                let s = format!("{d}");
                                draw_text_centered(mem_dc, &s, &mut cell_rc, rgb(90, 94, 110), font_grid);
                            } else if i < first_dow + days_in_month {
                                let d = i - first_dow + 1;
                                let is_today = year == pst.today_year
                                    && (month as u32) == pst.today_month
                                    && (d as u32) == pst.today_day;

                                if is_today {
                                    fill_aa_round_rect_f32(
                                        pixels,
                                        w as usize,
                                        w,
                                        h,
                                        (cell_x + (col_w - 24) / 2) as f32,
                                        (cell_y + (row_h - 24) / 2) as f32,
                                        24.0,
                                        24.0,
                                        12.0,
                                        rgb(166, 172, 255),
                                        COLORREF(0),
                                        0.0,
                                        1.0,
                                    );
                                    let s = format!("{d}");
                                    draw_text_centered(mem_dc, &s, &mut cell_rc, rgb(20, 21, 28), font_hdr);
                                } else {
                                    let s = format!("{d}");
                                    draw_text_centered(mem_dc, &s, &mut cell_rc, rgb(214, 218, 232), font_grid);
                                }
                            } else {
                                let d = i - (first_dow + days_in_month) + 1;
                                let s = format!("{d}");
                                draw_text_centered(mem_dc, &s, &mut cell_rc, rgb(90, 94, 110), font_grid);
                            }
                        }

                        // --- Bottom-Right-Middle 2 Vertical Sliders Card (x = body_x + 428, y = 156, w = 94, h = 214) ---
                        fill_aa_round_rect_f32(
                            pixels,
                            w as usize,
                            w,
                            h,
                            (body_x + 428) as f32,
                            156.0,
                            94.0,
                            214.0,
                            16.0,
                            card_bg,
                            card_border,
                            1.0,
                            0.95,
                        );

                        // Track 1: Master Volume (centered at body_x + 452)
                        let vol_pct = pst.volume_pct.min(100);
                        fill_aa_round_rect_f32(pixels, w as usize, w, h, (body_x + 452) as f32, 172.0, 10.0, 142.0, 5.0, rgb(20, 21, 27), COLORREF(0), 0.0, 1.0);
                        let vol_fill_h = (142 * vol_pct as i32) / 100;
                        if vol_fill_h > 0 {
                            fill_aa_round_rect_f32(pixels, w as usize, w, h, (body_x + 452) as f32, (314 - vol_fill_h) as f32, 10.0, vol_fill_h as f32, 5.0, rgb(180, 190, 254), COLORREF(0), 0.0, 1.0);
                        }
                        let mut ic1 = RECT { left: body_x + 442, top: 328, right: body_x + 472, bottom: 356 };
                        let vol_glyph = if pst.volume_muted || vol_pct == 0 {
                            "\u{EEE8}"
                        } else if vol_pct < 35 {
                            "\u{F026}"
                        } else if vol_pct < 70 {
                            "\u{F027}"
                        } else {
                            "\u{F028}"
                        };
                        draw_text_centered(mem_dc, vol_glyph, &mut ic1, rgb(180, 190, 254), font_hdr);

                        // Track 2: Mic Volume (centered at body_x + 488)
                        let mic_pct = pst.mic_volume_pct.min(100);
                        fill_aa_round_rect_f32(pixels, w as usize, w, h, (body_x + 488) as f32, 172.0, 10.0, 142.0, 5.0, rgb(20, 21, 27), COLORREF(0), 0.0, 1.0);
                        let mic_fill_h = (142 * mic_pct as i32) / 100;
                        if mic_fill_h > 0 {
                            fill_aa_round_rect_f32(pixels, w as usize, w, h, (body_x + 488) as f32, (314 - mic_fill_h) as f32, 10.0, mic_fill_h as f32, 5.0, rgb(242, 166, 212), COLORREF(0), 0.0, 1.0);
                        }
                        let mut ic2 = RECT { left: body_x + 478, top: 328, right: body_x + 508, bottom: 356 };
                        draw_text_centered(mem_dc, "\u{F130}", &mut ic2, rgb(242, 166, 212), font_hdr);

                        // --- Far-Right Tall Media Player Card (x = body_x + 534, y = 44, w = 184, h = 326) ---
                        fill_aa_round_rect_f32(
                            pixels,
                            w as usize,
                            w,
                            h,
                            (body_x + 534) as f32,
                            44.0,
                            184.0,
                            326.0,
                            16.0,
                            card_bg,
                            card_border,
                            1.0,
                            0.95,
                        );

                        let disc_cx = body_x + 626;
                        let disc_cy = 118;
                        // Album disc with dusk/mountain gradient vinyl rings
                        fill_aa_round_rect_f32(pixels, w as usize, w, h, (disc_cx - 52) as f32, (disc_cy - 52) as f32, 104.0, 104.0, 52.0, rgb(36, 38, 52), rgb(64, 68, 88), 1.0, 1.0);
                        fill_aa_round_rect_f32(pixels, w as usize, w, h, (disc_cx - 40) as f32, (disc_cy - 40) as f32, 80.0, 80.0, 40.0, rgb(48, 44, 68), rgb(72, 70, 96), 1.0, 1.0);
                        fill_aa_round_rect_f32(pixels, w as usize, w, h, (disc_cx - 24) as f32, (disc_cy - 24) as f32, 48.0, 48.0, 24.0, rgb(72, 54, 88), rgb(110, 80, 130), 1.0, 1.0);
                        fill_aa_round_rect_f32(pixels, w as usize, w, h, (disc_cx - 8) as f32, (disc_cy - 8) as f32, 16.0, 16.0, 8.0, rgb(20, 21, 28), rgb(166, 172, 255), 1.5, 1.0);

                        // Circular Progress Arc (radius 53.0, thickness 3.5, start 270.0)
                        let progress = if pst.media_status == MediaPlaybackStatus::Stopped
                            || pst.media_title.is_empty()
                            || pst.media_title == "No Media Playing"
                        {
                            0.0
                        } else {
                            (pst.media_position_sec as f32 / pst.media_duration_sec.max(1) as f32).clamp(0.03, 1.0)
                        };
                        draw_aa_progress_arc(pixels, w as usize, w, h, disc_cx as f32, disc_cy as f32, 53.0, 3.5, 270.0, progress * 360.0, rgb(166, 172, 255), 1.0);

                        // Track Info with Marquee Scrolling
                        let m_title = if pst.media_title.is_empty() { "No Media Playing" } else { &pst.media_title };
                        let mut t_rc = RECT { left: body_x + 538, top: 182, right: body_x + 714, bottom: 204 };
                        draw_text_marquee(mem_dc, m_title, &mut t_rc, rgb(180, 190, 254), font_hdr, pst.anim_marquee_phase);

                        let m_album = if pst.media_album.is_empty() { "System Audio" } else { &pst.media_album };
                        let mut a_rc = RECT { left: body_x + 538, top: 204, right: body_x + 714, bottom: 224 };
                        draw_text_marquee(mem_dc, m_album, &mut a_rc, rgb(148, 152, 168), font_grid, pst.anim_marquee_phase);

                        let m_artist = if pst.media_artist.is_empty() { "Hyprland-Win Media" } else { &pst.media_artist };
                        let mut art_rc = RECT { left: body_x + 538, top: 224, right: body_x + 714, bottom: 244 };
                        draw_text_marquee(mem_dc, m_artist, &mut art_rc, rgb(148, 152, 168), font_grid, pst.anim_marquee_phase);

                        // Playback transport buttons
                        let mut p_btn = RECT { left: disc_cx - 48, top: 250, right: disc_cx - 20, bottom: 276 };
                        draw_text_centered(mem_dc, "\u{F048}", &mut p_btn, rgb(230, 232, 242), font_hdr);
                        let pl_icon = if pst.media_status == MediaPlaybackStatus::Playing { "\u{F04C}" } else { "\u{F04B}" };
                        let mut pl_btn = RECT { left: disc_cx - 14, top: 250, right: disc_cx + 14, bottom: 276 };
                        draw_text_centered(mem_dc, pl_icon, &mut pl_btn, rgb(230, 232, 242), font_hdr);
                        let mut n_btn = RECT { left: disc_cx + 20, top: 250, right: disc_cx + 48, bottom: 276 };
                        draw_text_centered(mem_dc, "\u{F051}", &mut n_btn, rgb(230, 232, 242), font_hdr);

                        // Bongo Cat at bottom
                        draw_aa_bongo_cat(pixels, w as usize, w, h, disc_cx as f32, 325.0);
                    }
                    ActivePopover::Volume => {
                        // 1. Output Header + Mute Button
                        let vol_label = if pst.volume_muted {
                            format!("\u{F026}  Output: Muted ({}%)", pst.volume_pct)
                        } else {
                            format!("\u{F028}  Output: {}%", pst.volume_pct)
                        };
                        let mut title_rc = RECT {
                            left: 16,
                            top: 12,
                            right: w - 76,
                            bottom: 36,
                        };
                        draw_text_left(mem_dc, &vol_label, &mut title_rc, rgb(228, 230, 235), font_hdr);

                        let mute_bg = if pst.volume_muted {
                            rgb(235, 106, 102)
                        } else {
                            rgb(46, 48, 55)
                        };
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            w - 72,
                            12,
                            58,
                            24,
                            6.0,
                            mute_bg,
                            rgb(64, 66, 76),
                            1.0,
                            1.0,
                        );
                        let mut mute_rc = RECT {
                            left: w - 72,
                            top: 12,
                            right: w - 14,
                            bottom: 36,
                        };
                        let mute_txt = if pst.volume_muted { "Unmute" } else { "Mute" };
                        draw_text_centered(mem_dc, mute_txt, &mut mute_rc, rgb(228, 230, 235), font_grid);

                        // 2. Output Slider Track
                        let track_x = 18;
                        let track_y = 44;
                        let track_w = w - 36;
                        let track_h = 8;
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            track_x,
                            track_y,
                            track_w,
                            track_h,
                            4.0,
                            rgb(46, 48, 55),
                            COLORREF(0),
                            0.0,
                            1.0,
                        );
                        let fill_w = ((track_w as u32 * pst.volume_pct.min(100)) / 100) as i32;
                        let fill_col = if pst.volume_muted {
                            rgb(113, 113, 122)
                        } else {
                            rgb(94, 158, 248)
                        };
                        if fill_w > 0 {
                            fill_aa_round_rect(
                                pixels,
                                w as usize,
                                w,
                                h,
                                track_x,
                                track_y,
                                fill_w,
                                track_h,
                                4.0,
                                fill_col,
                                COLORREF(0),
                                0.0,
                                1.0,
                            );
                        }
                        let thumb_x = (track_x + fill_w - 7).clamp(track_x - 4, track_x + track_w - 10);
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            thumb_x,
                            track_y - 4,
                            16,
                            16,
                            8.0,
                            rgb(228, 230, 235),
                            rgb(32, 33, 38),
                            1.5,
                            1.0,
                        );

                        // 3. Output Devices Label & List
                        let mut out_lbl_rc = RECT {
                            left: 16,
                            top: 60,
                            right: w - 16,
                            bottom: 78,
                        };
                        draw_text_left(
                            mem_dc,
                            "OUTPUT DEVICES",
                            &mut out_lbl_rc,
                            rgb(152, 154, 164),
                            font_sec,
                        );

                        let out_count = pst.audio_outputs.len().clamp(1, 5) as i32;
                        if pst.audio_outputs.is_empty() {
                            let row_y = 80;
                            fill_aa_round_rect(
                                pixels,
                                w as usize,
                                w,
                                h,
                                14,
                                row_y,
                                w - 28,
                                25,
                                6.0,
                                rgb(46, 48, 55),
                                COLORREF(0),
                                0.0,
                                1.0,
                            );
                            let mut empty_rc = RECT {
                                left: 20,
                                top: row_y,
                                right: w - 20,
                                bottom: row_y + 25,
                            };
                            draw_text_left(
                                mem_dc,
                                "Default Audio Output",
                                &mut empty_rc,
                                rgb(152, 154, 164),
                                font_grid,
                            );
                        } else {
                            for (i, dev) in pst.audio_outputs.iter().take(5).enumerate() {
                                let row_y = 80 + (i as i32) * 28;
                                let (bg_col, border_col, border_w) = if dev.is_default {
                                    (rgb(54, 57, 67), rgb(94, 158, 248), 1.0)
                                } else {
                                    (rgb(46, 48, 55), COLORREF(0), 0.0)
                                };
                                fill_aa_round_rect(
                                    pixels,
                                    w as usize,
                                    w,
                                    h,
                                    14,
                                    row_y,
                                    w - 28,
                                    25,
                                    6.0,
                                    bg_col,
                                    border_col,
                                    border_w,
                                    1.0,
                                );
                                let mut icon_rc = RECT {
                                    left: 18,
                                    top: row_y,
                                    right: 36,
                                    bottom: row_y + 25,
                                };
                                let (radio_glyph, radio_col) = if dev.is_default {
                                    ("\u{F00C}", rgb(94, 158, 248))
                                } else {
                                    ("\u{F10C}", rgb(113, 113, 122))
                                };
                                draw_text_centered(mem_dc, radio_glyph, &mut icon_rc, radio_col, font_grid);

                                let mut name_rc = RECT {
                                    left: 38,
                                    top: row_y,
                                    right: w - 20,
                                    bottom: row_y + 25,
                                };
                                let name_col = if dev.is_default {
                                    rgb(228, 230, 235)
                                } else {
                                    rgb(186, 188, 198)
                                };
                                draw_text_left(mem_dc, &dev.name, &mut name_rc, name_col, font_grid);
                            }
                        }

                        // 4. Microphone Section
                        let out_end_y = 80 + out_count * 28;
                        let mic_sec_y = out_end_y + 10;

                        let mic_label = if pst.mic_muted {
                            format!("\u{F131}  Input: Muted ({}%)", pst.mic_volume_pct)
                        } else {
                            format!("\u{F130}  Input: {}%", pst.mic_volume_pct)
                        };
                        let mut mic_title_rc = RECT {
                            left: 16,
                            top: mic_sec_y,
                            right: w - 76,
                            bottom: mic_sec_y + 24,
                        };
                        draw_text_left(
                            mem_dc,
                            &mic_label,
                            &mut mic_title_rc,
                            rgb(228, 230, 235),
                            font_hdr,
                        );

                        let mic_mute_bg = if pst.mic_muted {
                            rgb(235, 106, 102)
                        } else {
                            rgb(46, 48, 55)
                        };
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            w - 72,
                            mic_sec_y,
                            58,
                            24,
                            6.0,
                            mic_mute_bg,
                            rgb(64, 66, 76),
                            1.0,
                            1.0,
                        );
                        let mut mic_mute_rc = RECT {
                            left: w - 72,
                            top: mic_sec_y,
                            right: w - 14,
                            bottom: mic_sec_y + 24,
                        };
                        let mic_mute_txt = if pst.mic_muted { "Unmute" } else { "Mute" };
                        draw_text_centered(
                            mem_dc,
                            mic_mute_txt,
                            &mut mic_mute_rc,
                            rgb(228, 230, 235),
                            font_grid,
                        );

                        // Mic Volume Slider
                        let mic_track_y = mic_sec_y + 32;
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            track_x,
                            mic_track_y,
                            track_w,
                            track_h,
                            4.0,
                            rgb(46, 48, 55),
                            COLORREF(0),
                            0.0,
                            1.0,
                        );
                        let mic_fill_w = ((track_w as u32 * pst.mic_volume_pct.min(100)) / 100) as i32;
                        let mic_fill_col = if pst.mic_muted {
                            rgb(113, 113, 122)
                        } else {
                            rgb(94, 158, 248)
                        };
                        if mic_fill_w > 0 {
                            fill_aa_round_rect(
                                pixels,
                                w as usize,
                                w,
                                h,
                                track_x,
                                mic_track_y,
                                mic_fill_w,
                                track_h,
                                4.0,
                                mic_fill_col,
                                COLORREF(0),
                                0.0,
                                1.0,
                            );
                        }
                        let mic_thumb_x = (track_x + mic_fill_w - 7).clamp(track_x - 4, track_x + track_w - 10);
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            mic_thumb_x,
                            mic_track_y - 4,
                            16,
                            16,
                            8.0,
                            rgb(228, 230, 235),
                            rgb(32, 33, 38),
                            1.5,
                            1.0,
                        );

                        // 5. Input Devices Label & List
                        let in_label_y = mic_sec_y + 48;
                        let mut in_lbl_rc = RECT {
                            left: 16,
                            top: in_label_y,
                            right: w - 16,
                            bottom: in_label_y + 16,
                        };
                        draw_text_left(
                            mem_dc,
                            "INPUT DEVICES",
                            &mut in_lbl_rc,
                            rgb(152, 154, 164),
                            font_sec,
                        );

                        let in_dev_start_y = mic_sec_y + 66;
                        let in_count = pst.audio_inputs.len().clamp(1, 4) as i32;
                        if pst.audio_inputs.is_empty() {
                            let row_y = in_dev_start_y;
                            fill_aa_round_rect(
                                pixels,
                                w as usize,
                                w,
                                h,
                                14,
                                row_y,
                                w - 28,
                                25,
                                6.0,
                                rgb(46, 48, 55),
                                COLORREF(0),
                                0.0,
                                1.0,
                            );
                            let mut empty_rc = RECT {
                                left: 20,
                                top: row_y,
                                right: w - 20,
                                bottom: row_y + 25,
                            };
                            draw_text_left(
                                mem_dc,
                                "Default Microphone",
                                &mut empty_rc,
                                rgb(152, 154, 164),
                                font_grid,
                            );
                        } else {
                            for (i, dev) in pst.audio_inputs.iter().take(4).enumerate() {
                                let row_y = in_dev_start_y + (i as i32) * 28;
                                let (bg_col, border_col, border_w) = if dev.is_default {
                                    (rgb(54, 57, 67), rgb(94, 158, 248), 1.0)
                                } else {
                                    (rgb(46, 48, 55), COLORREF(0), 0.0)
                                };
                                fill_aa_round_rect(
                                    pixels,
                                    w as usize,
                                    w,
                                    h,
                                    14,
                                    row_y,
                                    w - 28,
                                    25,
                                    6.0,
                                    bg_col,
                                    border_col,
                                    border_w,
                                    1.0,
                                );
                                let mut icon_rc = RECT {
                                    left: 18,
                                    top: row_y,
                                    right: 36,
                                    bottom: row_y + 25,
                                };
                                let (radio_glyph, radio_col) = if dev.is_default {
                                    ("\u{F00C}", rgb(94, 158, 248))
                                } else {
                                    ("\u{F10C}", rgb(113, 113, 122))
                                };
                                draw_text_centered(mem_dc, radio_glyph, &mut icon_rc, radio_col, font_grid);

                                let mut name_rc = RECT {
                                    left: 38,
                                    top: row_y,
                                    right: w - 20,
                                    bottom: row_y + 25,
                                };
                                let name_col = if dev.is_default {
                                    rgb(228, 230, 235)
                                } else {
                                    rgb(186, 188, 198)
                                };
                                draw_text_left(mem_dc, &dev.name, &mut name_rc, name_col, font_grid);
                            }
                        }

                        // 6. Bottom Action Buttons: [Volume Mixer] & [Sound Settings]
                        let snd_btn_y = in_dev_start_y + in_count * 28 + 10;
                        let btn_w = (w - 36) / 2;
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            14,
                            snd_btn_y,
                            btn_w,
                            30,
                            7.0,
                            rgb(46, 48, 55),
                            rgb(64, 66, 76),
                            1.0,
                            1.0,
                        );
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            22 + btn_w,
                            snd_btn_y,
                            btn_w,
                            30,
                            7.0,
                            rgb(46, 48, 55),
                            rgb(64, 66, 76),
                            1.0,
                            1.0,
                        );
                        let mut b1_rc = RECT {
                            left: 14,
                            top: snd_btn_y,
                            right: 14 + btn_w,
                            bottom: snd_btn_y + 30,
                        };
                        let mut b2_rc = RECT {
                            left: 22 + btn_w,
                            top: snd_btn_y,
                            right: 22 + btn_w * 2,
                            bottom: snd_btn_y + 30,
                        };
                        draw_text_centered(
                            mem_dc,
                            "Volume Mixer",
                            &mut b1_rc,
                            rgb(186, 188, 198),
                            font_grid,
                        );
                        draw_text_centered(
                            mem_dc,
                            "Sound Settings",
                            &mut b2_rc,
                            rgb(186, 188, 198),
                            font_grid,
                        );
                    }
                    ActivePopover::Bluetooth => {
                        // 1. Header: Title + Scan button + Power Toggle
                        let mut title_rc = RECT {
                            left: 16,
                            top: 12,
                            right: w - 120,
                            bottom: 36,
                        };
                        draw_text_left(
                            mem_dc,
                            "\u{F294}  Bluetooth",
                            &mut title_rc,
                            rgb(228, 230, 235),
                            font_hdr,
                        );

                        // Scan Button
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            w - 114,
                            12,
                            48,
                            24,
                            6.0,
                            rgb(46, 48, 55),
                            rgb(64, 66, 76),
                            1.0,
                            1.0,
                        );
                        let mut scan_rc = RECT {
                            left: w - 114,
                            top: 12,
                            right: w - 66,
                            bottom: 36,
                        };
                        draw_text_centered(
                            mem_dc,
                            "\u{F021} Scan",
                            &mut scan_rc,
                            rgb(186, 188, 198),
                            font_sec,
                        );

                        // Power Toggle Button
                        let bt_power_bg = if pst.bluetooth_enabled {
                            rgb(94, 158, 248)
                        } else {
                            rgb(46, 48, 55)
                        };
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            w - 60,
                            12,
                            46,
                            24,
                            6.0,
                            bt_power_bg,
                            rgb(64, 66, 76),
                            1.0,
                            1.0,
                        );
                        let mut pwr_rc = RECT {
                            left: w - 60,
                            top: 12,
                            right: w - 14,
                            bottom: 36,
                        };
                        let pwr_txt = if pst.bluetooth_enabled { "On" } else { "Off" };
                        draw_text_centered(
                            mem_dc,
                            pwr_txt,
                            &mut pwr_rc,
                            rgb(228, 230, 235),
                            font_grid,
                        );

                        // 2. Section Label
                        let mut sec_rc = RECT {
                            left: 16,
                            top: 44,
                            right: w - 16,
                            bottom: 62,
                        };
                        let sec_title = format!("PAIRED DEVICES ({})", pst.bluetooth_devices.len());
                        draw_text_left(
                            mem_dc,
                            &sec_title,
                            &mut sec_rc,
                            rgb(152, 154, 164),
                            font_sec,
                        );

                        // 3. Bluetooth Device Rows
                        let bt_count = pst.bluetooth_devices.len().clamp(1, 6) as i32;
                        if pst.bluetooth_devices.is_empty() {
                            let row_y = 66;
                            fill_aa_round_rect(
                                pixels,
                                w as usize,
                                w,
                                h,
                                14,
                                row_y,
                                w - 28,
                                32,
                                7.0,
                                rgb(46, 48, 55),
                                COLORREF(0),
                                0.0,
                                1.0,
                            );
                            let mut empty_rc = RECT {
                                left: 20,
                                top: row_y,
                                right: w - 20,
                                bottom: row_y + 32,
                            };
                            draw_text_left(
                                mem_dc,
                                "No Bluetooth devices found",
                                &mut empty_rc,
                                rgb(152, 154, 164),
                                font_grid,
                            );
                        } else {
                            for (i, dev) in pst.bluetooth_devices.iter().take(6).enumerate() {
                                let row_y = 66 + (i as i32) * 36;
                                let (bg_col, border_col, border_w) = if dev.connected {
                                    (rgb(54, 57, 67), rgb(94, 158, 248), 1.0)
                                } else {
                                    (rgb(46, 48, 55), COLORREF(0), 0.0)
                                };
                                fill_aa_round_rect(
                                    pixels,
                                    w as usize,
                                    w,
                                    h,
                                    14,
                                    row_y,
                                    w - 28,
                                    32,
                                    7.0,
                                    bg_col,
                                    border_col,
                                    border_w,
                                    1.0,
                                );

                                // Category Icon
                                let icon_glyph = match dev.category {
                                    "audio" => "\u{F025}",
                                    "input" => "\u{F11C}",
                                    "phone" => "\u{F10B}",
                                    _ => "\u{F294}",
                                };
                                let mut icon_rc = RECT {
                                    left: 18,
                                    top: row_y,
                                    right: 42,
                                    bottom: row_y + 32,
                                };
                                let icon_col = if dev.connected {
                                    rgb(94, 158, 248)
                                } else {
                                    rgb(152, 154, 164)
                                };
                                draw_text_centered(mem_dc, icon_glyph, &mut icon_rc, icon_col, font_hdr);

                                // Device Name
                                let mut name_rc = RECT {
                                    left: 44,
                                    top: row_y,
                                    right: w - 88,
                                    bottom: row_y + 32,
                                };
                                let name_col = if dev.connected {
                                    rgb(228, 230, 235)
                                } else {
                                    rgb(186, 188, 198)
                                };
                                draw_text_left(mem_dc, &dev.name, &mut name_rc, name_col, font_grid);

                                // Status Badge Pill
                                let (badge_bg, badge_border, badge_txt, badge_col) = if dev.connected {
                                    (
                                        rgb(26, 52, 38),
                                        rgb(82, 196, 126),
                                        "Connected",
                                        rgb(134, 239, 172),
                                    )
                                } else {
                                    (
                                        rgb(46, 48, 55),
                                        rgb(64, 66, 76),
                                        "Connect",
                                        rgb(152, 154, 164),
                                    )
                                };
                                fill_aa_round_rect(
                                    pixels,
                                    w as usize,
                                    w,
                                    h,
                                    w - 84,
                                    row_y + 6,
                                    66,
                                    20,
                                    5.0,
                                    badge_bg,
                                    badge_border,
                                    1.0,
                                    1.0,
                                );
                                let mut badge_rc = RECT {
                                    left: w - 84,
                                    top: row_y + 6,
                                    right: w - 18,
                                    bottom: row_y + 26,
                                };
                                draw_text_centered(mem_dc, badge_txt, &mut badge_rc, badge_col, font_sec);
                            }
                        }

                        // 4. Bottom Action Buttons: [Add Device] & [More Options]
                        let bt_btn_y = 66 + bt_count * 36 + 10;
                        let btn_w = (w - 36) / 2;
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            14,
                            bt_btn_y,
                            btn_w,
                            30,
                            7.0,
                            rgb(46, 48, 55),
                            rgb(64, 66, 76),
                            1.0,
                            1.0,
                        );
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            22 + btn_w,
                            bt_btn_y,
                            btn_w,
                            30,
                            7.0,
                            rgb(46, 48, 55),
                            rgb(64, 66, 76),
                            1.0,
                            1.0,
                        );
                        let mut b1_rc = RECT {
                            left: 14,
                            top: bt_btn_y,
                            right: 14 + btn_w,
                            bottom: bt_btn_y + 30,
                        };
                        let mut b2_rc = RECT {
                            left: 22 + btn_w,
                            top: bt_btn_y,
                            right: 22 + btn_w * 2,
                            bottom: bt_btn_y + 30,
                        };
                        draw_text_centered(
                            mem_dc,
                            "Add Device",
                            &mut b1_rc,
                            rgb(186, 188, 198),
                            font_grid,
                        );
                        draw_text_centered(
                            mem_dc,
                            "More Options",
                            &mut b2_rc,
                            rgb(186, 188, 198),
                            font_grid,
                        );
                    }
                    ActivePopover::Network => {
                        // Card 1: Network Status
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            14,
                            12,
                            w - 28,
                            46,
                            8.0,
                            rgb(46, 48, 55),
                            rgb(64, 66, 76),
                            1.0,
                            1.0,
                        );
                        let net_title = match pst.network_kind {
                            NetworkKind::Wifi => {
                                format!("\u{F1EB}  Wi-Fi ({} Mbps)", pst.network_speed_mbps)
                            }
                            NetworkKind::Ethernet => {
                                format!("\u{F0AC}  Ethernet ({} Mbps)", pst.network_speed_mbps)
                            }
                            NetworkKind::Disconnected => "\u{F127}  Disconnected".to_string(),
                        };
                        let mut nt_rc = RECT {
                            left: 24,
                            top: 14,
                            right: w - 24,
                            bottom: 34,
                        };
                        draw_text_centered(
                            mem_dc,
                            &net_title,
                            &mut nt_rc,
                            rgb(228, 230, 235),
                            font_hdr,
                        );
                        let mut na_rc = RECT {
                            left: 24,
                            top: 34,
                            right: w - 24,
                            bottom: 54,
                        };
                        draw_text_centered(
                            mem_dc,
                            &pst.network_adapter,
                            &mut na_rc,
                            rgb(152, 154, 164),
                            font_grid,
                        );

                        // Card 2: Bluetooth Toggle Row
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            14,
                            66,
                            w - 28,
                            38,
                            8.0,
                            rgb(46, 48, 55),
                            rgb(64, 66, 76),
                            1.0,
                            1.0,
                        );
                        let bt_txt = if pst.bluetooth_enabled {
                            "\u{F294}  Bluetooth: Enabled (Click to toggle)"
                        } else {
                            "\u{F294}  Bluetooth: Off (Click to toggle)"
                        };
                        let bt_col = if pst.bluetooth_enabled {
                            rgb(228, 230, 235)
                        } else {
                            rgb(152, 154, 164)
                        };
                        let mut bt_rc = RECT {
                            left: 24,
                            top: 66,
                            right: w - 24,
                            bottom: 104,
                        };
                        draw_text_centered(mem_dc, bt_txt, &mut bt_rc, bt_col, font_grid);

                        // Bottom Buttons: [Network Settings] & [Bluetooth Settings]
                        let btn_w = (w - 36) / 2;
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            14,
                            116,
                            btn_w,
                            32,
                            7.0,
                            rgb(46, 48, 55),
                            rgb(64, 66, 76),
                            1.0,
                            1.0,
                        );
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            22 + btn_w,
                            116,
                            btn_w,
                            32,
                            7.0,
                            rgb(46, 48, 55),
                            rgb(64, 66, 76),
                            1.0,
                            1.0,
                        );
                        let mut b1_rc = RECT {
                            left: 14,
                            top: 116,
                            right: 14 + btn_w,
                            bottom: 148,
                        };
                        let mut b2_rc = RECT {
                            left: 22 + btn_w,
                            top: 116,
                            right: 22 + btn_w * 2,
                            bottom: 148,
                        };
                        draw_text_centered(
                            mem_dc,
                            "Network Settings",
                            &mut b1_rc,
                            rgb(186, 188, 198),
                            font_grid,
                        );
                        draw_text_centered(
                            mem_dc,
                            "Bluetooth Settings",
                            &mut b2_rc,
                            rgb(186, 188, 198),
                            font_grid,
                        );
                    }
                    ActivePopover::SystemMenu => {
                        let hovered_idx = pst.menu_hovered_idx;
                        let mut cur_y = 6;
                        for item in SYSTEM_MENU_ITEMS {
                            match item {
                                SystemMenuItem::Action { id, icon, label } => {
                                    let is_hovered = hovered_idx == Some(*id);
                                    if is_hovered {
                                        fill_aa_round_rect_f32(
                                            pixels,
                                            w as usize,
                                            w,
                                            h,
                                            6.0,
                                            cur_y as f32,
                                            (w - 12) as f32,
                                            28.0,
                                            6.0,
                                            rgb(10, 132, 255),
                                            COLORREF(0),
                                            0.0,
                                            1.0,
                                        );
                                    }
                                    let text_color = if is_hovered {
                                        rgb(255, 255, 255)
                                    } else {
                                        rgb(228, 230, 236)
                                    };

                                    draw_icon_ink_centered(
                                        pixels,
                                        w as usize,
                                        w,
                                        h,
                                        icon,
                                        (12, cur_y, 22, 28),
                                        text_color,
                                        font_menu_icon,
                                    );

                                    let mut label_rc = RECT {
                                        left: 38,
                                        top: cur_y,
                                        right: w - 12,
                                        bottom: cur_y + 28,
                                    };
                                    draw_text_left(mem_dc, label, &mut label_rc, text_color, font_grid);

                                    cur_y += 28;
                                }
                                SystemMenuItem::Separator => {
                                    fill_aa_round_rect_f32(
                                        pixels,
                                        w as usize,
                                        w,
                                        h,
                                        12.0,
                                        (cur_y + 4) as f32,
                                        (w - 24) as f32,
                                        1.0,
                                        0.5,
                                        rgb(54, 56, 64),
                                        COLORREF(0),
                                        0.0,
                                        0.8,
                                    );
                                    cur_y += 9;
                                }
                            }
                        }
                    }
                    ActivePopover::AboutHyprlandWin => {
                        // Header icon \u{F17A} + Title "Hyprland-Win Compositor"
                        let mut title_rc = RECT {
                            left: 18,
                            top: 14,
                            right: w - 90,
                            bottom: 36,
                        };
                        draw_text_left(
                            mem_dc,
                            "\u{F17A}  Hyprland-Win Compositor",
                            &mut title_rc,
                            rgb(228, 230, 235),
                            font_hdr,
                        );

                        // Version badge pill "v0.2.0"
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            w - 82,
                            14,
                            64,
                            22,
                            5.0,
                            rgb(46, 48, 55),
                            rgb(64, 66, 76),
                            1.0,
                            1.0,
                        );
                        let mut badge_rc = RECT {
                            left: w - 82,
                            top: 14,
                            right: w - 18,
                            bottom: 36,
                        };
                        draw_text_centered(
                            mem_dc,
                            "v0.2.0",
                            &mut badge_rc,
                            rgb(82, 196, 126),
                            font_sec,
                        );

                        // Subtitle
                        let mut sub_rc = RECT {
                            left: 18,
                            top: 38,
                            right: w - 18,
                            bottom: 56,
                        };
                        draw_text_left(
                            mem_dc,
                            "Native Windows 11 Tiling Compositor",
                            &mut sub_rc,
                            rgb(152, 154, 164),
                            font_grid,
                        );

                        // 1px divider at y: 62
                        if h > 63 {
                            for x in 18..(w - 18) {
                                pixels[(62 * w + x) as usize] = 0xFF36_3840;
                            }
                        }

                        // Specs Card (y: 68..156)
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            14,
                            68,
                            w - 28,
                            88,
                            7.0,
                            rgb(46, 48, 55),
                            rgb(64, 66, 76),
                            1.0,
                            1.0,
                        );
                        let mut s1_rc = RECT {
                            left: 24,
                            top: 74,
                            right: w - 24,
                            bottom: 96,
                        };
                        draw_text_left(
                            mem_dc,
                            "Architecture: x86_64 Native (Win32 + DWM)",
                            &mut s1_rc,
                            rgb(186, 188, 198),
                            font_grid,
                        );
                        let mut s2_rc = RECT {
                            left: 24,
                            top: 98,
                            right: w - 24,
                            bottom: 120,
                        };
                        draw_text_left(
                            mem_dc,
                            "Renderer: Software Layered DIB / DirectComposition",
                            &mut s2_rc,
                            rgb(186, 188, 198),
                            font_grid,
                        );
                        let mut s3_rc = RECT {
                            left: 24,
                            top: 122,
                            right: w - 24,
                            bottom: 144,
                        };
                        draw_text_left(
                            mem_dc,
                            "Shell: HyprBar + HyprLock + Hyprland-Win Core",
                            &mut s3_rc,
                            rgb(186, 188, 198),
                            font_grid,
                        );

                        // Bottom Close pill button (y: 166..196)
                        fill_aa_round_rect(
                            pixels,
                            w as usize,
                            w,
                            h,
                            14,
                            166,
                            w - 28,
                            30,
                            6.0,
                            rgb(46, 48, 55),
                            rgb(64, 66, 76),
                            1.0,
                            1.0,
                        );
                        let mut close_rc = RECT {
                            left: 14,
                            top: 166,
                            right: w - 14,
                            bottom: 196,
                        };
                        draw_text_centered(
                            mem_dc,
                            "Close",
                            &mut close_rc,
                            rgb(244, 244, 245),
                            font_hdr,
                        );
                    }
                }

                if is_dashboard {
                    let t = pst.anim_dashboard_morph.clamp(0.0, 1.0);
                    // Seamless Title Disappear / Appear Animation
                    if t < 0.35 && !pst.title_text.is_empty() && pst.start_pill_h > 24.0 {
                        let title_alpha = ((0.35 - t) / 0.35).clamp(0.0, 1.0);
                        let title_col_val = (244.0 * title_alpha).round().clamp(0.0, 255.0) as u8;
                        let title_color = rgb(title_col_val, title_col_val, title_col_val);
                        let pill_center_y = (pst.start_pill_h as i32) / 2;
                        let mut title_rc = RECT {
                            left: (cur_x + 14.0).round() as i32,
                            top: (pill_center_y - 15).max(0),
                            right: (cur_x + cur_w - 14.0).round() as i32,
                            bottom: (pill_center_y + 15).max(30),
                        };
                        let font_title_pill = create_apple_font(-16, 600);
                        draw_text_centered(mem_dc, &pst.title_text, &mut title_rc, title_color, font_title_pill);
                        let _ = DeleteObject(font_title_pill);
                    }

                    // Seamless Dashboard Content Appear / Disappear Animation
                    let content_alpha = ((t - 0.25) / 0.75).clamp(0.0, 1.0);
                    let content_ease = content_alpha * content_alpha * (3.0 - 2.0 * content_alpha);

                    if !bg_pixels.is_empty() && content_ease < 0.999 {
                        for idx in 0..pixels.len() {
                            let fg_p = pixels[idx];
                            let bg_p = bg_pixels[idx];
                            if fg_p != bg_p {
                                let fg_a = ((fg_p >> 24) & 0xFF) as f32;
                                let fg_r = ((fg_p >> 16) & 0xFF) as f32;
                                let fg_g = ((fg_p >> 8) & 0xFF) as f32;
                                let fg_b = (fg_p & 0xFF) as f32;

                                let bg_a = ((bg_p >> 24) & 0xFF) as f32;
                                let bg_r = ((bg_p >> 16) & 0xFF) as f32;
                                let bg_g = ((bg_p >> 8) & 0xFF) as f32;
                                let bg_b = (bg_p & 0xFF) as f32;

                                let a = (bg_a + (fg_a - bg_a) * content_ease).round().clamp(0.0, 255.0) as u32;
                                let r = (bg_r + (fg_r - bg_r) * content_ease).round().clamp(0.0, 255.0) as u32;
                                let g = (bg_g + (fg_g - bg_g) * content_ease).round().clamp(0.0, 255.0) as u32;
                                let b = (bg_b + (fg_b - bg_b) * content_ease).round().clamp(0.0, 255.0) as u32;
                                pixels[idx] = (a << 24) | (r << 16) | (g << 8) | b;
                            }
                        }
                    }
                }

                // Premultiply BGRA channels for DWM hardware composition
                for p in pixels.iter_mut() {
                    let a = (*p >> 24) & 0xFF;
                    if a == 0 {
                        *p = 0;
                    } else if a < 255 {
                        let r = (((*p >> 16) & 0xFF) * a) / 255;
                        let g = (((*p >> 8) & 0xFF) * a) / 255;
                        let b = ((*p & 0xFF) * a) / 255;
                        *p = (a << 24) | (r << 16) | (g << 8) | b;
                    }
                }

                if is_dashboard {
                    let min_px = (cur_x - cur_r_ear - 2.0).floor() as i32;
                    let max_px = (cur_x + cur_w + cur_r_ear + 2.0).ceil() as i32;
                    let max_py = (cur_h + 2.0).ceil() as i32;

                    for py in 0..h {
                        let row = (py as usize) * (w as usize);
                        let py_f = py as f32 + 0.5;
                        for px in 0..w {
                            let idx = row + px as usize;
                            if py >= max_py || px < min_px || px > max_px {
                                pixels[idx] = 0;
                                continue;
                            }
                            if py < MONITOR_BORDER_PX as i32 {
                                continue;
                            }
                            let px_f = px as f32 + 0.5;
                            let dist = compute_notch_sdf(
                                px_f,
                                py_f,
                                cur_x,
                                cur_x + cur_w,
                                cur_h,
                                cur_r_corner,
                                cur_r_ear,
                                true,
                                true,
                            );
                            let cov = (0.5 - dist).clamp(0.0, 1.0);
                            if cov <= 0.001 {
                                pixels[idx] = 0;
                            } else if cov < 0.999 {
                                let p = pixels[idx];
                                let a = (((p >> 24) & 0xFF) as f32 * cov).round() as u32;
                                let r = (((p >> 16) & 0xFF) as f32 * cov).round() as u32;
                                let g = (((p >> 8) & 0xFF) as f32 * cov).round() as u32;
                                let b = ((p & 0xFF) as f32 * cov).round() as u32;
                                pixels[idx] = (a << 24) | (r << 16) | (g << 8) | b;
                            }
                        }
                    }
                }

                let sz_win = SIZE { cx: w, cy: h };
                let pt_src = POINT { x: 0, y: 0 };
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let _ = UpdateLayeredWindow(
                    hwnd,
                    hdc,
                    None,
                    Some(&sz_win),
                    mem_dc,
                    Some(&pt_src),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );

                SelectObject(mem_dc, old_bmp);
                let _ = DeleteObject(font_hdr);
                let _ = DeleteObject(font_grid);
                let _ = DeleteObject(font_sec);
                let _ = DeleteObject(font_menu_icon);
                let _ = DeleteObject(font_large_clock);
                let _ = DeleteObject(font_weather_temp);
                let _ = DeleteObject(font_weather_icon);
                let _ = DeleteObject(font_tab);
                let _ = DeleteObject(font_tab_icon);
                let _ = DeleteObject(dib_bmp);
            }

            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(HWND::default(), hdc);
}

// ----------------------------------------------------------------------------
// Internal Typography Drawing Helpers (Mixed Apple SF Pro & Nerd Font Runs)
// ----------------------------------------------------------------------------

struct TextScratchSurface {
    dc: HDC,
    bmp: HBITMAP,
    bits: *mut u32,
    w: i32,
    h: i32,
}

unsafe impl Send for TextScratchSurface {}
unsafe impl Sync for TextScratchSurface {}

static TEXT_SCRATCH: OnceLock<Mutex<TextScratchSurface>> = OnceLock::new();
static FONT_2X_CACHE: OnceLock<Mutex<HashMap<(i32, i32, String), usize>>> = OnceLock::new();

unsafe fn get_2x_font(font: HFONT) -> HFONT {
    if font.is_invalid() || font.0.is_null() {
        return font;
    }

    let mut lf = LOGFONTW::default();
    if GetObjectW(
        font,
        std::mem::size_of::<LOGFONTW>() as i32,
        Some(&mut lf as *mut _ as *mut core::ffi::c_void),
    ) == 0 {
        return font;
    }

    let face_len = lf.lfFaceName.iter().position(|&c| c == 0).unwrap_or(lf.lfFaceName.len());
    let face_name = String::from_utf16_lossy(&lf.lfFaceName[..face_len]);
    let key = (lf.lfHeight, lf.lfWeight, face_name.clone());

    let cache = FONT_2X_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(&cached_hfont) = guard.get(&key) {
            return HFONT(cached_hfont as *mut core::ffi::c_void);
        }
    }

    let face_u16: Vec<u16> = face_name.encode_utf16().chain(Some(0)).collect();
    let hfont_2x = create_font(
        PCWSTR(face_u16.as_ptr()),
        lf.lfHeight * 2,
        lf.lfWeight,
    );

    if let Ok(mut guard) = cache.lock() {
        guard.insert(key, hfont_2x.0 as usize);
    }
    hfont_2x
}

unsafe fn get_text_scratch(min_w: i32, min_h: i32) -> &'static Mutex<TextScratchSurface> {
    TEXT_SCRATCH.get_or_init(|| {
        let w = min_w.max(2048);
        let h = min_h.max(256);
        let screen_dc = GetDC(HWND::default());
        let dc = CreateCompatibleDC(screen_dc);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: (w * h * 4) as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
        let bmp = CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
        let _ = ReleaseDC(HWND::default(), screen_dc);
        SelectObject(dc, bmp);
        SetBkMode(dc, TRANSPARENT);
        Mutex::new(TextScratchSurface {
            dc,
            bmp,
            bits: bits as *mut u32,
            w,
            h,
        })
    })
}

unsafe fn get_dc_bitmap_bits(hdc: HDC) -> Option<(&'static mut [u32], i32, i32)> {
    let cur_obj = GetCurrentObject(hdc, OBJ_BITMAP);
    if cur_obj.is_invalid() || cur_obj.0.is_null() {
        return None;
    }
    let mut bm = BITMAP::default();
    if GetObjectW(
        cur_obj,
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut bm as *mut _ as *mut core::ffi::c_void),
    ) == 0 {
        return None;
    }
    if bm.bmBits.is_null() || bm.bmWidth <= 0 || bm.bmHeight <= 0 || bm.bmBitsPixel != 32 {
        return None;
    }
    let len = (bm.bmWidth * bm.bmHeight.abs()) as usize;
    let slice = std::slice::from_raw_parts_mut(bm.bmBits as *mut u32, len);
    Some((slice, bm.bmWidth, bm.bmHeight.abs()))
}

unsafe fn render_text_to_scratch_and_blend_clipped(
    hdc: HDC,
    text: &str,
    rect: &RECT,
    rc_clip: &RECT,
    color: COLORREF,
    font: HFONT,
    format: DRAW_TEXT_FORMAT,
) -> bool {
    let (dst_pixels, dst_w, dst_h) = match get_dc_bitmap_bits(hdc) {
        Some(res) => res,
        None => return false,
    };

    let rw = (rect.right - rect.left).max(1);
    let rh = (rect.bottom - rect.top).max(1);
    if rw <= 0 || rh <= 0 || text.is_empty() {
        return true;
    }

    let scratch_mutex = get_text_scratch(rw * 2, rh * 2);
    let guard = match scratch_mutex.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let s_w = guard.w;
    let s_h = guard.h;
    let s_bits = guard.bits;
    let s_dc = guard.dc;

    if s_bits.is_null() {
        return false;
    }

    let scratch_slice = std::slice::from_raw_parts_mut(s_bits, (s_w * s_h) as usize);

    // Render at 2x resolution with high-DPI TrueType rasterization
    let hfont_2x = get_2x_font(font);
    let render_w = rw.min(s_w / 2);
    let render_h = rh.min(s_h / 2);
    let sub_w = render_w * 2;
    let sub_h = render_h * 2;

    for y in 0..sub_h {
        let row_start = (y * s_w) as usize;
        scratch_slice[row_start..row_start + (sub_w as usize)].fill(0x0000_0000);
    }

    let old_font = SelectObject(s_dc, hfont_2x);
    SetTextColor(s_dc, rgb(255, 255, 255));
    SetBkMode(s_dc, TRANSPARENT);

    let mut scratch_rc = RECT {
        left: 0,
        top: 0,
        right: sub_w,
        bottom: sub_h,
    };
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    DrawTextW(s_dc, &mut utf16, &mut scratch_rc, format);
    SelectObject(s_dc, old_font);

    // Isotropic 2x2 box-filter downsampling with linear coverage and clip bounds check
    for iy in 0..render_h {
        let dy = rect.top + iy;
        if dy < 0 || dy >= dst_h || dy < rc_clip.top || dy >= rc_clip.bottom {
            continue;
        }
        let dst_row = (dy as usize) * (dst_w as usize);

        let sy0 = (iy * 2) as usize;
        let sy1 = sy0 + 1;
        let s_row0 = sy0 * (s_w as usize);
        let s_row1 = sy1 * (s_w as usize);

        for ix in 0..render_w {
            let dx = rect.left + ix;
            if dx < 0 || dx >= dst_w || dx < rc_clip.left || dx >= rc_clip.right {
                continue;
            }

            let sx0 = (ix * 2) as usize;
            let sx1 = sx0 + 1;

            let p00 = scratch_slice[s_row0 + sx0];
            let p01 = scratch_slice[s_row0 + sx1];
            let p10 = scratch_slice[s_row1 + sx0];
            let p11 = scratch_slice[s_row1 + sx1];

            if (p00 | p01 | p10 | p11) == 0 {
                continue;
            }

            let c00 = ((p00 >> 16) & 0xFF).max((p00 >> 8) & 0xFF).max(p00 & 0xFF);
            let c01 = ((p01 >> 16) & 0xFF).max((p01 >> 8) & 0xFF).max(p01 & 0xFF);
            let c10 = ((p10 >> 16) & 0xFF).max((p10 >> 8) & 0xFF).max(p10 & 0xFF);
            let c11 = ((p11 >> 16) & 0xFF).max((p11 >> 8) & 0xFF).max(p11 & 0xFF);

            let sum = (c00 + c01 + c10 + c11) as f32;
            let raw_cov = sum / (4.0 * 255.0);

            // Linear coverage eliminates bolding and color bleeding
            let cov_u8 = (raw_cov * 255.0).round().min(255.0) as u8;

            if cov_u8 > 0 {
                let dst_idx = dst_row + (dx as usize);
                dst_pixels[dst_idx] = alpha_blend_straight_bgra(dst_pixels[dst_idx], color, cov_u8);
            }
        }
    }
    true
}

#[inline]
unsafe fn render_text_to_scratch_and_blend(
    hdc: HDC,
    text: &str,
    rect: &RECT,
    color: COLORREF,
    font: HFONT,
    format: DRAW_TEXT_FORMAT,
) -> bool {
    render_text_to_scratch_and_blend_clipped(hdc, text, rect, rect, color, font, format)
}

unsafe fn draw_text_marquee(
    hdc: HDC,
    text: &str,
    rc: &mut RECT,
    color: COLORREF,
    font: HFONT,
    phase: f32,
) {
    let text_w = measure_text_width(hdc, text, font);
    let avail_w = (rc.right - rc.left).max(1);

    if text_w <= avail_w {
        draw_text_centered(hdc, text, rc, color, font);
        return;
    }

    let overflow = (text_w - avail_w) as f32;
    let t = 0.5 - 0.5 * (phase * 0.55).cos();
    let offset_x = -(overflow * t).round() as i32;
    let start_x = rc.left + offset_x;

    let rc_clip = *rc;

    if !text.chars().any(is_nerd_font_glyph) {
        let draw_rc = RECT {
            left: start_x,
            top: rc.top,
            right: start_x + text_w,
            bottom: rc.bottom,
        };
        let format = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX;
        render_text_to_scratch_and_blend_clipped(hdc, text, &draw_rc, &rc_clip, color, font, format);
        return;
    }

    let mut lf = LOGFONTW::default();
    GetObjectW(
        font,
        std::mem::size_of::<LOGFONTW>() as i32,
        Some(&mut lf as *mut LOGFONTW as *mut core::ffi::c_void),
    );
    let icon_font = create_icon_font(lf.lfHeight, 400);

    let chunks = split_icon_chunks(text);
    let mut cur_x = start_x;
    for (is_icon, chunk) in chunks {
        let f = if is_icon { icon_font } else { font };
        let old_f = SelectObject(hdc, f);
        let utf16: Vec<u16> = chunk.encode_utf16().collect();
        let mut sz = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &utf16, &mut sz);
        SelectObject(hdc, old_f);
        let chunk_w = if is_icon { sz.cx + (sz.cy / 2).max(12) } else { sz.cx };

        let chunk_rc = RECT {
            left: cur_x,
            top: rc.top,
            right: cur_x + chunk_w,
            bottom: rc.bottom,
        };
        let format = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX;
        if chunk_rc.right > rc_clip.left && chunk_rc.left < rc_clip.right {
            let _ = render_text_to_scratch_and_blend_clipped(hdc, &chunk, &chunk_rc, &rc_clip, color, f, format);
        }
        cur_x += chunk_w;
    }

    let _ = DeleteObject(icon_font);
}

unsafe fn draw_text_centered(
    hdc: HDC,
    text: &str,
    rect: &mut RECT,
    color: COLORREF,
    font: HFONT,
) {
    if !text.chars().any(is_nerd_font_glyph) {
        let format = DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS;
        if render_text_to_scratch_and_blend(hdc, text, rect, color, font, format) {
            return;
        }
        let old_font = SelectObject(hdc, font);
        SetTextColor(hdc, color);
        let mut utf16: Vec<u16> = text.encode_utf16().collect();
        DrawTextW(hdc, &mut utf16, rect, format);
        SelectObject(hdc, old_font);
        return;
    }

    let mut lf = LOGFONTW::default();
    GetObjectW(
        font,
        std::mem::size_of::<LOGFONTW>() as i32,
        Some(&mut lf as *mut LOGFONTW as *mut core::ffi::c_void),
    );
    let icon_font = create_icon_font(lf.lfHeight, 400);

    let chunks = split_icon_chunks(text);
    if chunks.len() == 1 && chunks[0].0 {
        let format = DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX;
        if render_text_to_scratch_and_blend(hdc, text, rect, color, icon_font, format) {
            let _ = DeleteObject(icon_font);
            return;
        }
    }

    let mut chunk_sizes = Vec::with_capacity(chunks.len());
    let mut total_w = 0;

    for (is_icon, chunk) in &chunks {
        let f = if *is_icon { icon_font } else { font };
        let old_f = SelectObject(hdc, f);
        let utf16: Vec<u16> = chunk.encode_utf16().collect();
        let mut sz = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &utf16, &mut sz);
        SelectObject(hdc, old_f);
        let chunk_w = if *is_icon { sz.cx + (sz.cy / 2).max(12) } else { sz.cx };
        chunk_sizes.push(chunk_w);
        total_w += chunk_w;
    }

    let rect_w = rect.right - rect.left;
    let mut cur_x = rect.left + ((rect_w - total_w) / 2).max(0);

    for (i, (is_icon, chunk)) in chunks.iter().enumerate() {
        let chunk_w = chunk_sizes[i];
        let f = if *is_icon { icon_font } else { font };
        let mut chunk_rc = RECT {
            left: cur_x,
            top: rect.top,
            right: cur_x + chunk_w,
            bottom: rect.bottom,
        };
        let format = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX;
        if !render_text_to_scratch_and_blend(hdc, chunk, &chunk_rc, color, f, format) {
            let old_f = SelectObject(hdc, f);
            SetTextColor(hdc, color);
            let mut utf16: Vec<u16> = chunk.encode_utf16().collect();
            DrawTextW(hdc, &mut utf16, &mut chunk_rc, format);
            SelectObject(hdc, old_f);
        }
        cur_x += chunk_w;
    }

    let _ = DeleteObject(icon_font);
}

unsafe fn draw_text_left(
    hdc: HDC,
    text: &str,
    rect: &mut RECT,
    color: COLORREF,
    font: HFONT,
) {
    let format = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS;
    if !text.chars().any(is_nerd_font_glyph) {
        if render_text_to_scratch_and_blend(hdc, text, rect, color, font, format) {
            return;
        }
        let old_font = SelectObject(hdc, font);
        SetTextColor(hdc, color);
        let mut utf16: Vec<u16> = text.encode_utf16().collect();
        DrawTextW(hdc, &mut utf16, rect, format);
        SelectObject(hdc, old_font);
        return;
    }

    let mut lf = LOGFONTW::default();
    GetObjectW(
        font,
        std::mem::size_of::<LOGFONTW>() as i32,
        Some(&mut lf as *mut LOGFONTW as *mut core::ffi::c_void),
    );
    let icon_font = create_icon_font(lf.lfHeight, 400);

    let chunks = split_icon_chunks(text);
    if chunks.len() == 1 && chunks[0].0 {
        let format = DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX;
        if render_text_to_scratch_and_blend(hdc, text, rect, color, icon_font, format) {
            let _ = DeleteObject(icon_font);
            return;
        }
    }
    let mut cur_x = rect.left;

    for (is_icon, chunk) in chunks {
        let f = if is_icon { icon_font } else { font };
        let mut utf16: Vec<u16> = chunk.encode_utf16().collect();
        let old_f = SelectObject(hdc, f);
        let mut sz = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &utf16, &mut sz);
        SelectObject(hdc, old_f);
        let chunk_w = if is_icon { sz.cx + (sz.cy / 2).max(12) } else { sz.cx };
        let mut chunk_rc = RECT {
            left: cur_x,
            top: rect.top,
            right: (cur_x + chunk_w).min(rect.right),
            bottom: rect.bottom,
        };
        let chunk_format = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX;
        if !render_text_to_scratch_and_blend(hdc, &chunk, &chunk_rc, color, f, chunk_format) {
            let old_f = SelectObject(hdc, f);
            SetTextColor(hdc, color);
            DrawTextW(hdc, &mut utf16, &mut chunk_rc, chunk_format);
            SelectObject(hdc, old_f);
        }
        cur_x += chunk_w;
        if cur_x >= rect.right {
            break;
        }
    }

    let _ = DeleteObject(icon_font);
}

#[derive(Clone)]
struct CachedGlyph {
    ink_w: i32,
    ink_h: i32,
    mask: Vec<u8>,
    opt_offset_x: f32,
}

static GLYPH_INK_CACHE: OnceLock<Mutex<HashMap<String, CachedGlyph>>> = OnceLock::new();

/// Renders a single Nerd Font icon glyph onto a scratch 48x48 DIB once, caches its exact non-zero
/// ink pixel bounding box `(ink_w, ink_h, alpha_mask, opt_offset_x)`, and alpha-blends the glyph
/// pixel-accurately centered (including optical center-of-mass balancing for perspective glyphs like
/// `\u{F17A}`) inside `target_rect` with 0 GDI overhead on subsequent frames!
unsafe fn draw_icon_ink_centered(
    pixels: &mut [u32],
    stride: usize,
    max_w: i32,
    max_h: i32,
    glyph: &str,
    target_rect: (i32, i32, i32, i32),
    color: COLORREF,
    font: HFONT,
) {
    let (rx, ry, rw, rh) = target_rect;
    if rw <= 0 || rh <= 0 || glyph.is_empty() {
        return;
    }

    let mut lf = LOGFONTW::default();
    GetObjectW(
        font,
        std::mem::size_of::<LOGFONTW>() as i32,
        Some(&mut lf as *mut _ as *mut _),
    );
    let cache_key = format!("{glyph}:{}", lf.lfHeight);

    let cache = GLYPH_INK_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let cached_opt = if let Ok(guard) = cache.lock() {
        guard.get(&cache_key).cloned()
    } else {
        None
    };

    let cached = if let Some(c) = cached_opt {
        c
    } else {
        const PAD_SZ: i32 = 96;
        let mut lf_2x = lf;
        lf_2x.lfHeight = lf.lfHeight * 2;
        lf_2x.lfWeight = 400; // FW_NORMAL
        let font_2x = CreateFontIndirectW(&lf_2x);

        let screen_dc = GetDC(HWND::default());
        if screen_dc.is_invalid() {
            let _ = DeleteObject(font_2x);
            return;
        }
        let dc = CreateCompatibleDC(screen_dc);
        if dc.is_invalid() {
            let _ = DeleteObject(font_2x);
            let _ = ReleaseDC(HWND::default(), screen_dc);
            return;
        }

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: PAD_SZ,
                biHeight: -PAD_SZ,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: (PAD_SZ * PAD_SZ * 4) as u32,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
        let mut new_entry = CachedGlyph {
            ink_w: 0,
            ink_h: 0,
            mask: Vec::new(),
            opt_offset_x: 0.0,
        };

        if let Ok(bmp) = CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            if !bmp.is_invalid() && !bits.is_null() {
                let scratch: &mut [u32] =
                    std::slice::from_raw_parts_mut(bits as *mut u32, (PAD_SZ * PAD_SZ) as usize);
                scratch.fill(0x0000_0000);

                let old_bmp = SelectObject(dc, bmp);
                let old_font = SelectObject(dc, font_2x);
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, rgb(255, 255, 255));

                let mut rc = RECT {
                    left: 0,
                    top: 0,
                    right: PAD_SZ,
                    bottom: PAD_SZ,
                };
                let mut utf16: Vec<u16> = glyph.encode_utf16().collect();
                DrawTextW(
                    dc,
                    &mut utf16,
                    &mut rc,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                );

                let mut min_x = PAD_SZ;
                let mut max_x = -1;
                let mut min_y = PAD_SZ;
                let mut max_y = -1;
                let mut sum_x: f64 = 0.0;
                let mut sum_w: f64 = 0.0;

                for y in 0..PAD_SZ {
                    for x in 0..PAD_SZ {
                        let p = scratch[(y * PAD_SZ + x) as usize];
                        let lum = ((p >> 16) & 0xFF).max((p >> 8) & 0xFF).max(p & 0xFF);
                        if lum > 16 {
                            if x < min_x { min_x = x; }
                            if x > max_x { max_x = x; }
                            if y < min_y { min_y = y; }
                            if y > max_y { max_y = y; }
                            let w = lum as f64;
                            sum_x += (x as f64 + 0.5) * w;
                            sum_w += w;
                        }
                    }
                }

                if max_x >= min_x && max_y >= min_y {
                    let min_x_even = (min_x / 2) * 2;
                    let max_x_even = (max_x / 2) * 2 + 1;
                    let min_y_even = (min_y / 2) * 2;
                    let max_y_even = (max_y / 2) * 2 + 1;

                    let box_w_2x = max_x_even - min_x_even + 1;
                    let box_h_2x = max_y_even - min_y_even + 1;
                    let ink_w = box_w_2x / 2;
                    let ink_h = box_h_2x / 2;

                    let bbox_cx_2x = min_x_even as f32 + (box_w_2x as f32) * 0.5;
                    let opt_offset_x = if sum_w > 0.0 {
                        (((sum_x / sum_w) as f32) - bbox_cx_2x) * 0.5
                    } else {
                        0.0
                    };

                    let mut mask = vec![0u8; (ink_w * ink_h) as usize];
                    for iy in 0..ink_h {
                        let sy0 = (min_y_even + iy * 2) as usize;
                        let sy1 = sy0 + 1;
                        let row0 = sy0 * (PAD_SZ as usize);
                        let row1 = sy1 * (PAD_SZ as usize);

                        for ix in 0..ink_w {
                            let sx0 = (min_x_even + ix * 2) as usize;
                            let sx1 = sx0 + 1;

                            let p00 = scratch[row0 + sx0];
                            let p01 = scratch[row0 + sx1];
                            let p10 = scratch[row1 + sx0];
                            let p11 = scratch[row1 + sx1];

                            let c00 = ((p00 >> 16) & 0xFF).max((p00 >> 8) & 0xFF).max(p00 & 0xFF);
                            let c01 = ((p01 >> 16) & 0xFF).max((p01 >> 8) & 0xFF).max(p01 & 0xFF);
                            let c10 = ((p10 >> 16) & 0xFF).max((p10 >> 8) & 0xFF).max(p10 & 0xFF);
                            let c11 = ((p11 >> 16) & 0xFF).max((p11 >> 8) & 0xFF).max(p11 & 0xFF);

                            let sum = (c00 + c01 + c10 + c11) as f32;
                            let raw_cov = sum / (4.0 * 255.0);
                            let cov_u8 = (raw_cov * 255.0).round().min(255.0) as u8;
                            mask[(iy * ink_w + ix) as usize] = cov_u8;
                        }
                    }

                    new_entry = CachedGlyph {
                        ink_w,
                        ink_h,
                        mask,
                        opt_offset_x,
                    };
                }

                SelectObject(dc, old_font);
                let _ = DeleteObject(font_2x);
                SelectObject(dc, old_bmp);
                let _ = DeleteObject(bmp);
            } else {
                let _ = DeleteObject(font_2x);
            }
        } else {
            let _ = DeleteObject(font_2x);
        }
        let _ = DeleteDC(dc);
        let _ = ReleaseDC(HWND::default(), screen_dc);

        if let Ok(mut guard) = cache.lock() {
            guard.insert(cache_key, new_entry.clone());
        }
        new_entry
    };

    let ink_w = cached.ink_w;
    let ink_h = cached.ink_h;
    if ink_w <= 0 || ink_h <= 0 {
        return;
    }

    let max_box_w = (rw - 2).max(1) as f32;
    let max_box_h = (rh - 2).max(1) as f32;
    let scale = (max_box_w / ink_w as f32)
        .min(max_box_h / ink_h as f32)
        .min(1.0);

    let draw_w = ((ink_w as f32) * scale).round().max(1.0) as i32;
    let draw_h = ((ink_h as f32) * scale).round().max(1.0) as i32;

    let target_cx = rx as f32 + (rw as f32) * 0.5;
    let target_cy = ry as f32 + (rh as f32) * 0.5;
    let opt_off = cached.opt_offset_x * scale;
    let dst_x0 = (target_cx - (draw_w as f32) * 0.5 - opt_off).round() as i32;
    let dst_y0 = (target_cy - (draw_h as f32) * 0.5).round() as i32;

    for iy in 0..draw_h {
        let dy = dst_y0 + iy;
        if dy < 0 || dy >= max_h {
            continue;
        }
        let dst_row = (dy as usize) * stride;

        for ix in 0..draw_w {
            let dx = dst_x0 + ix;
            if dx < 0 || dx >= max_w {
                continue;
            }

            let cov = if scale >= 0.999 {
                let sy = iy as usize;
                let sx = ix as usize;
                cached.mask[sy * (ink_w as usize) + sx]
            } else {
                let fx = ((ix as f32 + 0.5) / scale - 0.5).clamp(0.0, (ink_w - 1) as f32);
                let fy = ((iy as f32 + 0.5) / scale - 0.5).clamp(0.0, (ink_h - 1) as f32);
                let x0 = fx.floor() as usize;
                let x1 = (x0 + 1).min((ink_w - 1) as usize);
                let tx = fx - x0 as f32;
                let y0 = fy.floor() as usize;
                let y1 = (y0 + 1).min((ink_h - 1) as usize);
                let ty = fy - y0 as f32;

                let w_row0 = y0 * (ink_w as usize);
                let w_row1 = y1 * (ink_w as usize);
                let c00 = cached.mask[w_row0 + x0] as f32;
                let c10 = cached.mask[w_row0 + x1] as f32;
                let c01 = cached.mask[w_row1 + x0] as f32;
                let c11 = cached.mask[w_row1 + x1] as f32;

                let top = (1.0 - tx) * c00 + tx * c10;
                let bot = (1.0 - tx) * c01 + tx * c11;
                ((1.0 - ty) * top + ty * bot).round().min(255.0) as u8
            };

            if cov > 0 {
                let idx = dst_row + (dx as usize);
                pixels[idx] = alpha_blend_straight_bgra(pixels[idx], color, cov);
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Per-Element Hover Tooltip Popup (`HyprBarTooltipPopup` with `ULW_ALPHA`)
// ----------------------------------------------------------------------------

static TOOLTIP_HWND: OnceLock<Mutex<Option<(isize, bool, String, i32)>>> = OnceLock::new();
static TOOLTIP_CLASS_INIT: OnceLock<()> = OnceLock::new();

fn resolve_hover_tooltip(model: &BarRenderModel) -> Option<(String, (i32, i32, i32, i32))> {
    let (hx, hy) = model.hover_pos?;
    let target = hit_test_bar(model, hx, hy)?;
    match target {
        BarClickTarget::OmarchyLauncher => Some((
            "Hyprland-Win Menu".to_string(),
            model.omarchy_rect,
        )),
        BarClickTarget::Workspace(ws) => {
            let idx = (ws.saturating_sub(1) as usize).min(9);
            let count = model.workspace_hwnds[idx]
                .len()
                .max(model.occupied_workspaces[idx] as usize);
            let status = if ws == model.active_workspace {
                format!("Workspace {ws} (Active — {count} window{})", if count == 1 { "" } else { "s" })
            } else if count > 0 {
                format!("Workspace {ws} ({count} window{} — Click to switch)", if count == 1 { "" } else { "s" })
            } else {
                format!("Workspace {ws} (Empty — Click to switch)")
            };
            Some((status, model.ws_pill_rects[idx]))
        }
        BarClickTarget::WorkspacesContainer => None,
        BarClickTarget::MprisPlayPause | BarClickTarget::MprisBody => {
            let m = &model.widgets.media;
            let state_str = match m.status {
                MediaPlaybackStatus::Playing => "Playing",
                MediaPlaybackStatus::Paused => "Paused",
                MediaPlaybackStatus::Stopped => "Stopped",
            };
            let text = if m.artist.is_empty() {
                format!("{} [{}] — LMB: Play/Pause | RMB: Next | Scroll: Volume", m.title, state_str)
            } else {
                format!(
                    "{} - {} [{}] — LMB: Play/Pause | RMB: Next",
                    m.artist, m.title, state_str
                )
            };
            Some((text, model.mpris_rect))
        }
        BarClickTarget::FocusedWindow => {
            let title = if model.active_title.is_empty() {
                "Desktop"
            } else {
                &model.active_title
            };
            let cls = if model.active_class.is_empty() {
                "Hyprland-Win"
            } else {
                &model.active_class
            };
            Some((
                format!("{title} [{cls}] — LMB: Fullscreen | RMB: Toggle Floating"),
                model.window_rect,
            ))
        }
        BarClickTarget::TrayExpanderToggle => {
            let msg = if model.tray_expanded {
                "Collapse System Tray Utilities"
            } else {
                "Expand System Tray Utilities (Live Tray Apps / Screenshot / Lock / Tiling)"
            };
            Some((msg.to_string(), model.tray_toggle_rect))
        }
        BarClickTarget::TrayApp(idx) => {
            let item = model.widgets.tray_items.get(idx)?;
            let rect = *model.tray_app_rects.get(idx)?;
            Some((
                format!(
                    "{} (System Tray) — LMB: Open/Focus | RMB: Close",
                    item.name
                ),
                rect,
            ))
        }
        BarClickTarget::TrayShot => Some((
            "Screenshot (LMB: Region Super+Shift+S | RMB: Fullscreen)".to_string(),
            model.tray_shot_rect,
        )),
        BarClickTarget::TrayLock => Some((
            "Lock Session (hyprlock — Super + Alt + L)".to_string(),
            model.tray_lock_rect,
        )),
        BarClickTarget::TrayTiling => {
            let state = if model.tiling_active { "Active" } else { "Paused" };
            Some((
                format!("Automatic Tiling: {state} ({} — Click to toggle)", model.layout_mode),
                model.tray_tiling_rect,
            ))
        }
        BarClickTarget::CtlIdleInhibit => {
            let state = if model.widgets.idle_inhibited {
                "Idle Inhibitor: Active (Preventing sleep)"
            } else {
                "Idle Inhibitor: Off (Click to keep system awake)"
            };
            Some((state.to_string(), model.ctl_idle_rect))
        }
        BarClickTarget::CtlDndSilence => {
            let state = if model.widgets.dnd_silenced {
                "Notifications: Do Not Disturb (Silenced)"
            } else {
                "Notifications: Active (Click to enable DND)"
            };
            Some((state.to_string(), model.ctl_dnd_rect))
        }
        BarClickTarget::CtlBluetooth => {
            let state = if model.widgets.bluetooth_enabled {
                "Bluetooth: Enabled (Click for Bluetooth Settings)"
            } else {
                "Bluetooth: Off (Click for Bluetooth Settings)"
            };
            Some((state.to_string(), model.ctl_bt_rect))
        }
        BarClickTarget::CtlNetwork => {
            let state = match model.widgets.network_kind {
                NetworkKind::Wifi => "Network: Connected via Wi-Fi (Click for Network Settings)",
                NetworkKind::Ethernet => "Network: Connected via Ethernet (Click for Network Settings)",
                NetworkKind::Disconnected => "Network: Disconnected (Click for Network Settings)",
            };
            Some((state.to_string(), model.ctl_net_rect))
        }
        BarClickTarget::CtlPulseAudio => {
            let vol = if model.widgets.volume_muted {
                format!("Audio: Muted ({}%)", model.widgets.volume_pct)
            } else {
                format!("Audio Volume: {}%", model.widgets.volume_pct)
            };
            Some((
                format!("{vol} — Scroll: +/-5% | LMB: Mixer | RMB: Mute"),
                model.ctl_vol_rect,
            ))
        }
        BarClickTarget::CtlCpu => Some((
            format!(
                "CPU Load: {}%  |  RAM: {:.1} / {:.1} GB (Click for Task Manager)",
                model.cpu_usage_pct, model.ram_used_gb, model.ram_total_gb
            ),
            model.ctl_cpu_rect,
        )),
        BarClickTarget::CtlBattery => {
            let state = if model.widgets.battery_charging {
                "Charging"
            } else {
                "Discharging"
            };
            Some((
                format!("Battery: {}% ({state})", model.widgets.battery_pct),
                model.ctl_bat_rect,
            ))
        }
        BarClickTarget::SystemMenu => None,
        BarClickTarget::Clock | BarClickTarget::ToggleLayout | BarClickTarget::ToggleTiling => None,
    }
}

unsafe extern "system" fn tooltip_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

pub unsafe fn update_tooltip_popup(bar_hwnd: HWND, model: &BarRenderModel, show_allowed: bool) {
    let mutex = TOOLTIP_HWND.get_or_init(|| Mutex::new(None));
    let Ok(mut guard) = mutex.lock() else {
        return;
    };

    let resolved = if show_allowed && model.active_popover == ActivePopover::None {
        resolve_hover_tooltip(model)
    } else {
        None
    };

    if let Some((text, anchor_rect)) = resolved {
        if anchor_rect.2 <= 0 {
            if let Some((hwnd_val, true, ref prev_text, prev_x)) = *guard {
                let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
                let _ = ShowWindow(hwnd, SW_HIDE);
                *guard = Some((hwnd_val, false, prev_text.clone(), prev_x));
            }
            return;
        }

        let anchor_cx = anchor_rect.0 + anchor_rect.2 / 2;

        if let Some((hwnd_val, is_shown, ref prev_text, prev_cx)) = *guard {
            if is_shown && *prev_text == text && (prev_cx - anchor_cx).abs() <= 2 {
                return;
            }
            let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
            paint_and_position_tooltip_layered(hwnd, model, &text, anchor_cx);
            if !is_shown {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
            *guard = Some((hwnd_val, true, text, anchor_cx));
        } else {
            TOOLTIP_CLASS_INIT.get_or_init(|| {
                let class_name: Vec<u16> = "HyprBarTooltipPopup\0".encode_utf16().collect();
                let wc = WNDCLASSW {
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(tooltip_wnd_proc),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    lpszClassName: PCWSTR(class_name.as_ptr()),
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);
            });

            let class_name: Vec<u16> = "HyprBarTooltipPopup\0".encode_utf16().collect();
            let title: Vec<u16> = "HyprBar Tooltip\0".encode_utf16().collect();
            let ex_style =
                windows::Win32::UI::WindowsAndMessaging::WS_EX_LAYERED
                    | WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | windows::Win32::UI::WindowsAndMessaging::WS_EX_TRANSPARENT;

            if let Ok(hwnd) = CreateWindowExW(
                ex_style,
                PCWSTR(class_name.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_POPUP,
                0,
                0,
                64,
                28,
                bar_hwnd,
                None,
                None,
                None,
            ) {
                let round_pref = DWMWCP_ROUND.0 as u32;
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_WINDOW_CORNER_PREFERENCE,
                    &round_pref as *const u32 as *const _,
                    4,
                );
                let border_none: u32 = 0xFFFF_FFFE;
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_BORDER_COLOR,
                    &border_none as *const u32 as *const _,
                    4,
                );
                disable_window_acrylic(hwnd);
                paint_and_position_tooltip_layered(hwnd, model, &text, anchor_cx);
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                *guard = Some((hwnd.0 as isize, true, text, anchor_cx));
            }
        }
    } else if let Some((hwnd_val, is_shown, ref prev_text, prev_cx)) = *guard {
        if is_shown {
            let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
            let _ = ShowWindow(hwnd, SW_HIDE);
            *guard = Some((hwnd_val, false, prev_text.clone(), prev_cx));
        }
    }
}

unsafe fn paint_and_position_tooltip_layered(
    hwnd: HWND,
    model: &BarRenderModel,
    text: &str,
    anchor_cx: i32,
) {
    ensure_private_fonts_loaded();
    let screen_dc = GetDC(HWND::default());
    if screen_dc.is_invalid() {
        return;
    }
    let mem_dc = CreateCompatibleDC(screen_dc);
    if mem_dc.is_invalid() {
        let _ = ReleaseDC(HWND::default(), screen_dc);
        return;
    }

    let font = create_apple_font(-12, 600);
    let text_w = measure_text_width(mem_dc, text, font);
    let w = (text_w + 24).clamp(64, model.bar_width - 20);
    let h = 28;
    let x = (anchor_cx - w / 2).clamp(10, (model.bar_width - w - 10).max(10));
    let y = model.bar_height + 4;

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (w * h * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
    if let Ok(dib_bmp) = CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
        if !dib_bmp.is_invalid() && !bits.is_null() {
            let pixels: &mut [u32] =
                std::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize);
            pixels.fill(0x0000_0000);

            fill_aa_round_rect_f32(
                pixels,
                w as usize,
                w,
                h,
                0.0,
                0.0,
                w as f32,
                h as f32,
                10.0,
                rgb(32, 33, 38),
                rgb(68, 70, 78),
                1.0,
                1.0,
            );

            let pre_gdi: Vec<u32> = pixels.to_vec();
            let old_bmp = SelectObject(mem_dc, dib_bmp);
            SetBkMode(mem_dc, TRANSPARENT);

            let mut rc = RECT {
                left: 10,
                top: 0,
                right: w - 10,
                bottom: h,
            };
            draw_text_centered(mem_dc, text, &mut rc, rgb(228, 230, 235), font);

            for (idx, slot) in pixels.iter_mut().enumerate() {
                let base_p = pre_gdi[idx];
                let base_a = (base_p >> 24) & 0xFF;
                let cur_p = *slot;
                if cur_p == base_p {
                    if base_a == 0 {
                        *slot = 0;
                    } else {
                        let r = (((base_p >> 16) & 0xFF) * base_a) / 255;
                        let g = (((base_p >> 8) & 0xFF) * base_a) / 255;
                        let b = ((base_p & 0xFF) * base_a) / 255;
                        *slot = (base_a << 24) | (r << 16) | (g << 8) | b;
                    }
                } else {
                    let cur_r = (cur_p >> 16) & 0xFF;
                    let cur_g = (cur_p >> 8) & 0xFF;
                    let cur_b = cur_p & 0xFF;
                    let cur_a = (cur_p >> 24) & 0xFF;
                    let out_a = if cur_a > 0 { base_a.max(cur_a) } else { 255 };
                    let r = (cur_r * out_a) / 255;
                    let g = (cur_g * out_a) / 255;
                    let b = (cur_b * out_a) / 255;
                    *slot = (out_a << 24) | (r << 16) | (g << 8) | b;
                }
            }

            let pt_dst = POINT { x, y };
            let sz = SIZE { cx: w, cy: h };
            let pt_src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let _ = UpdateLayeredWindow(
                hwnd,
                screen_dc,
                Some(&pt_dst),
                Some(&sz),
                mem_dc,
                Some(&pt_src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );

            SelectObject(mem_dc, old_bmp);
            let _ = DeleteObject(dib_bmp);
        }
    }

    let _ = DeleteObject(font);
    let _ = DeleteDC(mem_dc);
    let _ = ReleaseDC(HWND::default(), screen_dc);
}
