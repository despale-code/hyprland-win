#![windows_subsystem = "windows"]
//! `hyprlauncher` — Native `wofi` / `rofi` Application & Window Switcher for `Hyprland-Win`.
//!
//! Resident ultra-low-latency (`< 0.5ms` wakeup) launcher daemon:
//! - Listens globally for `SUPER + R`, `SUPER + SPACE`, and `ALT + SPACE` (plus `--toggle` CLI arg).
//! - Pre-indexes Start Menu `.lnk` shortcuts and system executables.
//! - Queries live open windows across all 10 Hyprland-Win workspaces via `\\.\pipe\hyprland-win-ipc`.
//! - Ranks open windows and applications with a fast fuzzy scorer.
//! - Renders with `Departure Mono` and per-pixel SDF anti-aliasing (`ui.rs`).

mod ui;

use anyhow::Result;
use hyprland_win_core::{HyprIpcRequest, HyprIpcResponse, HYPRLAND_WIN_IPC_PIPE};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, keybd_event, SetFocus, KEYEVENTF_KEYUP, VK_BACK, VK_DOWN, VK_ESCAPE, VK_F24,
    VK_LMENU, VK_LWIN, VK_MENU, VK_RETURN, VK_RMENU, VK_RWIN, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, CallNextHookEx, CreateWindowExW, DefWindowProcW,
    DispatchMessageW, FindWindowW, GetMessageW, GetSystemMetrics, GetWindowRect,
    KBDLLHOOKSTRUCT, KillTimer, LoadCursorW, MSLLHOOKSTRUCT, PostMessageW, PostQuitMessage,
    RegisterClassW, SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos,
    SetWindowsHookExW, ShowWindow, TranslateMessage, UnhookWindowsHookEx, ASFW_ANY, CS_HREDRAW,
    CS_VREDRAW, GWLP_HWNDPARENT, HWND_TOPMOST, IDC_ARROW, MSG, SM_CXSCREEN, SM_CYSCREEN,
    SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNORMAL,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WM_ACTIVATE, WM_CHAR, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND,
    WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONDOWN,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WM_USER, WNDCLASSW, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use ui::{LauncherDisplayRow, LauncherRenderModel, LAUNCHER_HEIGHT, LAUNCHER_WIDTH, MAX_VISIBLE_ITEMS};

const WM_LAUNCHER_TOGGLE: u32 = WM_USER + 42;
const WM_LAUNCHER_CLIPBOARD: u32 = WM_USER + 43;
const TIMER_ID_ANIM: usize = 1;

static LAUNCHER_HWND: AtomicIsize = AtomicIsize::new(0);
static LAUNCHER_VISIBLE: AtomicBool = AtomicBool::new(false);
static CLIPBOARD_HISTORY: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

#[derive(Debug, Clone)]
enum CandidateAction {
    SwitchToWindow { hwnd: u64, workspace_id: u32 },
    LaunchApp { exec_path: String, args: String },
    CopyMathResult { result: String },
    CopyClipboardEntry { text: String },
    RunShellCommand { cmd: String },
    OpenWebUrl { url: String },
    HyprDispatch { dispatcher: String, arg: String },
}

#[derive(Debug, Clone)]
struct LauncherCandidate {
    title: String,
    subtitle: String,
    badge: String,
    search_key: String,
    icon_hwnd: u64,
    icon_path: String,
    action: CandidateAction,
}

struct LauncherState {
    screen_w: i32,
    screen_h: i32,
    win_x: i32,
    win_y: i32,
    app_catalog: Vec<LauncherCandidate>,
    open_windows: Vec<LauncherCandidate>,
    frecency: HashMap<String, u32>,
    filtered: Vec<LauncherCandidate>,
    model: LauncherRenderModel,
    last_anim_tick: Instant,
}

