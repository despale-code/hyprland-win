#![windows_subsystem = "windows"]
//! `hyprbar` — Native Top Status Bar (`zwlr_layer_shell_v1` equivalent) for `Hyprland-Win`
//! featuring the `atif-1402/minimal-waybar-themes` V2 layout, `UpdateLayeredWindow` (`ULW_ALPHA`)
//! transparency, `JetBrainsMono NF` Nerd Font icons, real-time `C:\Users\mbura\.config` hot-reloading,
//! and `SUPER + SHIFT + S` region screenshot capture.

mod theme;
mod ui;
mod widgets;

use anyhow::Result;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{FILETIME, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmFlush, DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DONOTROUND,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, EnumDisplaySettingsW, ScreenToClient, DEVMODEW, ENUM_CURRENT_SETTINGS,
    PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::{
    GetLocalTime, GlobalMemoryStatusEx, MEMORYSTATUSEX,
};
use windows::Win32::System::Threading::{GetCurrentProcessId, GetSystemTimes};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetSystemMetrics, KillTimer, LoadCursorW,
    PeekMessageW, PostMessageW, PostQuitMessage, RegisterClassW, SetWindowPos, ShowWindow,
    TranslateMessage, CS_HREDRAW, CS_VREDRAW, HWND_BOTTOM, HWND_TOPMOST, IDC_ARROW, MA_NOACTIVATE,
    MSG, PM_REMOVE, SM_CXSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_SHOWNOACTIVATE, WM_APP,
    WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEACTIVATE, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_PAINT, WM_QUIT, WM_RBUTTONDOWN, WM_TIMER, WM_WINDOWPOSCHANGING, WINDOWPOS,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    WS_VISIBLE,
};

use hyprland_win_core::{
    HyprBarStateSnapshot, HyprConfig, HyprIpcRequest, HyprIpcResponse, HYPRLAND_WIN_IPC_PIPE,
};
pub use ui::{ActivePopover, BarClickTarget, BarRenderModel};

pub const BAR_HEIGHT: i32 = 40;
const TIMER_ID_POLL: usize = 1;
const WM_BAR_VBLANK: u32 = WM_APP + 76;
const WM_BAR_FAST_WS: u32 = WM_APP + 77;
const WM_BAR_TELEMETRY_READY: u32 = WM_APP + 78;

struct BarRuntimeState {
    model: BarRenderModel,
    prev_idle: u64,
    prev_kernel: u64,
    prev_user: u64,
    last_cpu_poll: Option<Instant>,
    disconnected_ticks: u32,
    last_anim_tick: Instant,
    monitor_hz: u32,
    last_z_fullscreen: Option<bool>,
}