static STATE: OnceLock<Mutex<LauncherState>> = OnceLock::new();

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
        ui::ensure_apple_fonts_loaded();
    }

    let start_clipboard_mode = std::env::args().any(|a| a == "--clipboard" || a == "-c");

    // If another instance of `hyprlauncher` is already running, send it `WM_LAUNCHER_TOGGLE` or `WM_LAUNCHER_CLIPBOARD` and exit!
    unsafe {
        let cls: Vec<u16> = "HyprLauncherClass\0".encode_utf16().collect();
        if let Ok(existing) = FindWindowW(PCWSTR(cls.as_ptr()), PCWSTR::null()) {
            if !existing.0.is_null() {
                let _ = AllowSetForegroundWindow(ASFW_ANY);
                let msg = if start_clipboard_mode {
                    WM_LAUNCHER_CLIPBOARD
                } else {
                    WM_LAUNCHER_TOGGLE
                };
                let _ = PostMessageW(existing, msg, WPARAM(0), LPARAM(0));
                return Ok(());
            }
        }
    }

    let show_immediately =
        start_clipboard_mode || std::env::args().any(|a| a == "--show" || a == "--toggle");
    spawn_clipboard_history_watcher();

    let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN).max(1920) };
    let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN).max(1080) };
    let win_x = (screen_w - LAUNCHER_WIDTH) / 2;
    let win_y = screen_h - 9 - LAUNCHER_HEIGHT;

    let app_catalog = scan_installed_applications();
    let frecency = load_frecency_history();

    let mut initial_state = LauncherState {
        screen_w,
        screen_h,
        win_x,
        win_y,
        app_catalog,
        open_windows: Vec::new(),
        frecency,
        filtered: Vec::new(),
        model: LauncherRenderModel::default(),
        last_anim_tick: Instant::now(),
    };
    recompute_filtered_results(&mut initial_state);
    let _ = STATE.set(Mutex::new(initial_state));

    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let class_name: Vec<u16> = "HyprLauncherClass\0".encode_utf16().collect();
        let window_title: Vec<u16> = "HyprLauncher\0".encode_utf16().collect();

        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(launcher_wnd_proc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let win_x = (screen_w - LAUNCHER_WIDTH) / 2;
        let win_y = screen_h - 9 - LAUNCHER_HEIGHT;

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(window_title.as_ptr()),
            WS_POPUP,
            win_x,
            win_y,
            LAUNCHER_WIDTH,
            LAUNCHER_HEIGHT,
            HWND::default(),
            None,
            hinstance,
            None,
        )?;

        // Disable DWM rectangular rounding/acrylic so our custom per-pixel SDF bottom notch with concave ears has zero box artifact
        let round_pref = DWMWCP_DONOTROUND.0 as u32;
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
        ui::disable_window_acrylic(hwnd);

        LAUNCHER_HWND.store(hwnd.0 as isize, Ordering::SeqCst);

        // Install low-level keyboard & mouse hooks on a dedicated non-blocking thread so UI rendering
        // or IPC in `show_launcher_window` can NEVER stall the Windows `WH_KEYBOARD_LL` hook chain!
        std::thread::Builder::new()
            .name("hyprlauncher-hook-ll".to_string())
            .spawn(|| unsafe {
                attach_current_thread_to_default_desktop();
                let kbd_hook =
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(launcher_ll_kbd_proc), None, 0)
                        .ok();
                let mouse_hook =
                    SetWindowsHookExW(WH_MOUSE_LL, Some(launcher_ll_mouse_proc), None, 0)
                        .ok();
                let mut h_msg = MSG::default();
                while GetMessageW(&mut h_msg, HWND::default(), 0, 0).as_bool() {
                    let _ = TranslateMessage(&h_msg);
                    DispatchMessageW(&h_msg);
                }
                if let Some(kh) = kbd_hook {
                    let _ = UnhookWindowsHookEx(kh);
                }
                if let Some(mh) = mouse_hook {
                    let _ = UnhookWindowsHookEx(mh);
                }
            })
            .ok();

        if show_immediately {
            if start_clipboard_mode {
                show_launcher_window_with_query(hwnd, ":");
            } else {
                show_launcher_window(hwnd);
            }
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

unsafe extern "system" fn launcher_ll_mouse_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code < 0 || !LAUNCHER_VISIBLE.load(Ordering::SeqCst) {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let msg = wparam.0 as u32;
    if msg == WM_LBUTTONDOWN || msg == WM_RBUTTONDOWN {
        let ms = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        let hwnd_val = LAUNCHER_HWND.load(Ordering::SeqCst);
        if hwnd_val != 0 {
            let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
            let mut rc = RECT::default();
            if GetWindowRect(hwnd, &mut rc).is_ok() {
                let inside = ms.pt.x >= rc.left
                    && ms.pt.x <= rc.right
                    && ms.pt.y >= rc.top
                    && ms.pt.y <= rc.bottom;
                if !inside {
                    let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
        }
    }

    CallNextHookEx(None, code, wparam, lparam)
}

unsafe extern "system" fn launcher_ll_kbd_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // Only intercept typing when the launcher popup is actively visible on screen!
    // All global `SUPER` keybinds are handled exclusively by `hyprland-win-compositor`.
    if code < 0 || !LAUNCHER_VISIBLE.load(Ordering::SeqCst) {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
    let vk = kb.vkCode as u16;
    if vk == VK_F24.0 || vk == VK_LWIN.0 || vk == VK_RWIN.0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let msg = wparam.0 as u32;
    let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP || (kb.flags.0 & 0x80) != 0;
    let is_down = !is_up && (msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN);

    let super_down = (GetAsyncKeyState(VK_LWIN.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_RWIN.0 as i32) as u16 & 0x8000) != 0;
    let alt_down = (GetAsyncKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_LMENU.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_RMENU.0 as i32) as u16 & 0x8000) != 0;

    // If launcher is currently visible, intercept Escape / Up / Down / Tab / Backspace / Enter /
    // Ctrl shortcuts / printable characters globally in `WH_KEYBOARD_LL` so `follow_mouse = 1` never steals typing!
    if is_down && !super_down && !alt_down {
        let hwnd_val = LAUNCHER_HWND.load(Ordering::SeqCst);
        if hwnd_val != 0 {
            let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
            let ctrl_down = (GetAsyncKeyState(0x11) as u16 & 0x8000) != 0
                || (GetAsyncKeyState(0xA2) as u16 & 0x8000) != 0
                || (GetAsyncKeyState(0xA3) as u16 & 0x8000) != 0;

            if ctrl_down {
                // Ignore bare Ctrl key press itself, forward Ctrl+<key> combinations with lparam = 1
                if !matches!(vk, 0x11 | 0xA2 | 0xA3) {
                    let _ = PostMessageW(hwnd, WM_KEYDOWN, WPARAM(vk as usize), LPARAM(1));
                    return LRESULT(1);
                }
                return CallNextHookEx(None, code, wparam, lparam);
            }

            if matches!(
                vk,
                x if x == VK_ESCAPE.0
                    || x == VK_RETURN.0
                    || x == VK_UP.0
                    || x == VK_DOWN.0
                    || x == VK_TAB.0
                    || x == VK_BACK.0
                    || x == 0x2E // VK_DELETE
                    || x == 0x25 // VK_LEFT
                    || x == 0x27 // VK_RIGHT
                    || x == 0x24 // VK_HOME
                    || x == 0x23 // VK_END
            ) {
                let _ = PostMessageW(hwnd, WM_KEYDOWN, WPARAM(vk as usize), LPARAM(0));
                return LRESULT(1);
            }

            let shift = (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
            let ch_opt: Option<char> = match vk {
                0x41..=0x5A => {
                    let base = (vk as u8) - b'A';
                    Some((if shift { b'A' + base } else { b'a' + base }) as char)
                }
                0x30 => Some(if shift { ')' } else { '0' }),
                0x31 => Some(if shift { '!' } else { '1' }),
                0x32 => Some(if shift { '@' } else { '2' }),
                0x33 => Some(if shift { '#' } else { '3' }),
                0x34 => Some(if shift { '$' } else { '4' }),
                0x35 => Some(if shift { '%' } else { '5' }),
                0x36 => Some(if shift { '^' } else { '6' }),
                0x37 => Some(if shift { '&' } else { '7' }),
                0x38 => Some(if shift { '*' } else { '8' }),
                0x39 => Some(if shift { '(' } else { '9' }),
                // Numpad 0..9 and operators
                0x60..=0x69 => Some((b'0' + (vk as u8 - 0x60)) as char),
                0x6A => Some('*'), // VK_MULTIPLY
                0x6B => Some('+'), // VK_ADD
                0x6D => Some('-'), // VK_SUBTRACT
                0x6E => Some('.'), // VK_DECIMAL
                0x6F => Some('/'), // VK_DIVIDE
                x if x == VK_SPACE.0 => Some(' '),
                0xBA => Some(if shift { ':' } else { ';' }), // VK_OEM_1
                0xBB => Some(if shift { '+' } else { '=' }), // VK_OEM_PLUS
                0xBC => Some(if shift { '<' } else { ',' }), // VK_OEM_COMMA
                0xBD => Some(if shift { '_' } else { '-' }), // VK_OEM_MINUS
                0xBE => Some(if shift { '>' } else { '.' }), // VK_OEM_PERIOD
                0xBF => Some(if shift { '?' } else { '/' }), // VK_OEM_2
                0xC0 => Some(if shift { '~' } else { '`' }), // VK_OEM_3
                0xDB => Some(if shift { '{' } else { '[' }), // VK_OEM_4
                0xDC => Some(if shift { '|' } else { '\\' }), // VK_OEM_5
                0xDD => Some(if shift { '}' } else { ']' }), // VK_OEM_6
                0xDE => Some(if shift { '"' } else { '\'' }), // VK_OEM_7
                _ => None,
            };
            if let Some(ch) = ch_opt {
                let _ = PostMessageW(hwnd, WM_CHAR, WPARAM(ch as usize), LPARAM(0));
                return LRESULT(1);
            }
        }
    }

    CallNextHookEx(None, code, wparam, lparam)
}

unsafe fn find_dcomp_overlay_hwnd() -> Option<HWND> {
    let cls: Vec<u16> = "HyprlandWinNativeDCompOverlay\0".encode_utf16().collect();
    FindWindowW(PCWSTR(cls.as_ptr()), PCWSTR::null())
        .ok()
        .filter(|h| !h.0.is_null())
}

unsafe fn refresh_launcher(hwnd: HWND) {
    if let Some(st_mutex) = STATE.get() {
        if let Ok(st) = st_mutex.lock() {
            ui::render_launcher(hwnd, &st.model);
        }
    }
}

fn frecency_history_path() -> PathBuf {
    hyprland_win_core::hyprland_win_config_dir().join("launcher_history.tsv")
}

fn load_frecency_history() -> HashMap<String, u32> {
    let mut map = HashMap::new();
    if let Ok(content) = std::fs::read_to_string(frecency_history_path()) {
        for line in content.lines() {
            if let Some((k, v)) = line.split_once('\t') {
                if let Ok(cnt) = v.trim().parse::<u32>() {
                    map.insert(k.trim().to_ascii_lowercase(), cnt);
                }
            }
        }
    }
    map
}

fn record_frecency_hit(frecency: &mut HashMap<String, u32>, key: &str) {
    let norm = key.trim().to_ascii_lowercase();
    if norm.is_empty() {
        return;
    }
    let entry = frecency.entry(norm).or_insert(0);
    *entry = entry.saturating_add(1);

    let path = frecency_history_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut out = String::new();
    for (k, v) in frecency.iter() {
        out.push_str(&format!("{k}\t{v}\n"));
    }
    let _ = std::fs::write(path, out);
}

unsafe fn show_launcher_window(hwnd: HWND) {
    show_launcher_window_with_query(hwnd, "");
}

unsafe fn show_launcher_window_with_query(hwnd: HWND, initial_query: &str) {
    let (screen_w, screen_h) = STATE
        .get()
        .and_then(|m| m.lock().ok().map(|s| (s.screen_w, s.screen_h)))
        .unwrap_or((3440, 1440));

    let win_x = (screen_w - LAUNCHER_WIDTH) / 2;
    let win_y = screen_h - 9 - LAUNCHER_HEIGHT;

    // Query live open windows ONCE on popup open so keystroke filtering is 100% zero-IPC!
    let live_windows = query_open_window_candidates();

    if let Some(st_mutex) = STATE.get() {
        if let Ok(mut st) = st_mutex.lock() {
            st.win_x = win_x;
            st.win_y = win_y;
            st.open_windows = live_windows;
            st.model.query = initial_query.to_string();
            st.model.query_selected = false;
            st.model.selected_index = 0;
            st.model.hovered_index = None;
            recompute_filtered_results(&mut st);
            ui::reset_open_animation(&mut st.model);
            st.last_anim_tick = Instant::now();
        }
    }

    if let Some(ov_hwnd) = find_dcomp_overlay_hwnd() {
        let _ = SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, ov_hwnd.0 as isize);
    }
    ui::set_overlay_launcher_cutout(
        screen_w,
        screen_h,
        Some((win_x, win_y, LAUNCHER_WIDTH, LAUNCHER_HEIGHT)),
    );

    let round_pref = DWMWCP_DONOTROUND.0 as u32;
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

    ui::disable_window_acrylic(hwnd);
    refresh_launcher(hwnd);

    LAUNCHER_VISIBLE.store(true, Ordering::SeqCst);
    let _ = AllowSetForegroundWindow(ASFW_ANY);
    let _ = SetWindowPos(
        hwnd,
        HWND_TOPMOST,
        win_x,
        win_y,
        LAUNCHER_WIDTH,
        LAUNCHER_HEIGHT,
        SWP_SHOWWINDOW,
    );
    let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
    let _ = BringWindowToTop(hwnd);
    let _ = SetForegroundWindow(hwnd);
    let _ = SetFocus(hwnd);
    let _ = SetTimer(hwnd, TIMER_ID_ANIM, 7, None);
}

unsafe fn hide_launcher_window(hwnd: HWND) {
    if !LAUNCHER_VISIBLE.swap(false, Ordering::SeqCst) {
        return;
    }
    if let Some(st_mutex) = STATE.get() {
        if let Ok(mut st) = st_mutex.lock() {
            st.model.closing = true;
            st.last_anim_tick = Instant::now();
        }
    }
    let _ = SetTimer(hwnd, TIMER_ID_ANIM, 7, None);
}

unsafe fn finish_hide_launcher_window(hwnd: HWND) {
    let _ = KillTimer(hwnd, TIMER_ID_ANIM);
    let _ = ShowWindow(hwnd, SW_HIDE);
    let (screen_w, screen_h) = STATE
        .get()
        .and_then(|m| m.lock().ok().map(|s| (s.screen_w, s.screen_h)))
        .unwrap_or((3440, 1440));
    ui::set_overlay_launcher_cutout(screen_w, screen_h, None);
}

unsafe fn execute_selected_candidate(hwnd: HWND) {
    let chosen = STATE.get().and_then(|m| {
        let mut st = m.lock().ok()?;
        let cand = st.filtered.get(st.model.selected_index).cloned()?;
        record_frecency_hit(&mut st.frecency, &cand.title);
        Some(cand)
    });

    hide_launcher_window(hwnd);

    if let Some(cand) = chosen {
        match cand.action {
            CandidateAction::SwitchToWindow {
                hwnd: target_hwnd,
                workspace_id,
            } => {
                let _ = send_ipc_request(&HyprIpcRequest::Dispatch {
                    dispatcher: "workspace".to_string(),
                    arg: workspace_id.to_string(),
                });
                let win_h = HWND(target_hwnd as *mut core::ffi::c_void);
                let _ = AllowSetForegroundWindow(ASFW_ANY);
                let _ = BringWindowToTop(win_h);
                let _ = SetForegroundWindow(win_h);
            }
            CandidateAction::LaunchApp { exec_path, args } => {
                spawn_unelevated_command(&exec_path, &args);
            }
            CandidateAction::CopyMathResult { result } => {
                copy_utf16_text_to_clipboard(&result);
            }
            CandidateAction::CopyClipboardEntry { text } => {
                copy_utf16_text_to_clipboard(&text);
            }
            CandidateAction::RunShellCommand { cmd } => {
                spawn_unelevated_command("wt.exe", &format!("powershell -NoExit -Command \"{}\"", cmd.replace('"', "\\\"")));
            }
            CandidateAction::OpenWebUrl { url } => {
                spawn_unelevated_command(&url, "");
            }
            CandidateAction::HyprDispatch { dispatcher, arg } => {
                let _ = send_ipc_request(&HyprIpcRequest::Dispatch { dispatcher, arg });
            }
        }
    }
}

unsafe fn is_token_elevated(h_tok: windows::Win32::Foundation::HANDLE) -> bool {
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION};
    let mut elev = TOKEN_ELEVATION::default();
    let mut ret_len = 0u32;
    if GetTokenInformation(
        h_tok,
        TokenElevation,
        Some(&mut elev as *mut _ as *mut _),
        std::mem::size_of::<TOKEN_ELEVATION>() as u32,
        &mut ret_len,
    )
    .is_ok()
    {
        elev.TokenIsElevated != 0
    } else {
        true
    }
}

unsafe fn try_duplicate_unelevated_pid_token(
    pid: u32,
) -> Option<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        DuplicateTokenEx, SecurityImpersonation, TokenPrimary, TOKEN_ALL_ACCESS,
        TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return None;
    }
    let h_proc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut h_tok = HANDLE::default();
    let mut out_dup = None;
    if OpenProcessToken(
        h_proc,
        TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY | TOKEN_QUERY,
        &mut h_tok,
    )
    .is_ok()
    {
        if !is_token_elevated(h_tok) {
            let mut h_dup = HANDLE::default();
            if DuplicateTokenEx(
                h_tok,
                TOKEN_ALL_ACCESS,
                None,
                SecurityImpersonation,
                TokenPrimary,
                &mut h_dup,
            )
            .is_ok()
            {
                out_dup = Some(h_dup);
            }
        }
        let _ = CloseHandle(h_tok);
    }
    let _ = CloseHandle(h_proc);
    out_dup
}

/// Acquires a primary token for the current interactive user that is guaranteed to be
/// non-elevated (`TokenIsElevated == 0` / Medium Integrity).
/// This is essential for WebView2 / Chromium applications (such as `henbrowser.exe`),
/// which fail with `HRESULT(0x80070490)` if spawned with an Elevated Administrator token.
unsafe fn acquire_unelevated_session_token() -> Option<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

    #[link(name = "kernel32")]
    extern "system" {
        fn K32EnumProcesses(lpidprocess: *mut u32, cb: u32, lpcbneeded: *mut u32) -> i32;
    }

    // 1. Fast window-class check (excluding `Shell_TrayWnd` if hosted by elevated `hyprbar.exe`)
    for cls in ["Progman\0", "CASCADIA_HOSTING_WINDOW_CLASS\0"] {
        let cls_u16: Vec<u16> = cls.encode_utf16().collect();
        if let Ok(sh_hwnd) = FindWindowW(PCWSTR(cls_u16.as_ptr()), PCWSTR::null()) {
            if !sh_hwnd.0.is_null() {
                let mut pid: u32 = 0;
                GetWindowThreadProcessId(sh_hwnd, Some(&mut pid));
                if let Some(h_dup) = try_duplicate_unelevated_pid_token(pid) {
                    return Some(h_dup);
                }
            }
        }
    }

    // 2. Scan active user-session processes (`sihost.exe`, `ctfmon.exe`, `explorer.exe`, `RuntimeBroker.exe`, `taskhostw.exe`)
    // which `winlogon.exe` / `userinit.exe` always run at Medium Integrity (`TokenIsElevated == 0`)
    let mut pids = [0u32; 1024];
    let mut bytes_returned = 0u32;
    if K32EnumProcesses(
        pids.as_mut_ptr(),
        std::mem::size_of_val(&pids) as u32,
        &mut bytes_returned,
    ) != 0
    {
        let count = (bytes_returned as usize / std::mem::size_of::<u32>()).min(pids.len());
        for &pid in &pids[..count] {
            if pid <= 4 {
                continue;
            }
            if let Ok(h_proc) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                let mut buf = [0u16; 260];
                let mut len = buf.len() as u32;
                let ok = QueryFullProcessImageNameW(
                    h_proc,
                    PROCESS_NAME_WIN32,
                    windows::core::PWSTR(buf.as_mut_ptr()),
                    &mut len,
                )
                .is_ok();
                let _ = CloseHandle(h_proc);
                if ok && len > 0 {
                    let full = String::from_utf16_lossy(&buf[..len as usize]);
                    let base = full.rsplit(['\\', '/']).next().unwrap_or("").to_ascii_lowercase();
                    if matches!(
                        base.as_str(),
                        "sihost.exe"
                            | "ctfmon.exe"
                            | "explorer.exe"
                            | "runtimebroker.exe"
                            | "taskhostw.exe"
                            | "wt.exe"
                    ) {
                        if let Some(h_dup) = try_duplicate_unelevated_pid_token(pid) {
                            return Some(h_dup);
                        }
                    }
                }
            }
        }
    }

    None
}