static BAR_STATE: OnceLock<Mutex<BarRuntimeState>> = OnceLock::new();
static LAST_HYPRCONF_MTIME: OnceLock<Mutex<Option<SystemTime>>> = OnceLock::new();
static VBLANK_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static TELEMETRY_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn query_system_refresh_rate_hz() -> u32 {
    unsafe {
        let mut dm = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        if EnumDisplaySettingsW(PCWSTR::null(), ENUM_CURRENT_SETTINGS, &mut dm).as_bool() {
            if dm.dmDisplayFrequency >= 30 && dm.dmDisplayFrequency <= 540 {
                return dm.dmDisplayFrequency;
            }
        }
    }
    144
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
        ui::ensure_private_fonts_loaded();
    }

    if is_hyprbar_already_running() {
        return Ok(());
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let restart_marker = dir.join(".restart_compositor");
            if restart_marker.exists() {
                let _ = std::fs::remove_file(&restart_marker);
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("taskkill.exe")
                    .args(["/F", "/IM", "hyprland-win.exe", "/IM", "hyprlauncher.exe"])
                    .creation_flags(0x0800_0000)
                    .status();
                std::thread::sleep(std::time::Duration::from_millis(250));
                let hyprland_win_exe = dir.join("hyprland-win.exe");
                if hyprland_win_exe.exists() {
                    let _ = std::process::Command::new(hyprland_win_exe)
                        .creation_flags(0x0800_0000)
                        .spawn();
                }
                return Ok(());
            }

            let launcher_exe = dir.join("hyprlauncher.exe");
            if launcher_exe.exists() {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("taskkill.exe")
                    .args(["/F", "/IM", "hyprlauncher.exe"])
                    .creation_flags(0x0800_0000)
                    .status();
                let _ = std::process::Command::new(launcher_exe)
                    .creation_flags(0x0800_0000)
                    .spawn();
            }
        }
    }

    // Start background system & media telemetry worker
    widgets::ensure_widgets_worker_started();

    let mon_w = unsafe { GetSystemMetrics(SM_CXSCREEN).max(1280) };
    let monitor_hz = query_system_refresh_rate_hz();

    let initial_state = BarRuntimeState {
        model: BarRenderModel::default_with_width(mon_w),
        prev_idle: 0,
        prev_kernel: 0,
        prev_user: 0,
        last_cpu_poll: None,
        disconnected_ticks: 0,
        last_anim_tick: Instant::now(),
        monitor_hz,
        last_z_fullscreen: None,
    };
    let _ = BAR_STATE.set(Mutex::new(initial_state));

    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class_name: Vec<u16> = "HyprBarTopOverlay\0".encode_utf16().collect();
        let title_name: Vec<u16> = "HyprBar\0".encode_utf16().collect();

        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(bar_wnd_proc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&wc);

        // `WS_EX_LAYERED` enables per-pixel `ULW_ALPHA` transparency with zero outer window border!
        let ex_style = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let hwnd = CreateWindowExW(
            ex_style,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title_name.as_ptr()),
            WS_POPUP | WS_VISIBLE,
            0,
            0,
            mon_w,
            BAR_HEIGHT,
            HWND::default(),
            None,
            instance,
            None,
        )?;

        let border_none: u32 = 0xFFFF_FFFE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border_none as *const u32 as *const _,
            std::mem::size_of::<u32>() as u32,
        );
        let corner_square = DWMWCP_DONOTROUND.0 as u32;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner_square as *const u32 as *const _,
            std::mem::size_of::<u32>() as u32,
        );

        ui::disable_window_acrylic(hwnd);

        #[link(name = "wtsapi32")]
        extern "system" {
            fn WTSRegisterSessionNotification(hwnd: HWND, dw_flags: u32) -> i32;
        }
        let _ = WTSRegisterSessionNotification(hwnd, 0);

        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        repaint_layered_bar(hwnd);

        // Background IPC & telemetry poller thread (keeps all Named Pipe I/O 100% off the Win32 UI thread!)
        let hwnd_raw = hwnd.0 as usize;
        spawn_background_telemetry_poller(hwnd_raw);

        // Hardware DwmFlush VSync pacemaker thread matching native display refresh rate
        std::thread::spawn(move || {
            attach_current_thread_to_default_desktop();
            let target_frame_ns = (1_000_000_000u64 / (monitor_hz.max(60) as u64)).max(1_800_000);
            loop {
                let frame_start = Instant::now();
                let dwm_ok = DwmFlush().is_ok();
                if !dwm_ok {
                    std::thread::sleep(std::time::Duration::from_nanos(target_frame_ns));
                } else {
                    let elapsed_ns = frame_start.elapsed().as_nanos() as u64;
                    if elapsed_ns < target_frame_ns / 2 {
                        std::thread::sleep(std::time::Duration::from_nanos(
                            target_frame_ns.saturating_sub(elapsed_ns),
                        ));
                    }
                }
                if !VBLANK_PENDING.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    let h = HWND(hwnd_raw as *mut core::ffi::c_void);
                    if PostMessageW(h, WM_BAR_VBLANK, WPARAM(0), LPARAM(0)).is_err() {
                        break;
                    }
                }
            }
        });

        let mut msg = MSG::default();
        'msg_loop: loop {
            while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    break 'msg_loop;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = windows::Win32::UI::WindowsAndMessaging::WaitMessage();
        }
    }

    Ok(())
}

unsafe fn repaint_layered_bar(hwnd: HWND) {
    if let Some(st_mutex) = BAR_STATE.get() {
        if let Ok(mut st) = st_mutex.try_lock() {
            ui::paint_bar_layered(hwnd, &mut st.model);
        }
    }
}