/// Launch an application or `.lnk` shortcut as the normal interactive user (Medium Integrity)
/// even when `hyprland-win.exe` / `hyprlauncher.exe` is running with Administrator privileges,
/// unless the user explicitly launches Task Manager (`taskmgr.exe`).
unsafe fn spawn_unelevated_command(exec_path: &str, args: &str) {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Threading::{
        CreateProcessW, CreateProcessWithTokenW, GetCurrentProcess, OpenProcessToken,
        PROCESS_INFORMATION, STARTUPINFOW,
    };

    // If `exec_path` is a `.lnk` shortcut, resolve its underlying `.exe` target so we can
    // launch it directly at Medium Integrity with its working directory!
    let mut resolved_target = if exec_path.to_ascii_lowercase().ends_with(".lnk") {
        resolve_lnk_exe_target(Path::new(exec_path)).unwrap_or_else(|| exec_path.to_string())
    } else {
        exec_path.to_string()
    };

    let check_lower = resolved_target.to_ascii_lowercase();
    let is_explorer = check_lower == "explorer"
        || check_lower == "explorer.exe"
        || check_lower.ends_with("\\explorer.exe")
        || check_lower.ends_with("/explorer.exe");

    let effective_args = if is_explorer {
        let sys_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        resolved_target = format!("{sys_root}\\explorer.exe");
        if args.trim().is_empty() {
            "/NoShellRegistrationCheck /e,::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".to_string()
        } else if !args
            .to_ascii_lowercase()
            .contains("/noshellregistrationcheck")
        {
            format!("/NoShellRegistrationCheck {}", args.trim())
        } else {
            args.trim().to_string()
        }
    } else {
        args.to_string()
    };
    let args = effective_args.as_str();

    let lower = resolved_target.to_ascii_lowercase();
    let is_drive_path = matches!(
        lower.as_bytes(),
        [b'a'..=b'z', b':', b'\\' | b'/', ..]
    );
    let is_uri = lower.contains(':') && !is_drive_path;
    let is_unresolved_lnk = lower.ends_with(".lnk");

    let self_elevated = {
        let mut h_self_tok = HANDLE::default();
        if OpenProcessToken(
            GetCurrentProcess(),
            windows::Win32::Security::TOKEN_QUERY,
            &mut h_self_tok,
        )
        .is_ok()
        {
            let elev = is_token_elevated(h_self_tok);
            let _ = CloseHandle(h_self_tok);
            elev
        } else {
            false
        }
    };

    // Attempt Medium-Integrity token inheritance (`TokenIsElevated == 0`)
    if !is_uri && !is_unresolved_lnk && !lower.contains("taskmgr") {
        let cmd_str = if args.is_empty() {
            format!("\"{}\"", resolved_target)
        } else {
            format!("\"{}\" {}", resolved_target, args)
        };
        let work_dir_u16: Option<Vec<u16>> = Path::new(&resolved_target)
            .parent()
            .filter(|p| !p.as_os_str().is_empty() && p.is_dir())
            .map(|p| p.to_string_lossy().encode_utf16().chain(Some(0)).collect());
        let work_dir_ptr = work_dir_u16
            .as_ref()
            .map(|v| PCWSTR(v.as_ptr()))
            .unwrap_or(PCWSTR::null());

        if self_elevated {
            if let Some(h_dup) = acquire_unelevated_session_token() {
                let mut cmd_u16: Vec<u16> = cmd_str.encode_utf16().chain(Some(0)).collect();
                let si = STARTUPINFOW {
                    cb: std::mem::size_of::<STARTUPINFOW>() as u32,
                    ..Default::default()
                };
                let mut pi = PROCESS_INFORMATION::default();
                let ok = CreateProcessWithTokenW(
                    h_dup,
                    Default::default(),
                    PCWSTR::null(),
                    windows::core::PWSTR(cmd_u16.as_mut_ptr()),
                    Default::default(),
                    None,
                    work_dir_ptr,
                    &si,
                    &mut pi,
                )
                .is_ok();
                let _ = CloseHandle(pi.hThread);
                let _ = CloseHandle(pi.hProcess);
                let _ = CloseHandle(h_dup);
                if ok {
                    return;
                }
            }
        }

        // If `hyprlauncher.exe` is already unelevated (or token duplication fallback),
        // launch direct `.exe` targets (especially `explorer.exe /NoShellRegistrationCheck`)
        // via `CreateProcessW` so `ShellExecuteW` never invokes the desktop shell over DDE/COM!
        if is_explorer || Path::new(&resolved_target).is_file() {
            let mut cmd_u16: Vec<u16> = cmd_str.encode_utf16().chain(Some(0)).collect();
            let si = STARTUPINFOW {
                cb: std::mem::size_of::<STARTUPINFOW>() as u32,
                ..Default::default()
            };
            let mut pi = PROCESS_INFORMATION::default();
            let ok = CreateProcessW(
                PCWSTR::null(),
                windows::core::PWSTR(cmd_u16.as_mut_ptr()),
                None,
                None,
                false,
                Default::default(),
                None,
                work_dir_ptr,
                &si,
                &mut pi,
            )
            .is_ok();
            let _ = CloseHandle(pi.hThread);
            let _ = CloseHandle(pi.hProcess);
            if ok {
                return;
            }
        }
    }

    // Standard ShellExecuteW ("open" verb — never "runas") with explicit working directory
    let op: Vec<u16> = "open\0".encode_utf16().collect();
    let file: Vec<u16> = resolved_target.encode_utf16().chain(Some(0)).collect();
    let params: Vec<u16> = args.encode_utf16().chain(Some(0)).collect();
    let params_ptr = if args.is_empty() {
        PCWSTR::null()
    } else {
        PCWSTR(params.as_ptr())
    };
    let work_dir_u16: Option<Vec<u16>> = Path::new(&resolved_target)
        .parent()
        .filter(|p| !p.as_os_str().is_empty() && p.is_dir())
        .map(|p| p.to_string_lossy().encode_utf16().chain(Some(0)).collect());
    let work_dir_ptr = work_dir_u16
        .as_ref()
        .map(|v| PCWSTR(v.as_ptr()))
        .unwrap_or(PCWSTR::null());

    let _ = ShellExecuteW(
        HWND::default(),
        PCWSTR(op.as_ptr()),
        PCWSTR(file.as_ptr()),
        params_ptr,
        work_dir_ptr,
        SW_SHOWNORMAL,
    );
}

unsafe fn copy_utf16_text_to_clipboard(text: &str) {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

    const CF_UNICODETEXT_VAL: u32 = 13;
    let u16s: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let byte_len = u16s.len() * 2;
    if let Ok(hmem) = GlobalAlloc(GMEM_MOVEABLE, byte_len) {
        let ptr = GlobalLock(hmem);
        if !ptr.is_null() {
            std::ptr::copy_nonoverlapping(u16s.as_ptr() as *const u8, ptr as *mut u8, byte_len);
            let _ = GlobalUnlock(hmem);
            if OpenClipboard(HWND::default()).is_ok() {
                let _ = EmptyClipboard();
                let _ = SetClipboardData(CF_UNICODETEXT_VAL, HANDLE(hmem.0));
                let _ = CloseClipboard();
            }
        }
    }
}

unsafe fn read_utf16_text_from_clipboard() -> Option<String> {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};

    const CF_UNICODETEXT_VAL: u32 = 13;
    if IsClipboardFormatAvailable(CF_UNICODETEXT_VAL).is_err() {
        return None;
    }
    if OpenClipboard(HWND::default()).is_err() {
        return None;
    }
    let mut out = None;
    if let Ok(handle) = GetClipboardData(CF_UNICODETEXT_VAL) {
        if !handle.0.is_null() {
            let hmem = HGLOBAL(handle.0);
            let ptr = GlobalLock(hmem) as *const u16;
            if !ptr.is_null() {
                let mut len = 0usize;
                while len < 4096 && *ptr.add(len) != 0 {
                    len += 1;
                }
                let slice = std::slice::from_raw_parts(ptr, len);
                let raw = String::from_utf16_lossy(slice);
                let cleaned: String = raw
                    .chars()
                    .filter(|&c| c >= ' ' && c != '\r' && c != '\n')
                    .collect();
                if !cleaned.is_empty() {
                    out = Some(cleaned);
                }
                let _ = GlobalUnlock(hmem);
            }
        }
    }
    let _ = CloseClipboard();
    out
}

fn spawn_clipboard_history_watcher() {
    let _ = CLIPBOARD_HISTORY.set(Mutex::new(Vec::new()));
    let _ = std::thread::Builder::new()
        .name("hyprlauncher-cliphist".to_string())
        .spawn(|| loop {
            if let Some(txt) = unsafe { read_utf16_text_from_clipboard() } {
                let trimmed = txt.trim();
                if !trimmed.is_empty() {
                    if let Some(m) = CLIPBOARD_HISTORY.get() {
                        if let Ok(mut hist) = m.lock() {
                            if hist.first().map(|s| s.as_str()) != Some(trimmed) {
                                hist.retain(|s| s != trimmed);
                                hist.insert(0, trimmed.to_string());
                                hist.truncate(35);
                            }
                        }
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(650));
        });
}

fn delete_previous_word(s: &mut String) {
    while s.ends_with(|c: char| c.is_whitespace()) {
        s.pop();
    }
    if s.ends_with(|c: char| !c.is_alphanumeric() && !c.is_whitespace()) {
        while s.ends_with(|c: char| !c.is_alphanumeric() && !c.is_whitespace()) {
            s.pop();
        }
    } else {
        while s.ends_with(|c: char| c.is_alphanumeric()) {
            s.pop();
        }
    }
}

unsafe extern "system" fn launcher_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_LAUNCHER_TOGGLE => {
            if LAUNCHER_VISIBLE.load(Ordering::SeqCst) {
                hide_launcher_window(hwnd);
            } else {
                show_launcher_window(hwnd);
            }
            LRESULT(0)
        }
        WM_LAUNCHER_CLIPBOARD => {
            show_launcher_window_with_query(hwnd, ":");
            LRESULT(0)
        }
        WM_ACTIVATE => {
            // Do NOT hide on `WA_INACTIVE`: `Hyprland-Win` has `follow_mouse = 1` which changes
            // foreground focus whenever the mouse moves over a tiled window!
            // Instead, `launcher_ll_mouse_proc` closes the launcher only on an actual outside click.
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let vk = wparam.0 as u16;
            let ctrl_down = lparam.0 == 1
                || (GetAsyncKeyState(0x11) as u16 & 0x8000) != 0
                || (GetAsyncKeyState(0xA2) as u16 & 0x8000) != 0
                || (GetAsyncKeyState(0xA3) as u16 & 0x8000) != 0;
            let shift_down = (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;

            if ctrl_down {
                match vk {
                    // Ctrl+A: Select all text in query inputbar
                    0x41 => {
                        if let Some(st_mutex) = STATE.get() {
                            if let Ok(mut st) = st_mutex.lock() {
                                st.model.query_selected = !st.model.query.is_empty();
                            }
                        }
                        refresh_launcher(hwnd);
                        return LRESULT(0);
                    }
                    // Ctrl+Back or Ctrl+W: Delete previous word (or clear if selected)
                    x if x == VK_BACK.0 || x == 0x57 => {
                        if let Some(st_mutex) = STATE.get() {
                            if let Ok(mut st) = st_mutex.lock() {
                                if st.model.query_selected {
                                    st.model.query.clear();
                                    st.model.query_selected = false;
                                } else {
                                    delete_previous_word(&mut st.model.query);
                                }
                                recompute_filtered_results(&mut st);
                            }
                        }
                        refresh_launcher(hwnd);
                        return LRESULT(0);
                    }
                    // Ctrl+U or Ctrl+Delete: Clear entire query
                    0x55 | 0x2E => {
                        if let Some(st_mutex) = STATE.get() {
                            if let Ok(mut st) = st_mutex.lock() {
                                st.model.query.clear();
                                st.model.query_selected = false;
                                recompute_filtered_results(&mut st);
                            }
                        }
                        refresh_launcher(hwnd);
                        return LRESULT(0);
                    }
                    // Ctrl+V: Paste from clipboard
                    0x56 => {
                        if let Some(pasted) = read_utf16_text_from_clipboard() {
                            if let Some(st_mutex) = STATE.get() {
                                if let Ok(mut st) = st_mutex.lock() {
                                    if st.model.query_selected {
                                        st.model.query = pasted;
                                        st.model.query_selected = false;
                                    } else {
                                        st.model.query.push_str(&pasted);
                                    }
                                    recompute_filtered_results(&mut st);
                                }
                            }
                            refresh_launcher(hwnd);
                        }
                        return LRESULT(0);
                    }
                    // Ctrl+C: Copy query (or math result if present) to clipboard
                    0x43 => {
                        let to_copy = STATE.get().and_then(|m| {
                            let st = m.lock().ok()?;
                            if !st.model.query.is_empty() {
                                Some(st.model.query.clone())
                            } else {
                                None
                            }
                        });
                        if let Some(txt) = to_copy {
                            copy_utf16_text_to_clipboard(&txt);
                        }
                        return LRESULT(0);
                    }
                    // Ctrl+X: Cut query to clipboard
                    0x58 => {
                        let to_cut = STATE.get().and_then(|m| {
                            let mut st = m.lock().ok()?;
                            if !st.model.query.is_empty() {
                                let q = std::mem::take(&mut st.model.query);
                                st.model.query_selected = false;
                                recompute_filtered_results(&mut st);
                                Some(q)
                            } else {
                                None
                            }
                        });
                        if let Some(txt) = to_cut {
                            copy_utf16_text_to_clipboard(&txt);
                            refresh_launcher(hwnd);
                        }
                        return LRESULT(0);
                    }
                    // Ctrl+J / Ctrl+N: Move selection down
                    0x4A | 0x4E => {
                        if let Some(st_mutex) = STATE.get() {
                            if let Ok(mut st) = st_mutex.lock() {
                                let count = st.model.rows.len();
                                if count > 0 {
                                    st.model.selected_index = (st.model.selected_index + 1) % count;
                                }
                            }
                        }
                        refresh_launcher(hwnd);
                        return LRESULT(0);
                    }
                    // Ctrl+K / Ctrl+P: Move selection up
                    0x4B | 0x50 => {
                        if let Some(st_mutex) = STATE.get() {
                            if let Ok(mut st) = st_mutex.lock() {
                                let count = st.model.rows.len();
                                if count > 0 {
                                    st.model.selected_index = if st.model.selected_index == 0 {
                                        count - 1
                                    } else {
                                        st.model.selected_index - 1
                                    };
                                }
                            }
                        }
                        refresh_launcher(hwnd);
                        return LRESULT(0);
                    }
                    _ => return LRESULT(0),
                }
            }

            if vk == VK_ESCAPE.0 {
                hide_launcher_window(hwnd);
                return LRESULT(0);
            }
            if vk == VK_RETURN.0 {
                execute_selected_candidate(hwnd);
                return LRESULT(0);
            }
            if vk == VK_DOWN.0 || (vk == VK_TAB.0 && !shift_down) {
                if let Some(st_mutex) = STATE.get() {
                    if let Ok(mut st) = st_mutex.lock() {
                        let count = st.model.rows.len();
                        if count > 0 {
                            st.model.selected_index = (st.model.selected_index + 1) % count;
                        }
                    }
                }
                refresh_launcher(hwnd);
                return LRESULT(0);
            }
            if vk == VK_UP.0 || (vk == VK_TAB.0 && shift_down) {
                if let Some(st_mutex) = STATE.get() {
                    if let Ok(mut st) = st_mutex.lock() {
                        let count = st.model.rows.len();
                        if count > 0 {
                            st.model.selected_index = if st.model.selected_index == 0 {
                                count - 1
                            } else {
                                st.model.selected_index - 1
                            };
                        }
                    }
                }
                refresh_launcher(hwnd);
                return LRESULT(0);
            }
            // Left / Right / Home / End: unselect query highlight
            if matches!(vk, 0x25 | 0x27 | 0x24 | 0x23) {
                if let Some(st_mutex) = STATE.get() {
                    if let Ok(mut st) = st_mutex.lock() {
                        if st.model.query_selected {
                            st.model.query_selected = false;
                        }
                    }
                }
                refresh_launcher(hwnd);
                return LRESULT(0);
            }
            if vk == VK_BACK.0 || vk == 0x2E {
                if let Some(st_mutex) = STATE.get() {
                    if let Ok(mut st) = st_mutex.lock() {
                        if st.model.query_selected {
                            st.model.query.clear();
                            st.model.query_selected = false;
                        } else if vk == VK_BACK.0 {
                            st.model.query.pop();
                        }
                        recompute_filtered_results(&mut st);
                    }
                }
                refresh_launcher(hwnd);
                return LRESULT(0);
            }
            LRESULT(0)
        }
        WM_CHAR => {
            let ch = (wparam.0 as u32) as u8 as char;
            if ch >= ' ' && ch != '\x7f' {
                if let Some(st_mutex) = STATE.get() {
                    if let Ok(mut st) = st_mutex.lock() {
                        if st.model.query_selected {
                            st.model.query.clear();
                            st.model.query_selected = false;
                        }
                        st.model.query.push(ch);
                        recompute_filtered_results(&mut st);
                    }
                }
                refresh_launcher(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut changed = false;
            if let Some(st_mutex) = STATE.get() {
                if let Ok(mut st) = st_mutex.lock() {
                    let hit = ui::hit_test_row(&st.model, x, y);
                    if st.model.hovered_index != hit {
                        st.model.hovered_index = hit;
                        if let Some(idx) = hit {
                            st.model.selected_index = idx;
                        }
                        changed = true;
                    }
                }
            }
            if changed {
                refresh_launcher(hwnd);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut clicked_row = false;
            if let Some(st_mutex) = STATE.get() {
                if let Ok(mut st) = st_mutex.lock() {
                    if let Some(idx) = ui::hit_test_row(&st.model, x, y) {
                        st.model.selected_index = idx;
                        clicked_row = true;
                    }
                }
            }
            if clicked_row {
                execute_selected_candidate(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) as i16) as i32;
            if let Some(st_mutex) = STATE.get() {
                if let Ok(mut st) = st_mutex.lock() {
                    let count = st.model.rows.len();
                    if count > 0 {
                        if delta < 0 {
                            st.model.selected_index = (st.model.selected_index + 1) % count;
                        } else {
                            st.model.selected_index = if st.model.selected_index == 0 {
                                count - 1
                            } else {
                                st.model.selected_index - 1
                            };
                        }
                    }
                }
            }
            refresh_launcher(hwnd);
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            refresh_launcher(hwnd);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_ID_ANIM {
                let mut should_repaint = false;
                let mut should_finish_hide = false;

                if let Some(st_mutex) = STATE.get() {
                    if let Ok(mut st) = st_mutex.lock() {
                        let now = Instant::now();
                        let dt = now.duration_since(st.last_anim_tick).as_secs_f32();
                        st.last_anim_tick = now;

                        let (dirty, hide_done) = ui::step_launcher_animations(&mut st.model, dt);
                        should_repaint = dirty;
                        should_finish_hide = hide_done;
                    }
                }

                if should_finish_hide {
                    finish_hide_launcher_window(hwnd);
                } else if should_repaint {
                    refresh_launcher(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            hide_launcher_window(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn send_ipc_request(req: &HyprIpcRequest) -> Option<HyprIpcResponse> {
    let mut pipe_opt = None;
    for _ in 0..3 {
        if let Ok(p) = OpenOptions::new()
            .read(true)
            .write(true)
            .open(HYPRLAND_WIN_IPC_PIPE)
        {
            pipe_opt = Some(p);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let mut pipe = pipe_opt?;
    let bytes = serde_json::to_vec(req).ok()?;
    pipe.write_all(&bytes).ok()?;
    pipe.flush().ok()?;

    let mut resp_buf = Vec::with_capacity(16384);
    let mut chunk = [0u8; 8192];
    loop {
        let n = pipe.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        resp_buf.extend_from_slice(&chunk[..n]);
        if let Ok(resp) = serde_json::from_slice::<HyprIpcResponse>(&resp_buf) {
            return Some(resp);
        }
    }
    serde_json::from_slice::<HyprIpcResponse>(&resp_buf).ok()
}

fn query_open_window_candidates() -> Vec<LauncherCandidate> {
    let mut out = Vec::new();
    if let Some(resp) = send_ipc_request(&HyprIpcRequest::Clients { json: true }) {
        if resp.ok {
            if let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&resp.payload) {
                for item in items {
                    let id = item.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                    let ws = item
                        .get("workspace_id")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(1) as u32;
                    let title = item
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    let exe = item
                        .get("exe_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if id == 0 || (title.is_empty() && exe.is_empty()) {
                        continue;
                    }
                    let display_title = if title.is_empty() {
                        exe.clone()
                    } else {
                        title.clone()
                    };
                    let search_key = format!("{} {} ws{}", display_title, exe, ws).to_ascii_lowercase();
                    out.push(LauncherCandidate {
                        title: display_title,
                        subtitle: exe,
                        badge: format!("WS {ws}"),
                        search_key,
                        icon_hwnd: id,
                        icon_path: String::new(),
                        action: CandidateAction::SwitchToWindow {
                            hwnd: id,
                            workspace_id: ws,
                        },
                    });
                }
            }
        }
    }
    out
}

fn scan_installed_applications() -> Vec<LauncherCandidate> {
    let mut apps = Vec::new();
    let mut seen_titles = std::collections::HashSet::new();

    // 1. Core fast-access developer & system tools
    let builtins = [
        ("Windows Terminal", "wt.exe", "Command-line shell and terminal emulator"),
        ("File Explorer", "explorer.exe", "Browse and manage local files and folders"),
        ("Task Manager", "taskmgr.exe", "Monitor system performance and running processes"),
        ("PowerShell", "powershell.exe", "Task automation and configuration management shell"),
        ("Notepad", "notepad.exe", "Edit plain text files and scripts"),
        ("Calculator", "calc.exe", "Perform standard and scientific calculations"),
        ("System Settings", "ms-settings:", "Configure Windows system and device preferences"),
        (
            "HyprLock (Lock Screen)",
            "c:\\Users\\mbura\\Projects\\Other\\hyprland-win\\target\\release\\hyprlock.exe",
            "Lock the current Hyprland-Win desktop session",
        ),
        (
            "HyprShot (Region Screenshot)",
            "c:\\Users\\mbura\\Projects\\Other\\hyprland-win\\target\\release\\hyprshot.exe",
            "Capture a screen region to clipboard and disk",
        ),
    ];
    for (name, exe, sub) in builtins {
        seen_titles.insert(name.to_ascii_lowercase());
        let default_args = if exe.eq_ignore_ascii_case("explorer.exe") {
            "/NoShellRegistrationCheck /e,::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".to_string()
        } else {
            String::new()
        };
        apps.push(LauncherCandidate {
            title: name.to_string(),
            subtitle: sub.to_string(),
            badge: "APP".to_string(),
            search_key: format!("{} {} {}", name, exe, sub).to_ascii_lowercase(),
            icon_hwnd: 0,
            icon_path: exe.to_string(),
            action: CandidateAction::LaunchApp {
                exec_path: exe.to_string(),
                args: default_args,
            },
        });
    }

    // 1b. Built-in Hyprland-Win System & Compositor Commands
    let hypr_commands = [
        (
            "Hyprland-Win: Toggle Tiling / Floating",
            "Toggle active window between tiled and floating state (SUPER+V)",
            "HYPR",
            "togglefloating",
            "",
        ),
        (
            "Hyprland-Win: Toggle Fullscreen",
            "Toggle active window fullscreen mode (SUPER+F)",
            "HYPR",
            "fullscreen",
            "0",
        ),
        (
            "Hyprland-Win: Layout Dwindle",
            "Switch active workspace layout to BSP Dwindle",
            "HYPR",
            "layout",
            "dwindle",
        ),
        (
            "Hyprland-Win: Layout Master",
            "Switch active workspace layout to Master-Stack",
            "HYPR",
            "layout",
            "master",
        ),
    ];
    for (title, sub, badge, disp, arg) in hypr_commands {
        seen_titles.insert(title.to_ascii_lowercase());
        apps.push(LauncherCandidate {
            title: title.to_string(),
            subtitle: sub.to_string(),
            badge: badge.to_string(),
            search_key: format!("{} {} {}", title, sub, disp).to_ascii_lowercase(),
            icon_hwnd: 0,
            icon_path: "explorer.exe".to_string(),
            action: CandidateAction::HyprDispatch {
                dispatcher: disp.to_string(),
                arg: arg.to_string(),
            },
        });
    }

    // 2. Recursively scan Start Menu Programs folders (User & Common)
    let mut roots = Vec::new();
    if let Ok(appdata) = std::env::var("APPDATA") {
        roots.push(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Ok(progdata) = std::env::var("PROGRAMDATA") {
        roots.push(PathBuf::from(progdata).join(r"Microsoft\Windows\Start Menu\Programs"));
    }

    for root in roots {
        scan_shortcut_dir(&root, &mut apps, &mut seen_titles);
    }

    apps
}

fn scan_shortcut_dir(
    dir: &Path,
    out: &mut Vec<LauncherCandidate>,
    seen: &mut std::collections::HashSet<String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_shortcut_dir(&path, out, seen);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if ext.eq_ignore_ascii_case("lnk") || ext.eq_ignore_ascii_case("exe") {
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let lower = stem.to_ascii_lowercase();
                if lower.contains("uninstall") || lower.contains("readme") || lower.contains("help") {
                    continue;
                }
                if !seen.insert(lower.clone()) {
                    continue;
                }
                let full_path = path.to_string_lossy().to_string();
                let clean_icon_path = if ext.eq_ignore_ascii_case("lnk") {
                    resolve_lnk_icon_target(&path).unwrap_or_else(|| full_path.clone())
                } else {
                    full_path.clone()
                };
                let parent_cat = path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("Application")
                    .to_string();

                out.push(LauncherCandidate {
                    title: stem.to_string(),
                    subtitle: parent_cat.clone(),
                    badge: "APP".to_string(),
                    search_key: format!("{} {}", lower, parent_cat.to_ascii_lowercase()),
                    icon_hwnd: 0,
                    icon_path: clean_icon_path,
                    action: CandidateAction::LaunchApp {
                        exec_path: full_path,
                        args: String::new(),
                    },
                });
            }
        }
    }
}

/// Extract the underlying `.exe` target path from a Windows `.lnk` binary (`[MS-SHLLINK]`)
/// so `spawn_unelevated_command` can launch the target binary directly at Medium Integrity.
fn resolve_lnk_exe_target(lnk_path: &Path) -> Option<String> {
    let bytes = std::fs::read(lnk_path).ok()?;
    if bytes.len() < 76 {
        return None;
    }

    let mut best_match: Option<String> = None;
    for i in 0..bytes.len().saturating_sub(6) {
        let b0 = bytes[i];
        if b0.is_ascii_alphabetic() && bytes[i + 1] == b':' && bytes[i + 2] == b'\\' {
            let mut end = i;
            while end < bytes.len()
                && bytes[end] >= 0x20
                && bytes[end] < 0x7F
                && !matches!(bytes[end], b'"' | b'<' | b'>' | b'|')
            {
                end += 1;
            }
            if let Ok(s) = std::str::from_utf8(&bytes[i..end]) {
                let s_trim = s.trim();
                let lower = s_trim.to_ascii_lowercase();
                if lower.ends_with(".exe")
                    && !lower.contains("cmd.exe")
                    && Path::new(s_trim).is_file()
                {
                    best_match = Some(s_trim.to_string());
                }
            }
        }
    }
    if best_match.is_some() {
        return best_match;
    }

    for offset in [0usize, 1usize] {
        let u16s: Vec<u16> = bytes[offset..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        for i in 0..u16s.len().saturating_sub(6) {
            let c0 = u16s[i];
            if ((c0 >= b'A' as u16 && c0 <= b'Z' as u16)
                || (c0 >= b'a' as u16 && c0 <= b'z' as u16))
                && u16s[i + 1] == b':' as u16
                && u16s[i + 2] == b'\\' as u16
            {
                let mut end = i;
                while end < u16s.len()
                    && u16s[end] >= 0x20
                    && u16s[end] < 0xFFFE
                    && !matches!(u16s[end] as u8, b'"' | b'<' | b'>' | b'|')
                {
                    end += 1;
                }
                let s = String::from_utf16_lossy(&u16s[i..end]);
                let s_trim = s.trim();
                let lower = s_trim.to_ascii_lowercase();
                if lower.ends_with(".exe")
                    && !lower.contains("cmd.exe")
                    && Path::new(s_trim).is_file()
                {
                    best_match = Some(s_trim.to_string());
                }
            }
        }
    }

    best_match
}

/// Extract the underlying `.exe` or `.ico` target path from a Windows `.lnk` binary (`[MS-SHLLINK]`)
/// so icon extraction pulls the clean application icon directly from the PE binary without the
/// Windows Shell `.lnk` shortcut overlay arrow.
fn resolve_lnk_icon_target(lnk_path: &Path) -> Option<String> {
    let bytes = std::fs::read(lnk_path).ok()?;
    if bytes.len() < 76 {
        return None;
    }

    // 1. Scan ASCII/ANSI segments for drive-letter paths (`C:\...\.exe` or `.ico`)
    let mut best_match: Option<String> = None;
    for i in 0..bytes.len().saturating_sub(6) {
        let b0 = bytes[i];
        if b0.is_ascii_alphabetic() && bytes[i + 1] == b':' && bytes[i + 2] == b'\\' {
            let mut end = i;
            while end < bytes.len()
                && bytes[end] >= 0x20
                && bytes[end] < 0x7F
                && !matches!(bytes[end], b'"' | b'<' | b'>' | b'|')
            {
                end += 1;
            }
            if let Ok(s) = std::str::from_utf8(&bytes[i..end]) {
                let s_trim = s.trim();
                let lower = s_trim.to_ascii_lowercase();
                if (lower.ends_with(".exe") || lower.ends_with(".ico"))
                    && !lower.contains("cmd.exe")
                    && Path::new(s_trim).exists()
                {
                    best_match = Some(s_trim.to_string());
                }
            }
        }
    }
    if best_match.is_some() {
        return best_match;
    }

    // 2. Scan UTF-16LE segments for drive-letter paths (`C:\...\.exe` or `.ico`)
    for offset in [0usize, 1usize] {
        let u16s: Vec<u16> = bytes[offset..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        for i in 0..u16s.len().saturating_sub(6) {
            let c0 = u16s[i];
            if ((c0 >= b'A' as u16 && c0 <= b'Z' as u16)
                || (c0 >= b'a' as u16 && c0 <= b'z' as u16))
                && u16s[i + 1] == b':' as u16
                && u16s[i + 2] == b'\\' as u16
            {
                let mut end = i;
                while end < u16s.len()
                    && u16s[end] >= 0x20
                    && u16s[end] < 0xFFFE
                    && !matches!(u16s[end] as u8, b'"' | b'<' | b'>' | b'|')
                {
                    end += 1;
                }
                let s = String::from_utf16_lossy(&u16s[i..end]);
                let s_trim = s.trim();
                let lower = s_trim.to_ascii_lowercase();
                if (lower.ends_with(".exe") || lower.ends_with(".ico"))
                    && !lower.contains("cmd.exe")
                    && Path::new(s_trim).exists()
                {
                    best_match = Some(s_trim.to_string());
                }
            }
        }
    }

    best_match
}

fn fuzzy_token_score(token: &str, target: &str) -> Option<i32> {
    if token.is_empty() {
        return Some(0);
    }
    if target == token {
        return Some(2200);
    }
    if target.starts_with(token) {
        return Some(1650 - (target.len() as i32));
    }

    // Acronym match bonus (e.g. "wt" -> "windows terminal", "tm" -> "task manager")
    let initials: String = target
        .split(|c: char| !c.is_alphanumeric())
        .filter_map(|w| w.chars().next())
        .collect();
    if initials.len() >= 2 && initials.starts_with(token) {
        return Some(1580 - (target.len() as i32));
    }

    if let Some(pos) = target.find(token) {
        let word_boundary_bonus = if pos == 0
            || target
                .as_bytes()
                .get(pos.wrapping_sub(1))
                .map(|&b| !b.is_ascii_alphanumeric())
                .unwrap_or(false)
        {
            280
        } else {
            0
        };
        return Some(1050 + word_boundary_bonus - (pos as i32 * 6) - (target.len() as i32));
    }

    // Subsequence character match with word-boundary & consecutive bonuses
    let t_bytes = target.as_bytes();
    let mut score = 400;
    let mut t_idx = 0usize;
    let mut prev_match = None;

    for qc in token.bytes() {
        let mut found = false;
        while t_idx < t_bytes.len() {
            let tc = t_bytes[t_idx];
            if tc == qc {
                let is_boundary = t_idx == 0 || !t_bytes[t_idx - 1].is_ascii_alphanumeric();
                if is_boundary {
                    score += 85;
                }
                if let Some(prev) = prev_match {
                    if t_idx == prev + 1 {
                        score += 45;
                    } else {
                        score -= ((t_idx as i32) - (prev as i32)).min(25);
                    }
                }
                prev_match = Some(t_idx);
                t_idx += 1;
                found = true;
                break;
            }
            t_idx += 1;
        }
        if !found {
            return None;
        }
    }
    Some(score)
}

fn fuzzy_score(query: &str, target: &str, is_window: bool) -> Option<i32> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Some(if is_window { 500 } else { 100 });
    }
    let mut total = if is_window { 140 } else { 0 };
    for token in trimmed.split_whitespace() {
        total += fuzzy_token_score(token, target)?;
    }
    Some(total)
}

fn url_encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else if b == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn recompute_filtered_results(st: &mut LauncherState) {
    let q_raw = st.model.query.clone();
    let trimmed_q = q_raw.trim().to_string();
    let q = trimmed_q.to_ascii_lowercase();
    let mut scored: Vec<(i32, LauncherCandidate)> = Vec::new();
    ui::trigger_list_cascade(&mut st.model);

    // 0a. Clipboard History Mode (`:` prefix or `SUPER + SHIFT + V` / `hyprlauncher --clipboard`)
    if let Some(clip_filter) = trimmed_q.strip_prefix(':') {
        let cf = clip_filter.trim().to_ascii_lowercase();
        let entries = CLIPBOARD_HISTORY
            .get()
            .and_then(|m| m.lock().ok().map(|g| g.clone()))
            .unwrap_or_default();

        for (idx, item) in entries.iter().enumerate() {
            let lower_item = item.to_ascii_lowercase();
            let score_opt = if cf.is_empty() {
                Some(5000 - (idx as i32 * 10))
            } else {
                fuzzy_score(&cf, &lower_item, true).map(|s| s + 2000 - (idx as i32 * 5))
            };
            if let Some(sc) = score_opt {
                let preview: String = item.chars().take(68).collect();
                scored.push((
                    sc,
                    LauncherCandidate {
                        title: preview,
                        subtitle: format!(
                            "Clipboard History #{} ({} chars) · Press Enter to copy",
                            idx + 1,
                            item.chars().count()
                        ),
                        badge: "CLIP".to_string(),
                        search_key: lower_item,
                        icon_hwnd: 0,
                        icon_path: "notepad.exe".to_string(),
                        action: CandidateAction::CopyClipboardEntry { text: item.clone() },
                    },
                ));
            }
        }
        if scored.is_empty() {
            scored.push((
                100,
                LauncherCandidate {
                    title: "Clipboard history is empty".to_string(),
                    subtitle: "Copy any text (Ctrl+C) and press SUPER+SHIFT+V or type ':'".to_string(),
                    badge: "CLIP".to_string(),
                    search_key: String::new(),
                    icon_hwnd: 0,
                    icon_path: "notepad.exe".to_string(),
                    action: CandidateAction::CopyClipboardEntry {
                        text: String::new(),
                    },
                },
            ));
        }

        st.model.total_matches = scored.len();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.truncate(MAX_VISIBLE_ITEMS);
        st.filtered = scored.iter().map(|(_, c)| c.clone()).collect();
        st.model.rows = st
            .filtered
            .iter()
            .map(|c| LauncherDisplayRow {
                title: c.title.clone(),
                subtitle: c.subtitle.clone(),
                badge: c.badge.clone(),
                is_window: false,
                icon_hwnd: c.icon_hwnd,
                icon_path: c.icon_path.clone(),
            })
            .collect();
        if st.model.selected_index >= st.model.rows.len() {
            st.model.selected_index = 0;
        }
        return;
    }

    // 0b. Shell Command Mode (`>` or `$` prefix)
    if let Some(cmd_raw) = q_raw.strip_prefix('>').or_else(|| q_raw.strip_prefix('$')) {
        let cmd = cmd_raw.trim();
        let display_cmd = if cmd.is_empty() { "powershell" } else { cmd };
        scored.push((
            12_000,
            LauncherCandidate {
                title: format!("Run: {display_cmd}"),
                subtitle: "Execute command in Windows Terminal (PowerShell)".to_string(),
                badge: "CMD".to_string(),
                search_key: q.clone(),
                icon_hwnd: 0,
                icon_path: "wt.exe".to_string(),
                action: CandidateAction::RunShellCommand {
                    cmd: display_cmd.to_string(),
                },
            },
        ));
    }

    // 0c. Web Search / Direct URL Mode (`?` prefix or URL pattern)
    if let Some(web_q) = q_raw.strip_prefix('?') {
        let trimmed_w = web_q.trim();
        if !trimmed_w.is_empty() {
            let url = format!("https://www.google.com/search?q={}", url_encode_query(trimmed_w));
            scored.push((
                11_500,
                LauncherCandidate {
                    title: format!("Search Web: \"{trimmed_w}\""),
                    subtitle: url.clone(),
                    badge: "WEB".to_string(),
                    search_key: q.clone(),
                    icon_hwnd: 0,
                    icon_path: "explorer.exe".to_string(),
                    action: CandidateAction::OpenWebUrl { url },
                },
            ));
        }
    } else if q_raw.starts_with("https://") || q_raw.starts_with("http://") {
        scored.push((
            11_500,
            LauncherCandidate {
                title: format!("Open URL: {q_raw}"),
                subtitle: "Launch directly in default web browser".to_string(),
                badge: "URL".to_string(),
                search_key: q.clone(),
                icon_hwnd: 0,
                icon_path: "explorer.exe".to_string(),
                action: CandidateAction::OpenWebUrl {
                    url: q_raw.to_string(),
                },
            },
        ));
    }

    // 0d. Hex / Binary / Decimal Programmer Base Converter (`0x...` or `0b...`)
    if let Some(hex_str) = q.strip_prefix("0x") {
        if let Ok(val) = u64::from_str_radix(hex_str.trim(), 16) {
            let formatted = format!("{val}  (0x{val:X} · 0b{val:b})");
            scored.push((
                10_500,
                LauncherCandidate {
                    title: format!("= {formatted}"),
                    subtitle: "Hexadecimal conversion · Press Enter to copy decimal value".to_string(),
                    badge: "HEX".to_string(),
                    search_key: q.clone(),
                    icon_hwnd: 0,
                    icon_path: "calc.exe".to_string(),
                    action: CandidateAction::CopyMathResult {
                        result: val.to_string(),
                    },
                },
            ));
        }
    } else if let Some(bin_str) = q.strip_prefix("0b") {
        if let Ok(val) = u64::from_str_radix(bin_str.trim(), 2) {
            let formatted = format!("{val}  (0x{val:X} · 0b{val:b})");
            scored.push((
                10_500,
                LauncherCandidate {
                    title: format!("= {formatted}"),
                    subtitle: "Binary conversion · Press Enter to copy decimal value".to_string(),
                    badge: "BIN".to_string(),
                    search_key: q.clone(),
                    icon_hwnd: 0,
                    icon_path: "calc.exe".to_string(),
                    action: CandidateAction::CopyMathResult {
                        result: val.to_string(),
                    },
                },
            ));
        }
    }

    // 0e. Instant Math / Calculator Expression Evaluator (`3+4`, `2^10`, `sqrt(144)`, `(15*4)/3`, etc.)
    if let Some(val) = evaluate_math_expression(&q_raw) {
        let formatted = if (val - val.round()).abs() < 1e-9 && val.abs() < 1e15 {
            format!("{}", val.round() as i64)
        } else {
            let s = format!("{:.8}", val);
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        };
        scored.push((
            10_000,
            LauncherCandidate {
                title: format!("= {formatted}"),
                subtitle: format!("{q_raw}  ·  Press Enter to copy result to clipboard"),
                badge: "CALC".to_string(),
                search_key: q.clone(),
                icon_hwnd: 0,
                icon_path: "calc.exe".to_string(),
                action: CandidateAction::CopyMathResult { result: formatted },
            },
        ));
    }

    // 1. Cached open windows across all 10 workspaces (zero IPC on keystrokes!)
    for w in &st.open_windows {
        if let Some(s) = fuzzy_score(&q, &w.search_key, true) {
            let freq_boost = st
                .frecency
                .get(&w.title.to_ascii_lowercase())
                .copied()
                .unwrap_or(0)
                .min(25) as i32
                * 35;
            scored.push((s + freq_boost, w.clone()));
        }
    }

    // 2. Installed Start Menu & system applications + frecency boost
    for app in &st.app_catalog {
        if let Some(s) = fuzzy_score(&q, &app.search_key, false) {
            let freq_boost = st
                .frecency
                .get(&app.title.to_ascii_lowercase())
                .copied()
                .unwrap_or(0)
                .min(30) as i32
                * 45;
            scored.push((s + freq_boost, app.clone()));
        }
    }

    st.model.total_matches = scored.len();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
    scored.truncate(MAX_VISIBLE_ITEMS);

    st.filtered = scored.iter().map(|(_, c)| c.clone()).collect();
    st.model.rows = st
        .filtered
        .iter()
        .map(|c| LauncherDisplayRow {
            title: c.title.clone(),
            subtitle: c.subtitle.clone(),
            badge: c.badge.clone(),
            is_window: matches!(c.action, CandidateAction::SwitchToWindow { .. }),
            icon_hwnd: c.icon_hwnd,
            icon_path: c.icon_path.clone(),
        })
        .collect();

    if st.model.selected_index >= st.model.rows.len() {
        st.model.selected_index = 0;
    }
}

/// Recursive-descent math expression evaluator supporting `+`, `-`, `*`, `/`, `%`, `^`, `()`,
/// and functions `sqrt`, `sin`, `cos`, `tan`, `abs`, `ln`, `log` + constants `pi`, `e`.
fn evaluate_math_expression(input: &str) -> Option<f64> {
    let s = input.trim().trim_start_matches('=').trim();
    if s.is_empty() {
        return None;
    }
    let has_digit = s.chars().any(|c| c.is_ascii_digit());
    let has_op_or_fn = s.chars().any(|c| matches!(c, '+' | '-' | '*' | '/' | '%' | '^' | '(' | ')'))
        || s.contains("sqrt")
        || s.contains("sin")
        || s.contains("cos")
        || s.contains("tan")
        || s.contains("log")
        || s.contains("ln");
    if !has_digit || !has_op_or_fn {
        return None;
    }

    struct Parser<'a> {
        bytes: &'a [u8],
        pos: usize,
    }

    impl<'a> Parser<'a> {
        fn skip_ws(&mut self) {
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
        }

        fn parse_expr(&mut self) -> Option<f64> {
            let mut left = self.parse_term()?;
            loop {
                self.skip_ws();
                if self.pos >= self.bytes.len() {
                    break;
                }
                match self.bytes[self.pos] {
                    b'+' => {
                        self.pos += 1;
                        left += self.parse_term()?;
                    }
                    b'-' => {
                        self.pos += 1;
                        left -= self.parse_term()?;
                    }
                    _ => break,
                }
            }
            Some(left)
        }

        fn parse_term(&mut self) -> Option<f64> {
            let mut left = self.parse_power()?;
            loop {
                self.skip_ws();
                if self.pos >= self.bytes.len() {
                    break;
                }
                match self.bytes[self.pos] {
                    b'*' => {
                        self.pos += 1;
                        left *= self.parse_power()?;
                    }
                    b'/' => {
                        self.pos += 1;
                        let r = self.parse_power()?;
                        if r.abs() < 1e-15 {
                            return None;
                        }
                        left /= r;
                    }
                    b'%' => {
                        self.pos += 1;
                        let r = self.parse_power()?;
                        if r.abs() < 1e-15 {
                            return None;
                        }
                        left %= r;
                    }
                    _ => break,
                }
            }
            Some(left)
        }

        fn parse_power(&mut self) -> Option<f64> {
            let base = self.parse_unary()?;
            self.skip_ws();
            if self.pos < self.bytes.len() && self.bytes[self.pos] == b'^' {
                self.pos += 1;
                let exp = self.parse_power()?;
                Some(base.powf(exp))
            } else {
                Some(base)
            }
        }

        fn parse_unary(&mut self) -> Option<f64> {
            self.skip_ws();
            if self.pos < self.bytes.len() {
                if self.bytes[self.pos] == b'+' {
                    self.pos += 1;
                    return self.parse_unary();
                }
                if self.bytes[self.pos] == b'-' {
                    self.pos += 1;
                    return Some(-self.parse_unary()?);
                }
            }
            self.parse_primary()
        }

        fn parse_primary(&mut self) -> Option<f64> {
            self.skip_ws();
            if self.pos >= self.bytes.len() {
                return None;
            }
            if self.bytes[self.pos] == b'(' {
                self.pos += 1;
                let v = self.parse_expr()?;
                self.skip_ws();
                if self.pos < self.bytes.len() && self.bytes[self.pos] == b')' {
                    self.pos += 1;
                }
                return Some(v);
            }

            if self.bytes[self.pos].is_ascii_alphabetic() {
                let start = self.pos;
                while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_alphabetic() {
                    self.pos += 1;
                }
                let ident = std::str::from_utf8(&self.bytes[start..self.pos])
                    .ok()?
                    .to_ascii_lowercase();
                if ident == "pi" {
                    return Some(std::f64::consts::PI);
                }
                if ident == "e" {
                    return Some(std::f64::consts::E);
                }
                let arg = self.parse_primary()?;
                return match ident.as_str() {
                    "sqrt" => Some(arg.sqrt()),
                    "sin" => Some(arg.sin()),
                    "cos" => Some(arg.cos()),
                    "tan" => Some(arg.tan()),
                    "abs" => Some(arg.abs()),
                    "ln" => Some(arg.ln()),
                    "log" => Some(arg.log10()),
                    _ => None,
                };
            }

            let start = self.pos;
            let mut seen_dot = false;
            while self.pos < self.bytes.len() {
                let b = self.bytes[self.pos];
                if b.is_ascii_digit() {
                    self.pos += 1;
                } else if b == b'.' && !seen_dot {
                    seen_dot = true;
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if self.pos == start {
                return None;
            }
            let num_str = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
            num_str.parse::<f64>().ok()
        }
    }

    let mut p = Parser {
        bytes: s.as_bytes(),
        pos: 0,
    };
    let val = p.parse_expr()?;
    p.skip_ws();
    if p.pos == p.bytes.len() && val.is_finite() {
        Some(val)
    } else {
        None
    }
}