unsafe extern "system" fn bar_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_WINDOWPOSCHANGING => {
            // Lock `hyprbar` to (0, 0) at the top of the screen so it can never be dragged or displaced
            let wp = &mut *(lparam.0 as *mut WINDOWPOS);
            wp.x = 0;
            wp.y = 0;
            LRESULT(0)
        }
        WM_TIMER | WM_BAR_TELEMETRY_READY => {
            TELEMETRY_PENDING.store(false, std::sync::atomic::Ordering::SeqCst);
            if msg == WM_BAR_TELEMETRY_READY || wparam.0 == TIMER_ID_POLL {
                let mut should_exit = false;
                let mut z_change: Option<bool> = None;
                if let Some(st_mutex) = BAR_STATE.get() {
                    if let Ok(mut st) = st_mutex.try_lock() {
                        let is_fullscreen = st.model.active_fullscreen;
                        if st.last_z_fullscreen != Some(is_fullscreen) {
                            st.last_z_fullscreen = Some(is_fullscreen);
                            z_change = Some(is_fullscreen);
                        }
                        if st.disconnected_ticks > 40 {
                            should_exit = true;
                        }
                    }
                }
                if should_exit {
                    let _ = KillTimer(hwnd, TIMER_ID_POLL);
                    PostQuitMessage(0);
                    return LRESULT(0);
                }
                if let Some(is_fullscreen) = z_change {
                    let z_order = if is_fullscreen {
                        HWND_BOTTOM
                    } else {
                        HWND_TOPMOST
                    };
                    let _ = SetWindowPos(
                        hwnd,
                        z_order,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }
                repaint_layered_bar(hwnd);
            }
            LRESULT(0)
        }
        WM_BAR_VBLANK => {
            VBLANK_PENDING.store(false, std::sync::atomic::Ordering::SeqCst);
            let mut needs_repaint = false;
            let mut model_snap = None;
            let mut over_clock = false;
            if let Some(st_mutex) = BAR_STATE.get() {
                if let Ok(mut st) = st_mutex.try_lock() {
                    let now = Instant::now();
                    let raw_dt = now.duration_since(st.last_anim_tick).as_secs_f32();
                    st.last_anim_tick = now;
                    let nominal_dt = 1.0 / (st.monitor_hz.max(60) as f32);
                    let dt = raw_dt.clamp(0.0005, 0.050);
                    let effective_dt = if dt > 0.045 { nominal_dt } else { dt };

                    if st.model.hover_pos.is_some()
                        || st.model.active_popover != ActivePopover::None
                    {
                        ui::check_cursor_hover_leave(hwnd, &mut st.model);
                    }
                    needs_repaint = ui::step_bar_animations(&mut st.model, effective_dt);
                    if needs_repaint {
                        model_snap = Some(st.model.clone());
                    }
                }
            }
            if needs_repaint {
                repaint_layered_bar(hwnd);
                if let Some(ref m) = model_snap {
                    ui::update_calendar_popup(hwnd, m, false);
                }
            }
            LRESULT(0)
        }
        WM_BAR_FAST_WS => {
            let vk = wparam.0 as u32;
            let target_ws = match vk {
                0x31..=0x39 => Some(vk - 0x30),
                0x30 => Some(10),
                _ => None,
            };
            if let Some(ws) = target_ws {
                if let Some(st_mutex) = BAR_STATE.get() {
                    if let Ok(mut st) = st_mutex.lock() {
                        st.model.active_workspace = ws;
                    }
                }
                repaint_layered_bar(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut changed = false;
            let mut model_snap = None;
            if let Some(st_mutex) = BAR_STATE.get() {
                if let Ok(mut st) = st_mutex.try_lock() {
                    let prev_hover = st.model.hover_pos;
                    st.model.hover_pos = Some((x, y));
                    changed = prev_hover != st.model.hover_pos;
                    if ui::is_point_in_window_pill(&st.model, x, y)
                        && !matches!(st.model.active_popover, ActivePopover::Calendar { .. })
                    {
                        st.model.active_popover = ActivePopover::Calendar {
                            year: st.model.cur_year.max(2024),
                            month: st.model.cur_month.clamp(1, 12),
                        };
                        changed = true;
                    }
                    model_snap = Some(st.model.clone());
                }
            }
            if let Some(ref m) = model_snap {
                ui::update_calendar_popup(hwnd, m, false);
                ui::update_tooltip_popup(hwnd, m, true);
            }
            if changed {
                repaint_layered_bar(hwnd);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut hit_target = None;
            let mut mpris_stopped_or_paused = false;
            let mut tray_app_to_activate = None;
            let mut model_snap = None;

            if let Some(st_mutex) = BAR_STATE.get() {
                if let Ok(mut st) = st_mutex.try_lock() {
                    hit_target = ui::hit_test_bar(&st.model, x, y);
                    mpris_stopped_or_paused = st.model.widgets.media.status
                        != widgets::MediaPlaybackStatus::Playing;
                    match hit_target {
                        Some(BarClickTarget::TrayExpanderToggle) => {
                            st.model.tray_expanded = !st.model.tray_expanded;
                        }
                        Some(BarClickTarget::TrayApp(idx)) => {
                            tray_app_to_activate = st.model.widgets.tray_items.get(idx).cloned();
                        }
                        Some(BarClickTarget::Clock) => {
                            st.model.clock_alt_mode = !st.model.clock_alt_mode;
                        }
                        Some(BarClickTarget::FocusedWindow) => {
                            st.model.active_popover = match st.model.active_popover {
                                ActivePopover::Calendar { .. } => ActivePopover::None,
                                _ => ActivePopover::Calendar {
                                    year: st.model.cur_year.max(2024),
                                    month: st.model.cur_month.clamp(1, 12),
                                },
                            };
                        }
                        Some(BarClickTarget::CtlPulseAudio) => {
                            st.model.active_popover = if st.model.active_popover == ActivePopover::Volume {
                                ActivePopover::None
                            } else {
                                ActivePopover::Volume
                            };
                        }
                        Some(BarClickTarget::CtlBluetooth) => {
                            st.model.active_popover = if st.model.active_popover == ActivePopover::Bluetooth {
                                ActivePopover::None
                            } else {
                                widgets::refresh_bluetooth_devices();
                                ActivePopover::Bluetooth
                            };
                        }
                        Some(BarClickTarget::CtlNetwork) => {
                            st.model.active_popover = if st.model.active_popover == ActivePopover::Network {
                                ActivePopover::None
                            } else {
                                ActivePopover::Network
                            };
                        }
                        Some(BarClickTarget::OmarchyLauncher | BarClickTarget::SystemMenu) => {
                            st.model.active_popover = if matches!(
                                st.model.active_popover,
                                ActivePopover::SystemMenu | ActivePopover::AboutHyprlandWin
                            ) {
                                ActivePopover::None
                            } else {
                                ActivePopover::SystemMenu
                            };
                        }
                        _ => {
                            st.model.active_popover = ActivePopover::None;
                        }
                    }
                    model_snap = Some(st.model.clone());
                }
            }
            if let Some(ref m) = model_snap {
                ui::update_calendar_popup(hwnd, m, false);
                ui::update_tooltip_popup(hwnd, m, false);
            }

            if let Some(ref item) = tray_app_to_activate {
                widgets::activate_tray_item(item);
            }

            if let Some(target) = hit_target {
                match target {
                    BarClickTarget::OmarchyLauncher => {}
                    BarClickTarget::Workspace(ws) => {
                        if let Some(st_mutex) = BAR_STATE.get() {
                            if let Ok(mut st) = st_mutex.lock() {
                                st.model.active_workspace = ws;
                            }
                        }
                        dispatch_ipc_async(
                            hwnd,
                            HyprIpcRequest::Dispatch {
                                dispatcher: "workspace".to_string(),
                                arg: ws.to_string(),
                            },
                        );
                    }
                    BarClickTarget::MprisPlayPause => {
                        widgets::media_play_pause();
                    }
                    BarClickTarget::MprisBody => {
                        if mpris_stopped_or_paused {
                            widgets::media_play_pause();
                        } else {
                            widgets::media_prev_track();
                        }
                    }
                    BarClickTarget::TrayShot
                    | BarClickTarget::TrayTiling
                    | BarClickTarget::ToggleLayout
                    | BarClickTarget::ToggleTiling => {}
                    BarClickTarget::TrayLock => {
                        widgets::launch_hyprlock();
                    }
                    BarClickTarget::CtlIdleInhibit => {
                        widgets::toggle_idle_inhibit();
                    }
                    BarClickTarget::CtlDndSilence => {
                        widgets::toggle_dnd_silenced();
                    }
                    BarClickTarget::CtlCpu => {
                        widgets::spawn_detached_command("taskmgr.exe", &[]);
                    }
                    BarClickTarget::CtlBattery => {
                        widgets::spawn_detached_command(
                            "explorer.exe",
                            &["ms-settings:powersleep"],
                        );
                    }
                    BarClickTarget::WorkspacesContainer
                    | BarClickTarget::FocusedWindow
                    | BarClickTarget::TrayExpanderToggle
                    | BarClickTarget::TrayApp(_)
                    | BarClickTarget::CtlBluetooth
                    | BarClickTarget::CtlNetwork
                    | BarClickTarget::CtlPulseAudio
                    | BarClickTarget::Clock
                    | BarClickTarget::SystemMenu => {}
                }

                repaint_layered_bar(hwnd);
            }
            LRESULT(0)
        }
        WM_MBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let hit = BAR_STATE
                .get()
                .and_then(|m| m.lock().ok())
                .and_then(|st| ui::hit_test_bar(&st.model, x, y));

            if matches!(
                hit,
                Some(BarClickTarget::MprisPlayPause | BarClickTarget::MprisBody)
            ) {
                widgets::media_play_pause();
                repaint_layered_bar(hwnd);
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut hit = None;
            let mut tray_app_for_menu = None;

            if let Some(st_mutex) = BAR_STATE.get() {
                if let Ok(mut st) = st_mutex.try_lock() {
                    hit = ui::hit_test_bar(&st.model, x, y);
                    match hit {
                        Some(BarClickTarget::Clock) => {
                            st.model.clock_alt_mode = !st.model.clock_alt_mode;
                        }
                        Some(BarClickTarget::TrayApp(idx)) => {
                            tray_app_for_menu = st.model.widgets.tray_items.get(idx).cloned();
                        }
                        _ => {}
                    }
                }
            }

            if let Some(ref item) = tray_app_for_menu {
                widgets::context_menu_tray_item(item);
            }

            match hit {
                Some(BarClickTarget::OmarchyLauncher) => {
                    widgets::spawn_detached_command("wt.exe", &[]);
                }
                Some(BarClickTarget::MprisPlayPause | BarClickTarget::MprisBody) => {
                    widgets::media_next_track();
                }
                Some(BarClickTarget::CtlPulseAudio) => {
                    widgets::toggle_master_mute();
                }
                Some(BarClickTarget::CtlNetwork) => {
                    widgets::spawn_detached_command("explorer.exe", &["ms-settings:network"]);
                }
                Some(BarClickTarget::CtlBluetooth) => {
                    widgets::spawn_detached_command("explorer.exe", &["ms-settings:bluetooth"]);
                }
                Some(
                    BarClickTarget::Clock
                    | BarClickTarget::TrayApp(_)
                    | BarClickTarget::TrayShot
                    | BarClickTarget::TrayTiling,
                ) => {}
                _ => {
                    dispatch_ipc_async(
                        hwnd,
                        HyprIpcRequest::Dispatch {
                            dispatcher: "togglefloating".to_string(),
                            arg: String::new(),
                        },
                    );
                }
            }
            repaint_layered_bar(hwnd);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) as i16) as i32;
            let mut pt = POINT {
                x: (lparam.0 & 0xFFFF) as i16 as i32,
                y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
            };
            let _ = ScreenToClient(hwnd, &mut pt);

            let hit = BAR_STATE
                .get()
                .and_then(|m| m.lock().ok())
                .and_then(|st| ui::hit_test_bar(&st.model, pt.x, pt.y));

            if matches!(
                hit,
                Some(
                    BarClickTarget::MprisPlayPause
                        | BarClickTarget::MprisBody
                        | BarClickTarget::CtlPulseAudio
                )
            ) {
                let step = if delta > 0 { 5 } else { -5 };
                widgets::adjust_master_volume(step);
                repaint_layered_bar(hwnd);
            } else if let Some(st_mutex) = BAR_STATE.get() {
                let cur_ws = st_mutex
                    .lock()
                    .map(|st| st.model.active_workspace as i32)
                    .unwrap_or(1);
                let next_ws = if delta > 0 {
                    (cur_ws - 1).clamp(1, 10)
                } else {
                    (cur_ws + 1).clamp(1, 10)
                };
                if let Ok(mut st) = st_mutex.lock() {
                    st.model.active_workspace = next_ws as u32;
                }
                dispatch_ipc_async(
                    hwnd,
                    HyprIpcRequest::Dispatch {
                        dispatcher: "workspace".to_string(),
                        arg: next_ws.to_string(),
                    },
                );
                repaint_layered_bar(hwnd);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            repaint_layered_bar(hwnd);
            LRESULT(0)
        }
        0x02B1 /* WM_WTSSESSION_CHANGE */ => {
            if wparam.0 == 7 /* WTS_SESSION_LOCK */ {
                let _ = ShowWindow(hwnd, windows::Win32::UI::WindowsAndMessaging::SW_HIDE);
            } else if wparam.0 == 8 /* WTS_SESSION_UNLOCK */ {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                repaint_layered_bar(hwnd);
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

fn dispatch_ipc_async(hwnd: HWND, req: HyprIpcRequest) {
    let hwnd_raw = hwnd.0 as usize;
    std::thread::spawn(move || {
        let _ = send_ipc_request(&req);
        poll_and_publish_bar_telemetry();
        if !TELEMETRY_PENDING.swap(true, std::sync::atomic::Ordering::SeqCst) {
            unsafe {
                let h = HWND(hwnd_raw as *mut core::ffi::c_void);
                let _ = PostMessageW(h, WM_BAR_TELEMETRY_READY, WPARAM(0), LPARAM(0));
            }
        }
    });
}

fn spawn_background_telemetry_poller(hwnd_raw: usize) {
    std::thread::Builder::new()
        .name("hyprbar-telemetry-poller".to_string())
        .spawn(move || {
            attach_current_thread_to_default_desktop();
            loop {
                poll_and_publish_bar_telemetry();
                check_and_hot_reload_hyprland_conf();
                if !TELEMETRY_PENDING.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    unsafe {
                        let h = HWND(hwnd_raw as *mut core::ffi::c_void);
                        if PostMessageW(h, WM_BAR_TELEMETRY_READY, WPARAM(0), LPARAM(0)).is_err() {
                            break;
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        })
        .ok();
}

// ----------------------------------------------------------------------------
// Real-Time `<userfolder>/.config/hyprwin` Watcher
// ----------------------------------------------------------------------------

static LAST_HYPRCONF_HASH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn check_and_hot_reload_hyprland_conf() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let conf_path = hyprland_win_core::hyprland_win_config_file("hyprland.conf");
    let Ok(content) = fs::read_to_string(&conf_path) else {
        return;
    };

    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    let cur_hash = hasher.finish().max(1);
    let prev_hash = LAST_HYPRCONF_HASH.swap(cur_hash, std::sync::atomic::Ordering::SeqCst);
    if prev_hash == cur_hash {
        return;
    }

    let mtime_mutex = LAST_HYPRCONF_MTIME.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = mtime_mutex.lock() {
        *guard = fs::metadata(&conf_path).and_then(|m| m.modified()).ok();
    }

    // Push live config reload into the running `hyprland-win.exe` daemon via IPC
    let _ = send_ipc_request(&HyprIpcRequest::Reload);
}

fn is_hyprland_win_compositor_alive() -> bool {
    unsafe {
        for cls_name in ["HyprlandWinDCompOverlayClass\0", "HyprlandWinNativeDCompOverlay\0"] {
            let cls: Vec<u16> = cls_name.encode_utf16().collect();
            if let Ok(h) = windows::Win32::UI::WindowsAndMessaging::FindWindowW(
                PCWSTR(cls.as_ptr()),
                PCWSTR::null(),
            ) {
                if !h.0.is_null() {
                    return true;
                }
            }
        }
    }
    match OpenOptions::new().read(true).write(true).open(HYPRLAND_WIN_IPC_PIPE) {
        Ok(_) => true,
        Err(e) => e.raw_os_error() == Some(5) || e.raw_os_error() == Some(231),
    }
}

fn send_ipc_request(req: &HyprIpcRequest) -> Option<HyprIpcResponse> {
    let mut pipe_opt = None;
    for _ in 0..4 {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .open(HYPRLAND_WIN_IPC_PIPE)
        {
            Ok(p) => {
                pipe_opt = Some(p);
                break;
            }
            Err(e) if e.raw_os_error() == Some(5) => {
                return None;
            }
            Err(_) => {}
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

/// Collects IPC (`BarState`) and system telemetry (`widgets`, CPU, RAM, Clock) completely
/// outside `BAR_STATE.lock()`, then updates `BAR_STATE` in < 0.01ms so the Win32 UI thread
/// never blocks or contends on Named Pipe I/O!
fn poll_and_publish_bar_telemetry() {
    let widgets_snap = widgets::get_widgets_snapshot();
    let ipc_resp = send_ipc_request(&HyprIpcRequest::BarState);
    let parsed_snap = ipc_resp
        .as_ref()
        .filter(|r| r.ok)
        .and_then(|r| serde_json::from_str::<HyprBarStateSnapshot>(&r.payload).ok());
    let alive_fallback = if parsed_snap.is_none() {
        is_hyprland_win_compositor_alive()
    } else {
        true
    };

    let Some(st_mutex) = BAR_STATE.get() else {
        return;
    };
    let Ok(mut st) = st_mutex.lock() else {
        return;
    };

    st.model.widgets = widgets_snap;
    if let Some(snap) = parsed_snap {
        st.disconnected_ticks = 0;
        st.model.active_workspace = snap.active_workspace;
        st.model.occupied_workspaces = snap.occupied_workspaces;
        st.model.workspace_hwnds = snap.workspace_hwnds;
        st.model.layout_mode = snap.layout_mode;
        st.model.tiling_active = snap.tiling_active;
        st.model.active_class = snap.active_class;
        st.model.active_title = snap.active_title;
        st.model.active_floating = snap.active_floating;
        st.model.active_fullscreen = snap.active_fullscreen;
    } else if alive_fallback {
        st.disconnected_ticks = 0;
    } else {
        st.disconnected_ticks = st.disconnected_ticks.saturating_add(1);
    }

    unsafe {
        let now = Instant::now();
        let should_poll_cpu = match st.last_cpu_poll {
            None => true,
            Some(last) => now.duration_since(last) >= std::time::Duration::from_secs(3),
        };
        if should_poll_cpu {
            let mut idle = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();
            if GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).is_ok() {
                let i = ((idle.dwHighDateTime as u64) << 32) | (idle.dwLowDateTime as u64);
                let k = ((kernel.dwHighDateTime as u64) << 32) | (kernel.dwLowDateTime as u64);
                let u = ((user.dwHighDateTime as u64) << 32) | (user.dwLowDateTime as u64);

                let di = i.saturating_sub(st.prev_idle);
                let dk = k.saturating_sub(st.prev_kernel);
                let du = u.saturating_sub(st.prev_user);
                let total = dk.saturating_add(du);

                if total > 0 && st.prev_kernel > 0 {
                    let busy = total.saturating_sub(di);
                    st.model.cpu_usage_pct =
                        ((busy as f64 / total as f64) * 100.0).clamp(0.0, 100.0) as u32;
                    st.last_cpu_poll = Some(now);
                }
                st.prev_idle = i;
                st.prev_kernel = k;
                st.prev_user = u;
            }
        }

        let mut mem = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        if GlobalMemoryStatusEx(&mut mem).is_ok() {
            let total_gb = mem.ullTotalPhys as f32 / (1024.0 * 1024.0 * 1024.0);
            let avail_gb = mem.ullAvailPhys as f32 / (1024.0 * 1024.0 * 1024.0);
            st.model.ram_used_gb = (total_gb - avail_gb).max(0.1);
            st.model.ram_total_gb = total_gb.max(1.0);
        }

        let lt = GetLocalTime();
        st.model.cur_year = lt.wYear as i32;
        st.model.cur_month = lt.wMonth as u32;
        st.model.cur_day = lt.wDay as u32;
        st.model.cur_weekday = lt.wDayOfWeek as u32;
        st.model.clock_text = format!("{:02}:{:02}", lt.wHour, lt.wMinute);

        let month_names = [
            "",
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        let weekday_names = [
            "Sunday",
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
        ];
        let m_str = month_names.get(lt.wMonth as usize).unwrap_or(&"");
        let w_str = weekday_names.get(lt.wDayOfWeek as usize).unwrap_or(&"");
        st.model.clock_alt_text = format!("{:02} {m_str} {} | {w_str}", lt.wDay, lt.wYear);
    }
}

fn is_hyprbar_already_running() -> bool {
    unsafe {
        let cls: Vec<u16> = "HyprBarTopOverlay\0".encode_utf16().collect();
        if let Ok(existing) =
            windows::Win32::UI::WindowsAndMessaging::FindWindowW(PCWSTR(cls.as_ptr()), PCWSTR::null())
        {
            return !existing.0.is_null();
        }
    }
    false
}
