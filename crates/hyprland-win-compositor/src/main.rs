#![windows_subsystem = "windows"]
//! `hyprland-win` — Wayland & Hyprland Compositor Daemon for Windows NT (Hollowed DWM).
//!
//! Usage:
//! - `hyprland-win`                  Start the Wayland-NT server (`\\.\pipe\wayland-0`), `hyprctl` IPC
//!                              server (`\\.\pipe\hyprland-win-ipc`), `SUPER` keybind + mouse drag/resize
//!                              hooks, and DWM hollower / tiling loop.
//! - `hyprland-win --probe`          Inspect all open Win32 windows, extract their zero-copy
//!                              `DwmGetDxSharedSurface` GPU texture handles, and test importing
//!                              them into Direct3D 11 (`OpenSharedResource`) on the primary GPU.
//! - `hyprland-win --no-tiling`      Start with automatic physical tiling paused (toggle with `SUPER + T`).

mod input;
mod renderer;

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::ServerOptions;
use tokio::sync::{mpsc, RwLock};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetSystemMetrics, PostMessageW, SM_CXSCREEN, SM_CYSCREEN, WM_CLOSE,
};

use dwm_hollow::{
    apply_window_geometry_async, enumerate_manageable_windows, force_focus_window,
    hollow_window_dwm_attributes, init_foreground_lock_bypass, kill_explorer_shell,
    move_window_async, restore_explorer_shell, restore_window_dwm_attributes, set_window_cloaked,
    set_window_opacity,
};
use hyprland_win_core::{
    hyprland_win_config_file, HyprBarStateSnapshot, HyprConfig, HyprIpcRequest, HyprIpcResponse,
    LayoutMode, RectF, ResizeCorner, WorkspaceScene, HYPRLAND_WIN_IPC_PIPE,
};
use input::{
    spawn_input_hook_thread, take_latest_drag_state, take_latest_hover_point, update_live_keybinds,
    InputAction,
};
use renderer::D3d11CompositorRenderer;
use wayland_nt::WaylandNtServer;

struct CompositorDaemonState {
    config_path: PathBuf,
    last_config_mtime: Option<std::time::SystemTime>,
    last_config_hash: u64,
    config: HyprConfig,
    scene: WorkspaceScene,
    apply_tiling: bool,
    active_drag_hwnd: Option<u64>,
    active_drag_mode: u8, // 0 = None, 1 = Move, 2 = Resize
    hollowed_hwnds: HashSet<u64>,
    last_applied_rects: HashMap<u64, (i32, i32, i32, i32)>,
    last_hyprbar_fullscreen: Option<bool>,
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

#[tokio::main]
async fn main() -> Result<()> {
    attach_current_thread_to_default_desktop();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    let probe_only = args.iter().any(|a| a == "--probe");
    let apply_tiling = !args.iter().any(|a| a == "--no-tiling");
    let keep_explorer = args.iter().any(|a| a == "--keep-explorer");
    let no_bar = args.iter().any(|a| a == "--no-bar");
    let no_elevate = args.iter().any(|a| a == "--no-elevate");
    let boot_lock = args.iter().any(|a| a == "--boot-lock");

    // 1. Ensure the tile manager (`hyprland-win.exe`) holds Administrator privileges so UIPI-protected
    // system windows (`Taskmgr.exe`, `regedit.exe`, elevated terminals) can be tiled and focused!
    // Uses `schtasks /Run /TN Hyprland-WinBootLock` first so elevation happens with ZERO UAC prompt.
    if !probe_only && !no_elevate && !boot_lock && !is_current_process_elevated() {
        if relaunch_self_as_admin(&args) {
            return Ok(());
        }
    }

    // 2. Enforce strict Single-Instance Kernel Mutex so multiple `hyprland-win.exe` processes can NEVER coexist!
    if !probe_only && !acquire_single_instance_mutex() {
        return Ok(());
    }

    // 3. Zero-Latency Pre-Logon / Boot Takeover:
    // Suspend & kill `explorer.exe` and spawn `hyprlock.exe` BEFORE D3D11 shader compilation or window enumeration!
    if !probe_only {
        if !keep_explorer {
            kill_explorer_shell();
        }
        let uptime_ms = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };
        let boot_flag_path = std::env::temp_dir().join("hyprland_win_boot_unlocked.flag");
        if (boot_lock || uptime_ms < 180_000) && !boot_flag_path.is_file() {
            let _ = std::fs::write(&boot_flag_path, b"1");
            spawn_hyprlock_instant();
        }
    }
    enable_debug_privilege();

    let config_path = resolve_config_path();
    let cfg = HyprConfig::load_from_file(&config_path);
    update_live_keybinds(&cfg.binds);
    let initial_mtime = std::fs::metadata(&config_path)
        .and_then(|m| m.modified())
        .ok();
    let initial_hash = hash_file_contents(&config_path);

    // Disable Windows foreground lock timeout so `follow_mouse = 1` can transfer focus on hover
    init_foreground_lock_bypass();

    let (mon_w, mon_h) = unsafe {
        (
            GetSystemMetrics(SM_CXSCREEN).max(1280),
            GetSystemMetrics(SM_CYSCREEN).max(720),
        )
    };

    tracing::info!(
        "Initializing Hyprland-Win D3D11 Hardware Compositor ({}x{}, layout={:?}, rounding={}px, blur={}x{})",
        mon_w,
        mon_h,
        cfg.layout,
        cfg.rounding,
        cfg.blur_size,
        cfg.blur_passes
    );

    let d3d = D3d11CompositorRenderer::new()?;
    tracing::info!("Direct3D 11 Hardware Device & SDF/Kawase HLSL Shaders compiled successfully.");

    if probe_only {
        return run_zero_copy_gpu_probe(&d3d, mon_w, mon_h, &cfg);
    }

    // Install panic failsafe so if Hyprland-Win ever panics, all windows are uncloaked and explorer.exe restarts
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        for w in enumerate_manageable_windows(None) {
            let hwnd = HWND(w.hwnd as *mut core::ffi::c_void);
            let _ = restore_window_dwm_attributes(hwnd);
        }
        restore_explorer_shell();
        prev_hook(info);
    }));

    // Terminate explorer.exe on Hyprland-Win startup so the Windows taskbar/shell yields 100% of the screen
    if !keep_explorer {
        tracing::info!(
            "Killing explorer.exe shell for full Hyprland-Win takeover (Press Win+Alt+Shift+W at any time to exit Hyprland-Win & restart explorer.exe)"
        );
        kill_explorer_shell();
    }

    // 1. Start the Wayland-NT Display Server (`\\.\pipe\wayland-0`)
    let (wl_tx, mut wl_rx) = mpsc::unbounded_channel();
    let wayland_server = Arc::new(WaylandNtServer::new("wayland-0", wl_tx));
    let wl_clone = Arc::clone(&wayland_server);
    tokio::spawn(async move {
        if let Err(e) = wl_clone.run().await {
            tracing::error!("Wayland-NT server error: {e}");
        }
    });

    // 2. Initialize the DirectComposition Click-Through Border/Corner Overlay & Workspace Scene
    let dcomp_overlay = match d3d.create_dcomp_overlay(mon_w, mon_h) {
        Ok(ov) => {
            tracing::info!(
                "DirectComposition Premultiplied-Alpha SDF Border Overlay active (HWND=0x{:X})",
                ov.hwnd.0 as u64
            );
            Some(ov)
        }
        Err(e) => {
            tracing::warn!("Could not initialize DirectComposition border overlay: {e}");
            None
        }
    };
    let overlay_hwnd_id = dcomp_overlay.as_ref().map(|ov| ov.hwnd.0 as u64);

    let mut scene = WorkspaceScene::new(mon_w, mon_h);
    if no_bar {
        scene.reserved_margins = [0, 0, 0, 0];
    }
    let initial_windows = enumerate_manageable_windows(overlay_hwnd_id);
    scene.sync_bridged_windows(&initial_windows, &cfg);

    let state = Arc::new(RwLock::new(CompositorDaemonState {
        config_path,
        last_config_mtime: initial_mtime,
        last_config_hash: initial_hash,
        config: cfg,
        scene,
        apply_tiling,
        active_drag_hwnd: None,
        active_drag_mode: 0,
        hollowed_hwnds: HashSet::new(),
        last_applied_rects: HashMap::new(),
        last_hyprbar_fullscreen: None,
    }));

    // 3. Start Low-Level Win32 Keyboard & Mouse Hooks (`SUPER` keybinds + `bindm` mouse drag/resize)
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    spawn_input_hook_thread(input_tx);

    // 4. Start the `hyprctl` IPC Pipe Server (`\\.\pipe\hyprland-win-ipc`)
    let ipc_state = Arc::clone(&state);
    tokio::spawn(async move {
        if let Err(e) = run_hyprctl_ipc_server(ipc_state).await {
            tracing::error!("hyprctl IPC server error: {e}");
        }
    });

    // 4b. Auto-launch `hyprbar.exe` top status bar (unless `--no-bar` was passed)
    if !no_bar {
        spawn_hyprbar_process();
    }

    // 4b-ii. Register Hyprland-Win for automatic restart/boot
    register_boot_lock_autostart();

    // 4c. Start `hypridle` inactivity monitor (respects `hyprbar` idle inhibitor coffee toggle)
    spawn_hypridle_monitor();

    tracing::info!(
        "Hyprland-Win Daemon Ready! Wayland socket: \\\\.\\pipe\\wayland-0 | Control socket: {} | SUPER + LMB Drag & SUPER + RMB Scale Active",
        HYPRLAND_WIN_IPC_PIPE
    );

    // 5. Main Compositor, Input Dispatch, DWM Hollowing & DirectComposition Overlay Loop (144Hz+ High-Refresh Pacemaker)
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(7));
    let mut tick_count: u64 = 0;
    let mut last_tick = std::time::Instant::now();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                let st = state.read().await;
                emergency_shutdown_and_restore_explorer(&st.scene);
            }
            Some(wl_evt) = wl_rx.recv() => {
                tracing::info!("Wayland-NT Event: {:?}", wl_evt);
            }
            Some(input_evt) = input_rx.recv() => {
                let mut st = state.write().await;
                handle_input_action(input_evt, &mut st);
                let border_angle_rad = st.config.border_angle_deg.to_radians();
                if let Some(ref ov) = dcomp_overlay {
                    d3d.render_border_overlay_frame(
                        ov,
                        &st.scene,
                        &st.config,
                        border_angle_rad,
                        st.apply_tiling,
                    );
                }
            }
            _ = interval.tick() => {
                let now = std::time::Instant::now();
                let dt = now.duration_since(last_tick).as_secs_f32().clamp(0.001, 0.050);
                last_tick = now;

                tick_count = tick_count.wrapping_add(1);
                let mut st = state.write().await;
                let border_angle_rad = st.config.border_angle_deg.to_radians();

                // Never run window enumeration or bezier animation ticks while the user is actively
                // holding a mouse drag or resize (`active_drag_mode != 0`).
                if st.active_drag_mode == 0 {
                    if tick_count % 8 == 1 || st.last_applied_rects.is_empty() {
                        if !keep_explorer {
                            dwm_hollow::suppress_explorer_shell_surfaces();
                        }
                        let bridged = enumerate_manageable_windows(overlay_hwnd_id);
                        let cfg_clone = st.config.clone();
                        st.scene.sync_bridged_windows(&bridged, &cfg_clone);

                        // Self-healing tiled geometry enforcement: if a user dragged a tiled window by its
                        // native Win32 titlebar (without holding SUPER) or double-clicked to maximize it,
                        // detect the physical drift once LMB is released and snap it right back into its tile!
                        let lmb_down = unsafe {
                            (windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(0x01)
                                as u16
                                & 0x8000)
                                != 0
                        };
                        if !lmb_down && st.apply_tiling {
                            let active_ws = st.scene.active_workspace;
                            for bw in &bridged {
                                let _ = dwm_hollow::restore_if_minimized_or_maximized(bw.hwnd);
                                if let Some(c) = st.scene.clients.get(&bw.hwnd) {
                                    if c.workspace_id == active_ws
                                        && !c.floating
                                        && c.anim_progress >= 1.0
                                    {
                                        let hwnd = HWND(bw.hwnd as *mut core::ffi::c_void);
                                        let (ml, mt, mr, mb) =
                                            dwm_hollow::query_invisible_frame_margins(hwnd);
                                        let vis_x = bw.x + ml;
                                        let vis_y = bw.y + mt;
                                        let vis_w = bw.width - ml - mr;
                                        let vis_h = bw.height - mt - mb;
                                        let tx = c.target_rect.x.round() as i32;
                                        let ty = c.target_rect.y.round() as i32;
                                        let tw = c.target_rect.w.round() as i32;
                                        let th = c.target_rect.h.round() as i32;
                                        if (vis_x - tx).abs() > 6
                                            || (vis_y - ty).abs() > 6
                                            || (vis_w - tw).abs() > 6
                                            || (vis_h - th).abs() > 6
                                        {
                                            st.last_applied_rects.remove(&bw.hwnd);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    let cfg_clone = st.config.clone();
                    let _animating = st.scene.tick_animations(dt, &cfg_clone);
                    sync_active_workspace_geometries(&mut st);

                    // Live config hot-reload watcher (~196ms interval on `<userfolder>/.config/hyprwin/hyprland.conf`)
                    if tick_count % 28 == 0 {
                        check_and_reload_live_config(&mut st, false);
                    }

                    // Enforce absolute cursor focus every ~100ms (14 * 7ms = 98ms)
                    if tick_count % 14 == 0 {
                        enforce_absolute_cursor_focus(&mut st);
                    }
                }

                if let Some(ref ov) = dcomp_overlay {
                    d3d.render_border_overlay_frame(
                        ov,
                        &st.scene,
                        &st.config,
                        border_angle_rad,
                        st.apply_tiling,
                    );
                }
            }
        }
    }
}

/// Synchronize Win32 `HWND` positions with the current scene rectangles on the active workspace,
/// deduplicating `hollow_window_dwm_attributes` and `apply_window_geometry_async` calls so
/// stationary windows incur 0 Win32 calls.
fn sync_active_workspace_geometries(st: &mut CompositorDaemonState) {
    let active_ws = st.scene.active_workspace;
    let special_open = st.scene.special_workspace_open;
    let dragged = st.active_drag_hwnd;
    let drag_mode = st.active_drag_mode;
    let mut active_ws_has_fullscreen = false;
    let mut active_ws_fullscreen_id: Option<u64> = None;
    let mut active_ws_other_ids: Vec<u64> = Vec::new();
    let mut active_ws_floating_ids: Vec<u64> = Vec::new();
    let mut active_ws_tiled_ids: Vec<u64> = Vec::new();

    for client in st.scene.clients.values() {
        let hwnd = HWND(client.id as *mut core::ffi::c_void);
        let is_active_special =
            client.workspace_id == hyprland_win_core::SPECIAL_WORKSPACE_ID && special_open;

        if client.workspace_id != active_ws && !is_active_special {
            // If an outgoing workspace window is still sliding off-screen (`anim_progress < 1.0`),
            // drive its slide via fast `move_window_async` (`SWP_NOSIZE`)!
            if client.anim_progress < 1.0 {
                let tx = client.current_rect.x.round() as i32;
                let ty = client.current_rect.y.round() as i32;
                let _ = move_window_async(hwnd, tx, ty);
                continue;
            }
            // Once the slide completes (`anim_progress >= 1.0`), cloak and park it
            if st.last_applied_rects.remove(&client.id).is_some() {
                let _ = set_window_cloaked(hwnd, true);
            }
            continue;
        }

        if client.workspace_id == active_ws || is_active_special {
            if client.fullscreen {
                active_ws_has_fullscreen = true;
                active_ws_fullscreen_id = Some(client.id);
            } else {
                active_ws_other_ids.push(client.id);
                if client.floating || is_active_special {
                    active_ws_floating_ids.push(client.id);
                } else {
                    active_ws_tiled_ids.push(client.id);
                }
            }
        }

        // Skip the dragged window during a Move drag (`drag_mode == 1`) or Floating Resize (`drag_mode == 2 && client.floating`)
        // because its position is driven directly by the cursor. During a Tiled Resize (`drag_mode == 2 && !client.floating`),
        // ALL tiled windows (including the grabbed one) are updated concurrently!
        if Some(client.id) == dragged && (drag_mode == 1 || client.floating) {
            continue;
        }

        if !st.hollowed_hwnds.contains(&client.id) {
            let _ = hollow_window_dwm_attributes(hwnd);
            st.hollowed_hwnds.insert(client.id);
        }

        let target_opacity = if client.focused {
            st.config.active_opacity
        } else {
            st.config.inactive_opacity
        };
        set_window_opacity(hwnd, target_opacity);

        if !st.apply_tiling && !client.floating && !client.fullscreen {
            continue;
        }

        let target_tuple = (
            client.current_rect.x.round() as i32,
            client.current_rect.y.round() as i32,
            client.current_rect.w.round() as i32,
            client.current_rect.h.round() as i32,
        );

        if st.last_applied_rects.get(&client.id).copied() != Some(target_tuple) {
            let prev_size = st
                .last_applied_rects
                .get(&client.id)
                .map(|&(_, _, pw, ph)| (pw, ph));
            if prev_size == Some((target_tuple.2, target_tuple.3)) {
                let _ = move_window_async(hwnd, target_tuple.0, target_tuple.1);
            } else {
                let _ = apply_window_geometry_async(
                    hwnd,
                    target_tuple.0,
                    target_tuple.1,
                    target_tuple.2,
                    target_tuple.3,
                );
            }
            st.last_applied_rects.insert(client.id, target_tuple);
        }
    }

    let fs_transition = st.last_hyprbar_fullscreen != Some(active_ws_has_fullscreen);
    if fs_transition {
        st.last_hyprbar_fullscreen = Some(active_ws_has_fullscreen);
        sync_hyprbar_z_order(active_ws_has_fullscreen);
    }

    // Ensure fullscreen tile on the active workspace strictly holds the highest Z-order
    // above all other tiles/floating windows in its workspace, and otherwise ensure
    // all floating windows stay strictly above normally tiled windows!
    if let Some(fs_id) = active_ws_fullscreen_id {
        if !special_open {
            enforce_workspace_fullscreen_z_order(fs_id, &active_ws_other_ids, fs_transition);
        }
    } else if !active_ws_floating_ids.is_empty() && !active_ws_tiled_ids.is_empty() {
        enforce_floating_above_tiled_z_order(&active_ws_floating_ids, &active_ws_tiled_ids, false);
    }

    // Prevent off-workspace focus theft: if a background window on an inactive workspace called
    // `SetForegroundWindow` on itself, re-cloak it immediately and return focus to `active_ws`!
    let fg_hwnd = unsafe { GetForegroundWindow() };
    let fg_id = fg_hwnd.0 as u64;
    if let Some(fg_client) = st.scene.clients.get(&fg_id) {
        let fg_on_active = fg_client.workspace_id == active_ws
            || (fg_client.workspace_id == hyprland_win_core::SPECIAL_WORKSPACE_ID && special_open);
        if !fg_on_active && fg_client.anim_progress >= 1.0 {
            let _ = set_window_cloaked(fg_hwnd, true);
            if let Some( fallback_id ) = st
                .scene
                .workspace_order
                .get(&active_ws)
                .and_then(|list| list.last().copied())
            {
                force_focus_window(HWND(fallback_id as *mut core::ffi::c_void));
            }
        }
    }
}

/// Immediately synchronize `hyprbar`'s Z-order so fullscreen (`SUPER + F`) windows cover the top bar
/// (`HWND_BOTTOM`), while normal tiled/floating workspaces keep `hyprbar` topmost (`HWND_TOPMOST`).
/// Uses `SWP_ASYNCWINDOWPOS` so `hyprland-win.exe` never synchronously blocks on `hyprbar.exe`'s UI thread!
fn sync_hyprbar_z_order(fullscreen_active: bool) {
    unsafe {
        use windows::core::PCWSTR;
        use windows::Win32::UI::WindowsAndMessaging::{
            FindWindowW, SetWindowPos, HWND_BOTTOM, HWND_TOPMOST, SWP_ASYNCWINDOWPOS,
            SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        };
        let bar_cls: Vec<u16> = "HyprBarTopOverlay\0".encode_utf16().collect();
        if let Ok(bar_hwnd) = FindWindowW(PCWSTR(bar_cls.as_ptr()), PCWSTR::null()) {
            if !bar_hwnd.0.is_null() {
                let z = if fullscreen_active {
                    HWND_BOTTOM
                } else {
                    HWND_TOPMOST
                };
                let _ = SetWindowPos(
                    bar_hwnd,
                    z,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
                );
            }
        }
    }
}

/// Ensure the fullscreen window (`fs_id`) on the active workspace has the highest Z-order
/// among all windows on that workspace (`other_ws_ids`).
fn enforce_workspace_fullscreen_z_order(fs_id: u64, other_ws_ids: &[u64], force_restack: bool) {
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{
            BringWindowToTop, GetWindow, SetWindowPos, GW_HWNDPREV, HWND_TOP, SWP_ASYNCWINDOWPOS,
            SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
        };
        let fs_hwnd = HWND(fs_id as *mut core::ffi::c_void);
        if fs_hwnd.0.is_null() {
            return;
        }

        // Check if any sibling window on the same workspace is currently above `fs_hwnd` in Z-order
        let mut sibling_above = force_restack;
        if !sibling_above && !other_ws_ids.is_empty() {
            let mut cur = GetWindow(fs_hwnd, GW_HWNDPREV).unwrap_or_default();
            let mut steps = 0;
            while !cur.0.is_null() && steps < 256 {
                let cid = cur.0 as u64;
                if other_ws_ids.contains(&cid) {
                    sibling_above = true;
                    break;
                }
                cur = GetWindow(cur, GW_HWNDPREV).unwrap_or_default();
                steps += 1;
            }
        }

        if sibling_above {
            let _ = BringWindowToTop(fs_hwnd);
            let _ = SetWindowPos(
                fs_hwnd,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS,
            );
            for &other_id in other_ws_ids {
                let other_hwnd = HWND(other_id as *mut core::ffi::c_void);
                let _ = SetWindowPos(
                    other_hwnd,
                    fs_hwnd,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE
                        | SWP_NOSIZE
                        | SWP_NOACTIVATE
                        | SWP_NOOWNERZORDER
                        | SWP_ASYNCWINDOWPOS,
                );
            }
        }
    }
}

/// Ensure all floating windows (`floating_ids`) on the active workspace always stay visually
/// above all normally tiled windows (`tiled_ids`), even when a tiled window holds keyboard focus.
fn enforce_floating_above_tiled_z_order(
    floating_ids: &[u64],
    tiled_ids: &[u64],
    force_restack: bool,
) {
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindow, SetWindowPos, GW_HWNDPREV, HWND_TOP, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE,
            SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
        };
        if floating_ids.is_empty() || tiled_ids.is_empty() {
            return;
        }

        let mut tiled_above_floating = force_restack;
        if !tiled_above_floating {
            'outer: for &fl_id in floating_ids {
                let fl_hwnd = HWND(fl_id as *mut core::ffi::c_void);
                if fl_hwnd.0.is_null() {
                    continue;
                }
                let mut cur = GetWindow(fl_hwnd, GW_HWNDPREV).unwrap_or_default();
                let mut steps = 0;
                while !cur.0.is_null() && steps < 256 {
                    let cid = cur.0 as u64;
                    if tiled_ids.contains(&cid) {
                        tiled_above_floating = true;
                        break 'outer;
                    }
                    cur = GetWindow(cur, GW_HWNDPREV).unwrap_or_default();
                    steps += 1;
                }
            }
        }

        if tiled_above_floating {
            // Raise floating windows to HWND_TOP without stealing keyboard focus (SWP_NOACTIVATE),
            // and place all tiled windows behind the last floating window!
            let mut last_fl_hwnd = HWND::default();
            for &fl_id in floating_ids {
                let fl_hwnd = HWND(fl_id as *mut core::ffi::c_void);
                if !fl_hwnd.0.is_null() {
                    let _ = SetWindowPos(
                        fl_hwnd,
                        HWND_TOP,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE
                            | SWP_NOSIZE
                            | SWP_NOACTIVATE
                            | SWP_NOOWNERZORDER
                            | SWP_ASYNCWINDOWPOS,
                    );
                    last_fl_hwnd = fl_hwnd;
                }
            }
            if !last_fl_hwnd.0.is_null() {
                for &t_id in tiled_ids {
                    let t_hwnd = HWND(t_id as *mut core::ffi::c_void);
                    if !t_hwnd.0.is_null() {
                        let _ = SetWindowPos(
                            t_hwnd,
                            last_fl_hwnd,
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE
                                | SWP_NOSIZE
                                | SWP_NOACTIVATE
                                | SWP_NOOWNERZORDER
                                | SWP_ASYNCWINDOWPOS,
                        );
                    }
                }
            }
        }
    }
}

/// Process interactive keybind and mouse drag/resize actions from the `WH_KEYBOARD_LL` / `WH_MOUSE_LL` hooks.
fn handle_input_action(action: InputAction, st: &mut CompositorDaemonState) {
    let cfg_clone = st.config.clone();
    match action {
        InputAction::BeginDragMove {
            hwnd,
            initial_rect,
            ..
        } => {
            if !st.scene.clients.contains_key(&hwnd) {
                return;
            }
            st.active_drag_hwnd = Some(hwnd);
            st.active_drag_mode = 1;
            st.scene.active_drag_hwnd = Some(hwnd);
            let win_hwnd = HWND(hwnd as *mut core::ffi::c_void);
            force_focus_window(win_hwnd);
            for c in st.scene.clients.values_mut() {
                c.focused = c.id == hwnd;
            }
            if let Some(c) = st.scene.clients.get_mut(&hwnd) {
                c.start_rect = initial_rect;
                c.current_rect = initial_rect;
                c.anim_progress = 1.0;
            }
        }
        InputAction::EndDragMove { hwnd } => {
            st.active_drag_hwnd = None;
            st.active_drag_mode = 0;
            st.scene.active_drag_hwnd = None;
            if !st.scene.clients.contains_key(&hwnd) {
                return;
            }
            st.last_applied_rects.remove(&hwnd);
            let ws = st.scene.active_workspace;
            if let Some(c) = st.scene.clients.get(&hwnd) {
                if !c.floating {
                    st.scene.recalculate_layout(ws, &cfg_clone);
                }
            }
            sync_active_workspace_geometries(st);
        }
        InputAction::BeginDragResize {
            hwnd,
            initial_rect,
            corner,
            ..
        } => {
            if !st.scene.clients.contains_key(&hwnd) {
                return;
            }
            st.active_drag_hwnd = Some(hwnd);
            st.active_drag_mode = 2;
            // Only lock `scene.active_drag_hwnd` if the window is floating; for tiled windows,
            // `recalculate_layout` must update the grabbed tile's target_rect too!
            let is_floating = st
                .scene
                .clients
                .get(&hwnd)
                .map(|c| c.floating)
                .unwrap_or(false);
            st.scene.active_drag_hwnd = if is_floating { Some(hwnd) } else { None };

            let win_hwnd = HWND(hwnd as *mut core::ffi::c_void);
            force_focus_window(win_hwnd);
            for c in st.scene.clients.values_mut() {
                c.focused = c.id == hwnd;
            }
            if let Some(c) = st.scene.clients.get_mut(&hwnd) {
                c.start_rect = initial_rect;
                c.current_rect = initial_rect;
                c.anim_progress = 1.0;
            }
            tracing::debug!("Begin SUPER + RMB Resize on 0x{:X} ({:?})", hwnd, corner);
        }
        InputAction::EndDragResize { hwnd } => {
            st.active_drag_hwnd = None;
            st.active_drag_mode = 0;
            st.scene.active_drag_hwnd = None;
            if !st.scene.clients.contains_key(&hwnd) {
                return;
            }
            st.last_applied_rects.remove(&hwnd);
            sync_active_workspace_geometries(st);
        }
        InputAction::PollActiveDrag => {
            let Some(drag) = take_latest_drag_state() else {
                return;
            };
            let hwnd = drag.hwnd;
            let Some(is_floating) = st.scene.clients.get(&hwnd).map(|c| c.floating) else {
                return;
            };
            let win_hwnd = HWND(hwnd as *mut core::ffi::c_void);

            if drag.mode == 1 {
                // Coalesced Move Drag: use `move_window_async` (`SWP_NOSIZE`) so the target window
                // never runs `WM_SIZE` layout/reflow during translation!
                let new_x = (drag.initial_rect.x + drag.dx as f32).round() as i32;
                let new_y = (drag.initial_rect.y + drag.dy as f32).round() as i32;
                let _ = move_window_async(win_hwnd, new_x, new_y);

                if let Some(c) = st.scene.clients.get_mut(&hwnd) {
                    let r = RectF::new(
                        new_x as f32,
                        new_y as f32,
                        drag.initial_rect.w,
                        drag.initial_rect.h,
                    );
                    c.start_rect = r;
                    c.current_rect = r;
                    if is_floating || !st.apply_tiling {
                        c.target_rect = r;
                    }
                    c.anim_progress = 1.0;
                }

                if is_floating || !st.apply_tiling {
                    st.last_applied_rects.insert(
                        hwnd,
                        (
                            new_x,
                            new_y,
                            drag.initial_rect.w.round() as i32,
                            drag.initial_rect.h.round() as i32,
                        ),
                    );
                } else if let Some(target_tile) = st.scene.tiled_window_at_point(
                    drag.cursor_x as f32,
                    drag.cursor_y as f32,
                    Some(hwnd),
                ) {
                    let mut cfg_instant = cfg_clone;
                    cfg_instant.animations_enabled = false;
                    if st
                        .scene
                        .swap_windows_in_workspace(hwnd, target_tile, &cfg_instant)
                    {
                        sync_active_workspace_geometries(st);
                    }
                }
            } else if drag.mode == 2 {
                // Coalesced Resize Drag (Quadrant Floating Scale OR Multi-Window 2D Tiled Split Scale)
                if is_floating || !st.apply_tiling {
                    let min_w = 180.0_f32;
                    let min_h = 120.0_f32;
                    let mut x = drag.initial_rect.x;
                    let mut y = drag.initial_rect.y;
                    let mut w = drag.initial_rect.w;
                    let mut h = drag.initial_rect.h;

                    match drag.corner {
                        ResizeCorner::BottomRight => {
                            w = (drag.initial_rect.w + drag.dx as f32).max(min_w);
                            h = (drag.initial_rect.h + drag.dy as f32).max(min_h);
                        }
                        ResizeCorner::BottomLeft => {
                            let new_w = (drag.initial_rect.w - drag.dx as f32).max(min_w);
                            x = drag.initial_rect.x + (drag.initial_rect.w - new_w);
                            w = new_w;
                            h = (drag.initial_rect.h + drag.dy as f32).max(min_h);
                        }
                        ResizeCorner::TopRight => {
                            w = (drag.initial_rect.w + drag.dx as f32).max(min_w);
                            let new_h = (drag.initial_rect.h - drag.dy as f32).max(min_h);
                            y = drag.initial_rect.y + (drag.initial_rect.h - new_h);
                            h = new_h;
                        }
                        ResizeCorner::TopLeft => {
                            let new_w = (drag.initial_rect.w - drag.dx as f32).max(min_w);
                            let new_h = (drag.initial_rect.h - drag.dy as f32).max(min_h);
                            x = drag.initial_rect.x + (drag.initial_rect.w - new_w);
                            y = drag.initial_rect.y + (drag.initial_rect.h - new_h);
                            w = new_w;
                            h = new_h;
                        }
                    }

                    if let Some(c) = st.scene.clients.get_mut(&hwnd) {
                        let r = RectF::new(x, y, w, h);
                        c.start_rect = r;
                        c.current_rect = r;
                        c.target_rect = r;
                        c.anim_progress = 1.0;
                    }
                    let tuple = (
                        x.round() as i32,
                        y.round() as i32,
                        w.round() as i32,
                        h.round() as i32,
                    );
                    let _ = apply_window_geometry_async(win_hwnd, tuple.0, tuple.1, tuple.2, tuple.3);
                    st.last_applied_rects.insert(hwnd, tuple);
                } else {
                    // Tiled window scaling: adjust both Horizontal and Vertical Dwindle/Master splits live!
                    let mut cfg_instant = cfg_clone;
                    cfg_instant.animations_enabled = false;
                    st.scene.adjust_tiled_split_ratio(
                        hwnd,
                        drag.corner,
                        drag.step_dx as f32,
                        drag.step_dy as f32,
                        &cfg_instant,
                    );
                    sync_active_workspace_geometries(st);
                }
            }
        }
        InputAction::ToggleFloating(opt_hwnd) => {
            let target = opt_hwnd.or_else(|| active_window_on_current_workspace(&st.scene));
            if let Some(id) = target {
                if let Some(floating) = st.scene.toggle_floating(id, &cfg_clone) {
                    st.last_applied_rects.remove(&id);
                    sync_active_workspace_geometries(st);
                    if floating {
                        let ws = st.scene.active_workspace;
                        let fl_ids: Vec<u64> = st
                            .scene
                            .clients
                            .values()
                            .filter(|c| c.workspace_id == ws && c.floating && !c.fullscreen)
                            .map(|c| c.id)
                            .collect();
                        let tl_ids: Vec<u64> = st
                            .scene
                            .clients
                            .values()
                            .filter(|c| c.workspace_id == ws && !c.floating && !c.fullscreen)
                            .map(|c| c.id)
                            .collect();
                        enforce_floating_above_tiled_z_order(&fl_ids, &tl_ids, true);
                    }
                    tracing::info!("Toggled floating={} for window 0x{:X}", floating, id);
                }
            }
        }
        InputAction::ToggleFullscreen => {
            if let Some(id) = active_window_on_current_workspace(&st.scene) {
                if let Some(fs) = st.scene.toggle_fullscreen(id, &cfg_clone) {
                    st.last_applied_rects.clear();
                    st.last_hyprbar_fullscreen = None;
                    sync_active_workspace_geometries(st);
                    force_focus_window(HWND(id as *mut core::ffi::c_void));
                    tracing::info!("Toggled fullscreen={} for window 0x{:X}", fs, id);
                }
            }
        }
        InputAction::TogglePhysicalTiling => {
            st.apply_tiling = !st.apply_tiling;
            if st.apply_tiling {
                st.last_applied_rects.clear();
                sync_active_workspace_geometries(st);
            }
            tracing::info!("Physical tiling toggled: {}", st.apply_tiling);
        }
        InputAction::ToggleLayoutMode => {
            st.config.layout = match st.config.layout {
                LayoutMode::Dwindle => LayoutMode::Master,
                LayoutMode::Master => LayoutMode::Dwindle,
            };
            let ws = st.scene.active_workspace;
            let cfg2 = st.config.clone();
            st.scene.recalculate_layout(ws, &cfg2);
            sync_active_workspace_geometries(st);
            tracing::info!("Switched layout mode to {:?}", st.config.layout);
        }
        InputAction::ToggleGroup => {
            if let Some(id) = active_window_on_current_workspace(&st.scene) {
                let grouped = st.scene.toggle_group(id, &cfg_clone);
                st.last_applied_rects.clear();
                sync_active_workspace_geometries(st);
                tracing::info!("Toggled group={} for window 0x{:X}", grouped, id);
            }
        }
        InputAction::ChangeGroupActive(forward) => {
            if let Some(next_id) = st.scene.change_group_active(forward, &cfg_clone) {
                st.last_applied_rects.clear();
                sync_active_workspace_geometries(st);
                force_focus_window(HWND(next_id as *mut core::ffi::c_void));
                for c in st.scene.clients.values_mut() {
                    c.focused = c.id == next_id;
                }
                tracing::info!("Changed active group tab to 0x{:X}", next_id);
            }
        }
        InputAction::FocusDirection(dir) => {
            if let Some(from) = active_window_on_current_workspace(&st.scene) {
                if let Some(neighbor) = st.scene.find_neighbor_in_direction(from, dir) {
                    warp_cursor_to_window_center(&st.scene, neighbor);
                    force_focus_window(HWND(neighbor as *mut core::ffi::c_void));
                    for c in st.scene.clients.values_mut() {
                        c.focused = c.id == neighbor;
                    }
                }
            }
        }
        InputAction::MouseHoverMoved => {
            let _ = take_latest_hover_point();
            enforce_absolute_cursor_focus(st);
        }
        InputAction::SwapDirection(dir) => {
            if let Some(from) = active_window_on_current_workspace(&st.scene) {
                if let Some(neighbor) = st.scene.find_neighbor_in_direction(from, dir) {
                    st.scene.swap_windows_in_workspace(from, neighbor, &cfg_clone);
                    sync_active_workspace_geometries(st);
                    warp_cursor_to_window_center(&st.scene, from);
                }
            }
        }
        InputAction::SwitchWorkspace(ws) => {
            switch_to_workspace(st, ws.clamp(1, 10));
        }
        InputAction::CycleWorkspace(delta) => {
            let cur = st.scene.active_workspace as i32;
            let next = (cur + delta).clamp(1, 10) as u32;
            switch_to_workspace(st, next);
        }
        InputAction::MoveActiveToWorkspace(ws) => {
            let target_ws = ws.clamp(1, 10);
            let active_ws = st.scene.active_workspace;
            if target_ws == active_ws {
                return;
            }
            if let Some(id) = active_window_on_current_workspace(&st.scene) {
                if st.scene.move_window_to_workspace(id, target_ws, &cfg_clone) {
                    st.last_applied_rects.remove(&id);
                    tracing::info!(
                        "Moved window 0x{:X} to workspace {} and switching to workspace {}",
                        id,
                        target_ws,
                        target_ws
                    );
                    // Automatically swap to the target workspace with smooth slide animation,
                    // keeping the moved window focused on arrival!
                    switch_to_workspace_with_moved_window(st, target_ws, Some(id));
                }
            }
        }
        InputAction::MoveActiveToWorkspaceSilent(ws) => {
            let target_ws = ws.clamp(1, 10);
            let active_ws = st.scene.active_workspace;
            if target_ws == active_ws {
                return;
            }
            if let Some(id) = active_window_on_current_workspace(&st.scene) {
                if st.scene.move_window_to_workspace(id, target_ws, &cfg_clone) {
                    st.last_applied_rects.remove(&id);
                    let hwnd = HWND(id as *mut core::ffi::c_void);
                    let _ = set_window_cloaked(hwnd, true);
                    sync_active_workspace_geometries(st);
                    tracing::info!("Silently moved window 0x{:X} to workspace {}", id, target_ws);
                }
            }
        }
        InputAction::ResizeActive(dx, dy) => {
            if let Some(id) = active_window_on_current_workspace(&st.scene) {
                let is_floating = st
                    .scene
                    .clients
                    .get(&id)
                    .map(|c| c.floating)
                    .unwrap_or(false);
                if is_floating || !st.apply_tiling {
                    if let Some(c) = st.scene.clients.get_mut(&id) {
                        let new_w = (c.current_rect.w + dx as f32).max(180.0);
                        let new_h = (c.current_rect.h + dy as f32).max(120.0);
                        let r = RectF::new(c.current_rect.x, c.current_rect.y, new_w, new_h);
                        c.start_rect = r;
                        c.current_rect = r;
                        c.target_rect = r;
                        c.anim_progress = 1.0;
                    }
                    st.last_applied_rects.remove(&id);
                    sync_active_workspace_geometries(st);
                } else {
                    st.scene.adjust_tiled_split_ratio(
                        id,
                        ResizeCorner::BottomRight,
                        dx as f32,
                        dy as f32,
                        &cfg_clone,
                    );
                    sync_active_workspace_geometries(st);
                }
            }
        }
        InputAction::CenterActiveWindow => {
            if let Some(id) = active_window_on_current_workspace(&st.scene) {
                let mon_w = st.scene.monitor_width as f32;
                let mon_h = st.scene.monitor_height as f32;
                let top_m = st.scene.reserved_margins[0] as f32;
                if let Some(c) = st.scene.clients.get_mut(&id) {
                    if c.floating {
                        let nx = ((mon_w - c.current_rect.w) * 0.5).max(8.0);
                        let ny = (top_m + (mon_h - top_m - c.current_rect.h) * 0.5).max(top_m + 4.0);
                        let r = RectF::new(nx, ny, c.current_rect.w, c.current_rect.h);
                        c.start_rect = r;
                        c.current_rect = r;
                        c.target_rect = r;
                        c.anim_progress = 1.0;
                        st.last_applied_rects.remove(&id);
                    }
                }
                sync_active_workspace_geometries(st);
            }
        }
        InputAction::ReloadConfig => {
            check_and_reload_live_config(st, true);
        }
        InputAction::ReloadBar => {
            {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("taskkill.exe")
                    .args([
                        "/F",
                        "/IM",
                        "hyprbar.exe",
                        "/IM",
                        "hyprpaper.exe",
                        "/IM",
                        "hyprlauncher.exe",
                        "/IM",
                        "hyprnotify.exe",
                    ])
                    .creation_flags(0x0800_0000)
                    .status();
            }
            spawn_hyprbar_process();
        }
        InputAction::ToggleSpecialWorkspace => {
            let open = st.scene.toggle_special_workspace();
            if open {
                for client in st.scene.clients.values() {
                    if client.workspace_id == hyprland_win_core::SPECIAL_WORKSPACE_ID {
                        let hwnd = HWND(client.id as *mut core::ffi::c_void);
                        let _ = set_window_cloaked(hwnd, false);
                        force_focus_window(hwnd);
                    }
                }
            }
            sync_active_workspace_geometries(st);
            tracing::info!("Toggled special:scratchpad workspace open={}", open);
        }
        InputAction::MoveActiveToSpecialWorkspace => {
            if let Some(id) = active_window_on_current_workspace(&st.scene) {
                st.last_applied_rects.remove(&id);
                st.scene.move_to_special_workspace(id, &cfg_clone);
                let hwnd = HWND(id as *mut core::ffi::c_void);
                let _ = set_window_cloaked(hwnd, false);
                sync_active_workspace_geometries(st);
                tracing::info!("Moved/restored window 0x{:X} <-> special:scratchpad", id);
            }
        }
        InputAction::CloseActiveWindow => {
            if let Some(id) = active_window_on_current_workspace(&st.scene) {
                let hwnd = HWND(id as *mut core::ffi::c_void);
                unsafe {
                    let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
        }
        InputAction::ExecCommand(cmd) => {
            tracing::info!("Executing keybind command (non-elevated user context): {}", cmd);
            spawn_unelevated_user_command(&cmd);
        }
        InputAction::PanicExitAndRestoreExplorer => {
            emergency_shutdown_and_restore_explorer(&st.scene);
        }
    }
}

/// Find the currently focused/active window on `scene.active_workspace` (ignoring parked windows on other workspaces).
fn active_window_on_current_workspace(scene: &WorkspaceScene) -> Option<u64> {
    let ws = scene.active_workspace;
    if !scene.special_workspace_open {
        if let Some(c) = scene
            .clients
            .values()
            .find(|c| c.workspace_id == ws && c.fullscreen)
        {
            return Some(c.id);
        }
    }
    let fg = unsafe { GetForegroundWindow().0 as u64 };
    if let Some(c) = scene.clients.get(&fg) {
        if c.workspace_id == ws
            || (c.workspace_id == hyprland_win_core::SPECIAL_WORKSPACE_ID && scene.special_workspace_open)
        {
            return Some(fg);
        }
    }
    if let Some(c) = scene
        .clients
        .values()
        .find(|c| c.workspace_id == ws && c.focused)
    {
        return Some(c.id);
    }
    scene
        .workspace_order
        .get(&ws)
        .and_then(|list| list.last().copied())
}

/// Uncloak and restore stock Windows 11 DWM attributes on all managed windows,
/// relaunch `explorer.exe`, and terminate `hyprland-win.exe`.
fn emergency_shutdown_and_restore_explorer(scene: &WorkspaceScene) -> ! {
    tracing::warn!(
        "PANIC COMBO (Win+Alt+Shift+W) / EXIT triggered: Uncloaking all windows, restoring DWM attributes, and restarting explorer.exe..."
    );
    for client_id in scene.clients.keys() {
        let hwnd = HWND(*client_id as *mut core::ffi::c_void);
        let _ = restore_window_dwm_attributes(hwnd);
    }
    for w in enumerate_manageable_windows(None) {
        let hwnd = HWND(w.hwnd as *mut core::ffi::c_void);
        let _ = restore_window_dwm_attributes(hwnd);
    }
    restore_explorer_shell();
    std::process::exit(0);
}

fn switch_to_workspace(st: &mut CompositorDaemonState, target_ws: u32) {
    switch_to_workspace_with_moved_window(st, target_ws, None);
}

fn switch_to_workspace_with_moved_window(
    st: &mut CompositorDaemonState,
    target_ws: u32,
    carried_hwnd: Option<u64>,
) {
    let old_ws = st.scene.active_workspace;
    if old_ws == target_ws {
        return;
    }
    st.scene.active_workspace = target_ws;

    let mon_w = st.scene.monitor_width as f32;
    let slide_dir = if target_ws > old_ws { 1.0_f32 } else { -1.0_f32 };
    let animate = st.config.animations_enabled;

    // 1. Slide all windows on the outgoing workspace off-screen in the opposite direction
    //    (or park immediately if animations are disabled).
    for c in st.scene.clients.values_mut() {
        if c.workspace_id == old_ws {
            if animate {
                c.start_rect = c.current_rect;
                c.target_rect = RectF::new(
                    c.current_rect.x - slide_dir * (mon_w + 80.0),
                    c.current_rect.y,
                    c.current_rect.w,
                    c.current_rect.h,
                );
                c.anim_progress = 0.0;
                // Keep `last_applied_rects` entry so `sync_active_workspace_geometries` parks it
                // at `OFFSCREEN_PARK_X_BASE` as soon as `anim_progress` reaches 1.0.
                st.last_applied_rects.insert(
                    c.id,
                    (
                        c.current_rect.x.round() as i32,
                        c.current_rect.y.round() as i32,
                        c.current_rect.w.round() as i32,
                        c.current_rect.h.round() as i32,
                    ),
                );
            } else {
                let hwnd = HWND(c.id as *mut core::ffi::c_void);
                let _ = set_window_cloaked(hwnd, true);
                st.last_applied_rects.remove(&c.id);
            }
        }
    }

    // 2. Recalculate layout on the target workspace and slide incoming windows in from the side
    //    (while any `carried_hwnd` morphs smoothly from its current on-screen rect to its new tile rect!).
    let mut cfg_instant = st.config.clone();
    cfg_instant.animations_enabled = false;
    st.scene.recalculate_layout(target_ws, &cfg_instant);

    for c in st.scene.clients.values_mut() {
        if c.workspace_id == target_ws {
            let hwnd = HWND(c.id as *mut core::ffi::c_void);
            let _ = set_window_cloaked(hwnd, false);

            if animate {
                if Some(c.id) == carried_hwnd {
                    // Carried window is already on-screen: morph smoothly from its current rect to target_rect!
                    c.start_rect = c.current_rect;
                    c.anim_progress = 0.0;
                } else {
                    // Pre-size the incoming window at its off-screen entrance edge so the slide is 100% `SWP_NOSIZE`!
                    let start_r = RectF::new(
                        c.target_rect.x + slide_dir * (mon_w + 80.0),
                        c.target_rect.y,
                        c.target_rect.w,
                        c.target_rect.h,
                    );
                    c.start_rect = start_r;
                    c.current_rect = start_r;
                    c.anim_progress = 0.0;
                    let init_tuple = (
                        start_r.x.round() as i32,
                        start_r.y.round() as i32,
                        start_r.w.round() as i32,
                        start_r.h.round() as i32,
                    );
                    let _ = apply_window_geometry_async(
                        hwnd,
                        init_tuple.0,
                        init_tuple.1,
                        init_tuple.2,
                        init_tuple.3,
                    );
                    st.last_applied_rects.insert(c.id, init_tuple);
                }
            } else {
                c.current_rect = c.target_rect;
                c.anim_progress = 1.0;
                st.last_applied_rects.remove(&c.id);
            }
        }
    }

    st.last_hyprbar_fullscreen = None;
    sync_active_workspace_geometries(st);

    // 3. Focus logic on workspace switch:
    //    - If `target_ws` has a fullscreen window, always focus and raise that fullscreen window first.
    //    - Else if a window was moved with `SUPER + SHIFT + 1..0` (`carried_hwnd`), center cursor on it and focus it.
    //    - Otherwise, focus the window on `target_ws` directly underneath the mouse cursor!
    let target_ws_fs_id = st
        .scene
        .clients
        .values()
        .find(|c| c.workspace_id == target_ws && c.fullscreen)
        .map(|c| c.id);
    if let Some(fs_id) = target_ws_fs_id {
        force_focus_window(HWND(fs_id as *mut core::ffi::c_void));
        for c in st.scene.clients.values_mut() {
            c.focused = c.id == fs_id;
        }
    } else if let Some(moved_id) = carried_hwnd {
        warp_cursor_to_window_center(&st.scene, moved_id);
        force_focus_window(HWND(moved_id as *mut core::ffi::c_void));
        for c in st.scene.clients.values_mut() {
            c.focused = c.id == moved_id;
        }
    } else {
        enforce_absolute_cursor_focus(st);
    }

    tracing::info!("Switched active workspace: {} -> {}", old_ws, target_ws);
}

fn warp_cursor_to_window_center(scene: &WorkspaceScene, hwnd_id: u64) {
    if let Some(c) = scene.clients.get(&hwnd_id) {
        let r = if c.floating { c.current_rect } else { c.target_rect };
        let cx = (r.x + r.w * 0.5).round() as i32;
        let cy = (r.y + r.h * 0.5).round() as i32;
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetCursorPos(cx, cy);
        }
    }
}

fn enforce_absolute_cursor_focus(st: &mut CompositorDaemonState) {
    if !st.config.follow_mouse || st.active_drag_mode != 0 {
        return;
    }

    let mut pt = windows::Win32::Foundation::POINT::default();
    if unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt) }.is_err() {
        return;
    }
    let ws = st.scene.active_workspace;
    let has_fs = st
        .scene
        .clients
        .values()
        .any(|c| c.workspace_id == ws && c.fullscreen);
    if !has_fs && pt.y < st.scene.reserved_margins[0] {
        return;
    }

    // Do not steal focus while HyprLauncher or an Open With dialog is open
    unsafe {
        for cls_name in ["HyprLauncherClass\0", "Open With\0"] {
            let l_cls: Vec<u16> = cls_name.encode_utf16().collect();
            if let Ok(lh) = windows::Win32::UI::WindowsAndMessaging::FindWindowW(
                windows::core::PCWSTR(l_cls.as_ptr()),
                windows::core::PCWSTR::null(),
            ) {
                if !lh.0.is_null()
                    && windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(lh).as_bool()
                {
                    return;
                }
            }
        }
    }

    if let Some(hovered_id) = st.scene.window_at_point(pt.x as f32, pt.y as f32) {
        let scene_matches = st
            .scene
            .clients
            .get(&hovered_id)
            .map(|c| c.focused)
            .unwrap_or(false);

        if !scene_matches {
            for c in st.scene.clients.values_mut() {
                c.focused = c.id == hovered_id;
            }
            force_focus_window(HWND(hovered_id as *mut core::ffi::c_void));
            let fl_ids: Vec<u64> = st
                .scene
                .clients
                .values()
                .filter(|c| c.workspace_id == ws && c.floating && !c.fullscreen)
                .map(|c| c.id)
                .collect();
            let tl_ids: Vec<u64> = st
                .scene
                .clients
                .values()
                .filter(|c| c.workspace_id == ws && !c.floating && !c.fullscreen)
                .map(|c| c.id)
                .collect();
            enforce_floating_above_tiled_z_order(&fl_ids, &tl_ids, true);
        }
    }
}

fn spawn_hyprbar_process() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    for bin in ["hyprbar.exe", "hyprpaper.exe", "hyprnotify.exe"] {
        if let Ok(cur_exe) = std::env::current_exe() {
            if let Some(dir) = cur_exe.parent() {
                let exe_path = dir.join(bin);
                if exe_path.exists() {
                    let _ = std::process::Command::new(&exe_path)
                        .creation_flags(CREATE_NO_WINDOW)
                        .spawn();
                    tracing::info!("Launched Hyprland-Win companion daemon ({})", bin);
                    continue;
                }
            }
        }
        let _ = std::process::Command::new(bin)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    // Launch `hyprlauncher.exe` in the user's normal (Medium Integrity / non-Admin) context
    // so apps launched from it do NOT inherit Administrator privileges!
    spawn_unelevated_user_command("hyprlauncher.exe");
}

fn spawn_hyprlock_instant() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if let Ok(cur_exe) = std::env::current_exe() {
        if let Some(dir) = cur_exe.parent() {
            let lock_path = dir.join("hyprlock.exe");
            if lock_path.exists() {
                let _ = std::process::Command::new(&lock_path)
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn();
                return;
            }
        }
    }
    let _ = std::process::Command::new("hyprlock.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

fn acquire_single_instance_mutex() -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(
            lp_mutex_attributes: *const core::ffi::c_void,
            b_initial_owner: i32,
            lp_name: *const u16,
        ) -> *mut core::ffi::c_void;
        fn GetLastError() -> u32;
    }
    unsafe {
        let name: Vec<u16> = "Local\\Hyprland-WinCompositorSingleInstanceMutex\0"
            .encode_utf16()
            .collect();
        let h = CreateMutexW(core::ptr::null(), 1, name.as_ptr());
        if h.is_null() {
            return true;
        }
        // ERROR_ALREADY_EXISTS = 183
        GetLastError() != 183
    }
}

fn register_boot_lock_autostart() {
    // Clean up any legacy HKCU\...\Run\Hyprland-Win entry so Windows never spawns a duplicate unelevated instance
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("reg.exe")
        .args([
            "delete",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            "Hyprland-Win",
            "/f",
        ])
        .creation_flags(0x0800_0000)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Probe all open Win32 windows, extract their `DwmGetDxSharedSurface` GPU texture handles,
/// and verify zero-copy `ID3D11Device::OpenSharedResource` import on the GPU.
fn run_zero_copy_gpu_probe(
    d3d: &D3d11CompositorRenderer,
    mon_w: i32,
    mon_h: i32,
    cfg: &HyprConfig,
) -> Result<()> {
    let windows = enumerate_manageable_windows(None);
    let mut scene = WorkspaceScene::new(mon_w, mon_h);
    scene.sync_bridged_windows(&windows, cfg);

    println!(
        "=== Hyprland-Win Zero-Copy DWM Shared Texture & Wayland-NT Probe ({}x{}) ===",
        mon_w, mon_h
    );
    println!(
        "{:<12} {:<24} {:<16} {:<14} {:<22} {}",
        "HWND", "EXE", "SHARED_HANDLE", "D3D11_IMPORT", "DWINDLE_TILE_RECT", "TITLE"
    );
    println!("{}", "-".repeat(115));

    let mut imported_count = 0usize;
    for w in &windows {
        let import_status = if w.shared_surface_handle != 0 {
            match d3d.open_shared_window_srv(w.shared_surface_handle) {
                Ok((_tex, _srv)) => {
                    imported_count += 1;
                    "ZERO-COPY OK"
                }
                Err(_) => "DRV_PRIVATE",
            }
        } else if w.is_dcomp_visual {
            match d3d.capture_dcomp_window_srv(w.hwnd, w.width as u32, w.height as u32) {
                Ok((_tex, _srv)) => {
                    imported_count += 1;
                    "DCOMP-GPU OK"
                }
                Err(_) => "DCOMP_ERR",
            }
        } else {
            "NO_SURFACE"
        };

        let tile_rect = scene
            .clients
            .get(&w.hwnd)
            .map(|c| {
                format!(
                    "{:.0},{:.0} {}x{}",
                    c.target_rect.x, c.target_rect.y, c.target_rect.w, c.target_rect.h
                )
            })
            .unwrap_or_default();

        let title_short: String = w.title.chars().take(32).collect();
        println!(
            "0x{:08X}   {:<24} 0x{:012X}   {:<14} {:<22} {}",
            w.hwnd, w.exe_name, w.shared_surface_handle, import_status, tile_rect, title_short
        );
    }
    println!("{}", "-".repeat(115));
    println!(
        "Total Manageable Windows: {} | Zero-Copy GPU Textures Imported via OpenSharedResource: {}",
        windows.len(),
        imported_count
    );
    Ok(())
}

async fn run_hyprctl_ipc_server(state: Arc<RwLock<CompositorDaemonState>>) -> Result<()> {
    #[repr(C)]
    struct SecurityAttributes {
        n_length: u32,
        lp_security_descriptor: *mut core::ffi::c_void,
        b_inherit_handle: i32,
    }
    #[link(name = "advapi32")]
    extern "system" {
        fn InitializeSecurityDescriptor(
            psecuritydescriptor: *mut core::ffi::c_void,
            dwrevision: u32,
        ) -> i32;
        fn SetSecurityDescriptorDacl(
            psecuritydescriptor: *mut core::ffi::c_void,
            bdaclpresent: i32,
            pdacl: *const core::ffi::c_void,
            bdacldefaulted: i32,
        ) -> i32;
    }

    let mut is_first = true;
    loop {
        let server = unsafe {
            let mut sd_buf = [0u64; 8]; // 64-byte aligned buffer for SECURITY_DESCRIPTOR
            let sd_ptr = sd_buf.as_mut_ptr() as *mut core::ffi::c_void;
            let _ = InitializeSecurityDescriptor(sd_ptr, 1);
            let _ = SetSecurityDescriptorDacl(sd_ptr, 1, core::ptr::null(), 0);
            let mut sa = SecurityAttributes {
                n_length: std::mem::size_of::<SecurityAttributes>() as u32,
                lp_security_descriptor: sd_ptr,
                b_inherit_handle: 0,
            };
            ServerOptions::new()
                .first_pipe_instance(is_first)
                .create_with_security_attributes_raw(
                    HYPRLAND_WIN_IPC_PIPE,
                    &mut sa as *mut SecurityAttributes as *mut core::ffi::c_void,
                )?
        };
        is_first = false;

        server.connect().await?;
        let st = Arc::clone(&state);

        tokio::spawn(async move {
            let mut pipe = server;
            let mut buf = [0u8; 4096];
            if let Ok(n) = pipe.read(&mut buf).await {
                if n > 0 {
                    if let Ok(req) = serde_json::from_slice::<HyprIpcRequest>(&buf[..n]) {
                        let resp = handle_ipc_request(req, &st).await;
                        if let Ok(bytes) = serde_json::to_vec(&resp) {
                            let _ = pipe.write_all(&bytes).await;
                            let _ = pipe.flush().await;
                        }
                    }
                }
            }
        });
    }
}

async fn handle_ipc_request(
    req: HyprIpcRequest,
    state: &Arc<RwLock<CompositorDaemonState>>,
) -> HyprIpcResponse {
    let mut st = state.write().await;
    match req {
        HyprIpcRequest::Clients { json } => {
            let clients: Vec<_> = st.scene.clients.values().cloned().collect();
            if json {
                HyprIpcResponse {
                    ok: true,
                    payload: serde_json::to_string_pretty(&clients).unwrap_or_default(),
                }
            } else {
                let mut out = String::new();
                for c in clients {
                    out.push_str(&format!(
                        "Window {:x} -> {}:\n\tat: {:.0},{:.0}\n\tsize: {:.0},{:.0}\n\tworkspace: {}\n\tclass: {}\n\ttitle: {}\n\tsharedGpuHandle: 0x{:x}\n\n",
                        c.id, c.title, c.current_rect.x, c.current_rect.y, c.current_rect.w, c.current_rect.h, c.workspace_id, c.exe_name, c.title, c.shared_surface_handle
                    ));
                }
                HyprIpcResponse { ok: true, payload: out }
            }
        }
        HyprIpcRequest::Workspaces { json } => {
            let summaries = st.scene.workspace_summaries();
            if json {
                HyprIpcResponse {
                    ok: true,
                    payload: serde_json::to_string_pretty(&summaries).unwrap_or_default(),
                }
            } else {
                let mut out = String::new();
                for w in summaries {
                    out.push_str(&format!(
                        "workspace ID {} ({}) on monitor primary:\n\twindows: {}\n\thasfullscreen: {}\n\n",
                        w.id, w.name, w.windows, w.has_fullscreen
                    ));
                }
                HyprIpcResponse { ok: true, payload: out }
            }
        }
        HyprIpcRequest::ActiveWindow { json } => {
            let active = st.scene.clients.values().find(|c| c.focused).cloned();
            if json {
                HyprIpcResponse {
                    ok: true,
                    payload: serde_json::to_string_pretty(&active).unwrap_or_default(),
                }
            } else if let Some(c) = active {
                HyprIpcResponse {
                    ok: true,
                    payload: format!(
                        "Window {:x} -> {}\n\tclass: {}\n\tworkspace: {}\n\tsharedGpuHandle: 0x{:x}\n",
                        c.id, c.title, c.exe_name, c.workspace_id, c.shared_surface_handle
                    ),
                }
            } else {
                HyprIpcResponse {
                    ok: true,
                    payload: "Window none\n".to_string(),
                }
            }
        }
        HyprIpcRequest::Dispatch { dispatcher, arg } => {
            match dispatcher.as_str() {
                "workspace" => {
                    if let Ok(target_ws) = arg.parse::<u32>() {
                        let target_ws = target_ws.clamp(1, 10);
                        switch_to_workspace(&mut st, target_ws);
                        return HyprIpcResponse {
                            ok: true,
                            payload: format!("ok: switched to workspace {target_ws}"),
                        };
                    }
                }
                "togglefloating" => {
                    if let Some(id) = active_window_on_current_workspace(&st.scene) {
                        let cfg_clone = st.config.clone();
                        let _ = st.scene.toggle_floating(id, &cfg_clone);
                        st.last_applied_rects.remove(&id);
                        sync_active_workspace_geometries(&mut st);
                    }
                    return HyprIpcResponse {
                        ok: true,
                        payload: "ok".to_string(),
                    };
                }
                "fullscreen" => {
                    handle_input_action(InputAction::ToggleFullscreen, &mut st);
                    return HyprIpcResponse {
                        ok: true,
                        payload: "ok: toggled fullscreen".to_string(),
                    };
                }
                "layout" => {
                    st.config.layout = if arg.eq_ignore_ascii_case("master") {
                        LayoutMode::Master
                    } else {
                        LayoutMode::Dwindle
                    };
                    let ws = st.scene.active_workspace;
                    let cfg2 = st.config.clone();
                    st.scene.recalculate_layout(ws, &cfg2);
                    sync_active_workspace_geometries(&mut st);
                    return HyprIpcResponse {
                        ok: true,
                        payload: format!("ok: layout = {:?}", st.config.layout),
                    };
                }
                "tiling" => {
                    st.apply_tiling = !st.apply_tiling;
                    if st.apply_tiling {
                        st.last_applied_rects.clear();
                        sync_active_workspace_geometries(&mut st);
                    }
                    return HyprIpcResponse {
                        ok: true,
                        payload: format!("ok: active physical tiling = {}", st.apply_tiling),
                    };
                }
                "togglelayout" => {
                    st.config.layout = match st.config.layout {
                        LayoutMode::Dwindle => LayoutMode::Master,
                        LayoutMode::Master => LayoutMode::Dwindle,
                    };
                    let ws = st.scene.active_workspace;
                    let cfg2 = st.config.clone();
                    st.scene.recalculate_layout(ws, &cfg2);
                    sync_active_workspace_geometries(&mut st);
                    return HyprIpcResponse {
                        ok: true,
                        payload: format!("ok: layout = {:?}", st.config.layout),
                    };
                }
                "togglespecialworkspace" => {
                    handle_input_action(InputAction::ToggleSpecialWorkspace, &mut st);
                    return HyprIpcResponse {
                        ok: true,
                        payload: format!(
                            "ok: special_workspace_open = {}",
                            st.scene.special_workspace_open
                        ),
                    };
                }
                "togglegroup" => {
                    handle_input_action(InputAction::ToggleGroup, &mut st);
                    return HyprIpcResponse {
                        ok: true,
                        payload: "ok: toggled window group".to_string(),
                    };
                }
                "changegroupactive" => {
                    let forward = !arg.eq_ignore_ascii_case("b") && !arg.eq_ignore_ascii_case("prev");
                    handle_input_action(InputAction::ChangeGroupActive(forward), &mut st);
                    return HyprIpcResponse {
                        ok: true,
                        payload: "ok: cycled active group tab".to_string(),
                    };
                }
                "reloadbar" => {
                    {
                        use std::os::windows::process::CommandExt;
                        let _ = std::process::Command::new("taskkill.exe")
                            .args([
                                "/F",
                                "/IM",
                                "hyprbar.exe",
                                "/IM",
                                "hyprpaper.exe",
                                "/IM",
                                "hyprlauncher.exe",
                                "/IM",
                                "hyprnotify.exe",
                            ])
                            .creation_flags(0x0800_0000)
                            .status();
                    }
                    spawn_hyprbar_process();
                    return HyprIpcResponse {
                        ok: true,
                        payload: "ok: respawned hyprbar.exe, hyprpaper.exe, hyprlauncher.exe, and hyprnotify.exe"
                            .to_string(),
                    };
                }
                "exit" => {
                    let scene_clone = st.scene.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        emergency_shutdown_and_restore_explorer(&scene_clone);
                    });
                    return HyprIpcResponse {
                        ok: true,
                        payload: "ok: exiting Hyprland-Win and restoring explorer.exe".to_string(),
                    };
                }
                "exec" => {
                    spawn_unelevated_user_command(&arg);
                    return HyprIpcResponse {
                        ok: true,
                        payload: format!("ok: dispatched exec {arg}"),
                    };
                }
                _ => {}
            }
            HyprIpcResponse {
                ok: true,
                payload: format!("ok: dispatched {dispatcher} {arg}"),
            }
        }
        HyprIpcRequest::Keyword { key, value } => {
            let applied = st.config.apply_keyword(&key, &value);
            if applied {
                st.last_applied_rects.clear();
                let cfg_clone = st.config.clone();
                for ws in 1..=10 {
                    st.scene.recalculate_layout(ws, &cfg_clone);
                }
                sync_active_workspace_geometries(&mut st);
            }
            HyprIpcResponse {
                ok: applied,
                payload: if applied {
                    "ok".to_string()
                } else {
                    format!("unknown keyword '{key}'")
                },
            }
        }
        HyprIpcRequest::Reload => {
            check_and_reload_live_config(&mut st, true);
            HyprIpcResponse {
                ok: true,
                payload: format!(
                    "ok: reloaded hyprland.conf from {}",
                    st.config_path.display()
                ),
            }
        }
        HyprIpcRequest::Status { .. } => HyprIpcResponse {
            ok: true,
            payload: format!(
                "Hyprland-Win v0.1.0 (Wayland-NT + Hollowed DWM)\nActive Workspace: {}\nManaged Windows: {}\nLayout: {:?}\nPhysical Tiling Active: {}\n",
                st.scene.active_workspace,
                st.scene.clients.len(),
                st.config.layout,
                st.apply_tiling
            ),
        },
        HyprIpcRequest::BarState => {
            let mut occupied = [0u16; 10];
            let mut ws_hwnds: [Vec<u64>; 10] = Default::default();
            for ws in 1..=10 {
                let list = st
                    .scene
                    .workspace_order
                    .get(&ws)
                    .cloned()
                    .unwrap_or_default();
                occupied[(ws - 1) as usize] = list.len() as u16;
                ws_hwnds[(ws - 1) as usize] = list;
            }
            let active_id = active_window_on_current_workspace(&st.scene);
            let active_client = active_id.and_then(|id| st.scene.clients.get(&id));
            let active_ws_fullscreen = st
                .scene
                .clients
                .values()
                .any(|c| c.workspace_id == st.scene.active_workspace && c.fullscreen);
            let snap = HyprBarStateSnapshot {
                active_workspace: st.scene.active_workspace,
                occupied_workspaces: occupied,
                workspace_hwnds: ws_hwnds,
                layout_mode: match st.config.layout {
                    LayoutMode::Dwindle => "DWINDLE".to_string(),
                    LayoutMode::Master => "MASTER".to_string(),
                },
                tiling_active: st.apply_tiling,
                active_class: active_client
                    .map(|c| c.exe_name.clone())
                    .unwrap_or_default(),
                active_title: active_client
                    .map(|c| c.title.clone())
                    .unwrap_or_default(),
                active_floating: active_client.map(|c| c.floating).unwrap_or(false),
                active_fullscreen: active_ws_fullscreen,
            };
            HyprIpcResponse {
                ok: true,
                payload: serde_json::to_string(&snap).unwrap_or_default(),
            }
        }
    }
}

fn resolve_config_path() -> PathBuf {
    if let Ok(p) = std::env::var("HYPRLAND_WIN_CONFIG") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return pb;
        }
    }
    hyprland_win_config_file("hyprland.conf")
}

fn hash_file_contents(path: & std::path::Path) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    if let Ok(bytes) = std::fs::read(path) {
        let mut h = DefaultHasher::new();
        bytes.hash(&mut h);
        h.finish().max(1)
    } else {
        0
    }
}

fn check_and_reload_live_config(st: &mut CompositorDaemonState, force: bool) {
    let config_path = resolve_config_path();
    let mtime = std::fs::metadata(&config_path)
        .and_then(|m| m.modified())
        .ok();
    let cur_hash = hash_file_contents(&config_path);

    let mtime_changed = match (st.last_config_mtime, mtime) {
        (Some(prev), Some(now)) => now != prev,
        (None, Some(_)) => true,
        _ => false,
    };
    let hash_changed = cur_hash != 0 && cur_hash != st.last_config_hash;

    if !force && !mtime_changed && !hash_changed {
        return;
    }

    let new_cfg = HyprConfig::load_from_file(&config_path);
    update_live_keybinds(&new_cfg.binds);

    tracing::info!(
        "Live-reloaded hyprland.conf from {} ({} binds, gaps_in={}, gaps_out={}, border_size={}, rounding={})",
        config_path.display(),
        new_cfg.binds.len(),
        new_cfg.gaps_in,
        new_cfg.gaps_out,
        new_cfg.border_size,
        new_cfg.rounding
    );

    // Re-evaluate window rules on all currently open clients so live rule edits apply immediately!
    let mon_w = st.scene.monitor_width as f32;
    let mon_h = st.scene.monitor_height as f32;
    let top_m = st.scene.reserved_margins[0] as f32;
    let client_ids: Vec<u64> = st.scene.clients.keys().copied().collect();
    for id in client_ids {
        let Some((exe_name, class_name, title)) = st
            .scene
            .clients
            .get(&id)
            .map(|c| (c.exe_name.clone(), c.class_name.clone(), c.title.clone()))
        else {
            continue;
        };
        let rule_dec = new_cfg.evaluate_window_rules(&exe_name, &class_name, &title);
        if let Some(c) = st.scene.clients.get_mut(&id) {
            if let Some(fl) = rule_dec.float {
                c.floating = fl;
            }
            if let Some(fs) = rule_dec.fullscreen {
                c.fullscreen = fs;
            }
            if let Some((w, h)) = rule_dec.size {
                if c.floating {
                    c.current_rect.w = w;
                    c.current_rect.h = h;
                    c.target_rect.w = w;
                    c.target_rect.h = h;
                }
            }
            if rule_dec.center && c.floating {
                let nx = ((mon_w - c.current_rect.w) * 0.5).max(8.0);
                let ny = (top_m + (mon_h - top_m - c.current_rect.h) * 0.5).max(top_m + 4.0);
                let r = RectF::new(nx, ny, c.current_rect.w, c.current_rect.h);
                c.start_rect = r;
                c.current_rect = r;
                c.target_rect = r;
            }
        }
    }

    st.config_path = config_path;
    st.last_config_mtime = mtime;
    st.last_config_hash = cur_hash;
    st.config = new_cfg;
    st.last_applied_rects.clear();
    let cfg_clone = st.config.clone();
    for ws in 1..=10 {
        st.scene.recalculate_layout(ws, &cfg_clone);
    }
    sync_active_workspace_geometries(st);
}

fn is_current_process_elevated() -> bool {
    unsafe {
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::Security::{
            GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
        };
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut ret_len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

fn relaunch_self_as_admin(args: &[String]) -> bool {
    // 1. Try silent zero-prompt elevation via the pre-registered `Hyprland-WinBootLock` (RL HIGHEST) Scheduled Task
    {
        use std::os::windows::process::CommandExt;
        if let Ok(st) = std::process::Command::new("schtasks.exe")
            .args(["/Run", "/TN", "Hyprland-WinBootLock"])
            .creation_flags(0x0800_0000)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            if st.success() {
                return true;
            }
        }
    }

    // 2. Fallback to ShellExecuteW("runas")
    unsafe {
        use windows::core::PCWSTR;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        let mut joined_args: Vec<String> = args.iter().skip(1).cloned().collect();
        if !joined_args.iter().any(|a| a == "--no-elevate") {
            joined_args.push("--no-elevate".to_string());
        }
        let params_str = joined_args.join(" ");
        let cwd_str = std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let verb: Vec<u16> = "runas\0".encode_utf16().collect();
        let file: Vec<u16> = exe.to_string_lossy().encode_utf16().chain(Some(0)).collect();
        let params: Vec<u16> = params_str.encode_utf16().chain(Some(0)).collect();
        let dir: Vec<u16> = cwd_str.encode_utf16().chain(Some(0)).collect();

        let res = ShellExecuteW(
            HWND::default(),
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR(params.as_ptr()),
            PCWSTR(dir.as_ptr()),
            SW_SHOWNORMAL,
        );
        (res.0 as isize) > 32
    }
}

fn enable_debug_privilege() {
    unsafe {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID};
        use windows::Win32::Security::{
            AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES,
            SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
        };
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        let mut token = HANDLE::default();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .is_ok()
        {
            let priv_name: Vec<u16> = "SeDebugPrivilege\0".encode_utf16().collect();
            let mut luid = LUID::default();
            if LookupPrivilegeValueW(PCWSTR::null(), PCWSTR(priv_name.as_ptr()), &mut luid).is_ok()
            {
                let tp = TOKEN_PRIVILEGES {
                    PrivilegeCount: 1,
                    Privileges: [LUID_AND_ATTRIBUTES {
                        Luid: luid,
                        Attributes: SE_PRIVILEGE_ENABLED,
                    }],
                };
                let _ = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None);
            }
            let _ = CloseHandle(token);
        }
    }
}

/// Spawn a keybind command in the user's normal (Medium Integrity / non-Admin) security context
/// even when `hyprland-win.exe` is running elevated as Administrator.
fn spawn_unelevated_user_command(cmd_line: &str) {
    let trimmed = cmd_line.trim();
    if trimmed.is_empty() {
        return;
    }

    let (raw_exe, args_part) = if let Some(rest) = trimmed.strip_prefix('"') {
        if let Some(end_idx) = rest.find('"') {
            (rest[..end_idx].trim(), rest[end_idx + 1..].trim())
        } else {
            (rest.trim(), "")
        }
    } else if let Some(rest) = trimmed.strip_prefix('\'') {
        if let Some(end_idx) = rest.find('\'') {
            (rest[..end_idx].trim(), rest[end_idx + 1..].trim())
        } else {
            (rest.trim(), "")
        }
    } else if std::path::Path::new(trimmed).is_file() {
        (trimmed, "")
    } else if let Some((a, b)) = trimmed.split_once(' ') {
        (a.trim(), b.trim())
    } else {
        (trimmed, "")
    };

    let exe_normalized = if raw_exe.contains('/') && (raw_exe.contains(':') || std::path::Path::new(raw_exe).exists()) {
        raw_exe.replace('/', "\\")
    } else {
        raw_exe.to_string()
    };
    let exe_part = exe_normalized.as_str();

    // Resolve companion binaries (`hyprlauncher.exe`, `hyprshot.exe`, etc.) next to `hyprland-win.exe`,
    // and resolve bare `alacritty` / `alacritty.exe` to its standard installation path if present.
    let mut resolved_exe = if exe_part.eq_ignore_ascii_case("alacritty")
        || exe_part.eq_ignore_ascii_case("alacritty.exe")
    {
        let std_alacritty = r"C:\Program Files\Alacritty\alacritty.exe";
        if std::path::Path::new(std_alacritty).is_file() {
            std_alacritty.to_string()
        } else {
            "alacritty.exe".to_string()
        }
    } else if let Ok(cur_exe) = std::env::current_exe() {
        if let Some(dir) = cur_exe.parent() {
            let candidate = dir.join(exe_part);
            if candidate.exists() {
                candidate.to_string_lossy().to_string()
            } else {
                exe_part.to_string()
            }
        } else {
            exe_part.to_string()
        }
    } else {
        exe_part.to_string()
    };

    let exe_lower_check = resolved_exe.to_ascii_lowercase();
    let is_explorer = exe_lower_check == "explorer"
        || exe_lower_check == "explorer.exe"
        || exe_lower_check.ends_with("\\explorer.exe")
        || exe_lower_check.ends_with("/explorer.exe");
    let effective_args = if is_explorer {
        let sys_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        resolved_exe = format!("{sys_root}\\explorer.exe");
        if args_part.is_empty() {
            "/NoShellRegistrationCheck /e,::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".to_string()
        } else if !args_part
            .to_ascii_lowercase()
            .contains("/noshellregistrationcheck")
        {
            format!("/NoShellRegistrationCheck {args_part}")
        } else {
            args_part.to_string()
        }
    } else {
        args_part.to_string()
    };
    let args_part = effective_args.as_str();

    unsafe {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::Security::{
            DuplicateTokenEx, SecurityImpersonation, TokenPrimary, TOKEN_ALL_ACCESS,
        };
        use windows::Win32::System::Threading::{
            CreateProcessWithTokenW, OpenProcess, OpenProcessToken, CREATE_NO_WINDOW,
            PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
            STARTUPINFOW,
        };
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::{
            FindWindowW, GetWindowThreadProcessId, SW_SHOWNORMAL,
        };

        let lower = resolved_exe.to_ascii_lowercase();
        let is_background_companion = lower.contains("hyprlauncher")
            || lower.contains("hyprshot")
            || lower.contains("hyprlock")
            || lower.contains("hyprbar")
            || lower.contains("hyprpaper")
            || lower.contains("hyprnotify");

        if lower.ends_with("hyprlauncher.exe") || lower.ends_with("hyprlauncher") {
            let cls_u16: Vec<u16> = "HyprLauncherClass\0".encode_utf16().collect();
            if let Ok(hwnd) = FindWindowW(PCWSTR(cls_u16.as_ptr()), PCWSTR::null()) {
                if !hwnd.0.is_null() {
                    let msg = if args_part.contains("--clipboard") {
                        0x0400 + 43 // WM_LAUNCHER_CLIPBOARD (WM_USER + 43)
                    } else {
                        0x0400 + 42 // WM_LAUNCHER_TOGGLE (WM_USER + 42)
                    };
                    let _ = PostMessageW(hwnd, msg, WPARAM(0), LPARAM(0));
                    return;
                }
            }
        }
        if !is_background_companion && !lower.contains("taskmgr") {
            #[link(name = "kernel32")]
            extern "system" {
                fn K32EnumProcesses(lpidprocess: *mut u32, cb: u32, lpcbneeded: *mut u32) -> i32;
            }
            use windows::Win32::Security::{
                GetTokenInformation, TokenElevation, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE,
                TOKEN_ELEVATION, TOKEN_QUERY,
            };
            use windows::Win32::System::Threading::{
                QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
            };

            unsafe fn try_dup_unelevated(pid: u32) -> Option<HANDLE> {
                if pid <= 4 {
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
                    let mut elev = TOKEN_ELEVATION::default();
                    let mut ret_len = 0u32;
                    let is_elev = GetTokenInformation(
                        h_tok,
                        TokenElevation,
                        Some(&mut elev as *mut _ as *mut _),
                        std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                        &mut ret_len,
                    )
                    .is_ok()
                        && elev.TokenIsElevated != 0;
                    if !is_elev {
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

            let mut unelevated_tok: Option<HANDLE> = None;
            for cls in ["Progman\0", "CASCADIA_HOSTING_WINDOW_CLASS\0"] {
                let cls_u16: Vec<u16> = cls.encode_utf16().collect();
                if let Ok(sh_hwnd) = FindWindowW(PCWSTR(cls_u16.as_ptr()), PCWSTR::null()) {
                    if !sh_hwnd.0.is_null() {
                        let mut pid: u32 = 0;
                        GetWindowThreadProcessId(sh_hwnd, Some(&mut pid));
                        if let Some(h_dup) = try_dup_unelevated(pid) {
                            unelevated_tok = Some(h_dup);
                            break;
                        }
                    }
                }
            }
            if unelevated_tok.is_none() {
                let mut pids = [0u32; 1024];
                let mut bytes_returned = 0u32;
                if K32EnumProcesses(
                    pids.as_mut_ptr(),
                    std::mem::size_of_val(&pids) as u32,
                    &mut bytes_returned,
                ) != 0
                {
                    let count =
                        (bytes_returned as usize / std::mem::size_of::<u32>()).min(pids.len());
                    for &pid in &pids[..count] {
                        if let Ok(h_proc) =
                            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                        {
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
                                let base = full
                                    .rsplit(['\\', '/'])
                                    .next()
                                    .unwrap_or("")
                                    .to_ascii_lowercase();
                                if matches!(
                                    base.as_str(),
                                    "sihost.exe"
                                        | "ctfmon.exe"
                                        | "explorer.exe"
                                        | "runtimebroker.exe"
                                        | "taskhostw.exe"
                                        | "wt.exe"
                                ) {
                                    if let Some(h_dup) = try_dup_unelevated(pid) {
                                        unelevated_tok = Some(h_dup);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let user_work_dir_u16: Option<Vec<u16>> = if is_background_companion {
                std::path::Path::new(&resolved_exe)
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty() && p.is_dir())
                    .map(|p| p.to_string_lossy().encode_utf16().chain(Some(0)).collect())
            } else {
                std::env::var("USERPROFILE")
                    .ok()
                    .filter(|p| !p.is_empty() && std::path::Path::new(p).is_dir())
                    .map(|p| p.encode_utf16().chain(Some(0)).collect())
                    .or_else(|| {
                        std::path::Path::new(&resolved_exe)
                            .parent()
                            .filter(|p| !p.as_os_str().is_empty() && p.is_dir())
                            .map(|p| p.to_string_lossy().encode_utf16().chain(Some(0)).collect())
                    })
            };

            if let Some(h_dup) = unelevated_tok {
                let full_cmd = if args_part.is_empty() {
                    format!("\"{}\"", resolved_exe)
                } else {
                    format!("\"{}\" {}", resolved_exe, args_part)
                };
                let mut cmd_u16: Vec<u16> = full_cmd.encode_utf16().chain(Some(0)).collect();
                let work_dir_ptr = user_work_dir_u16
                    .as_ref()
                    .map(|v| PCWSTR(v.as_ptr()))
                    .unwrap_or(PCWSTR::null());

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
                    PROCESS_CREATION_FLAGS(0),
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

        if is_background_companion {
            use std::os::windows::process::CommandExt;
            let mut cmd = std::process::Command::new(&resolved_exe);
            if !args_part.is_empty() {
                for a in args_part.split_whitespace() {
                    cmd.arg(a);
                }
            }
            if cmd.creation_flags(0x0800_0000).spawn().is_ok() {
                return;
            }
        }

        let fallback_dir_u16: Option<Vec<u16>> = std::env::var("USERPROFILE")
            .ok()
            .filter(|p| !p.is_empty() && std::path::Path::new(p).is_dir())
            .map(|p| p.encode_utf16().chain(Some(0)).collect());
        let fallback_dir_ptr = fallback_dir_u16
            .as_ref()
            .map(|v| PCWSTR(v.as_ptr()))
            .unwrap_or(PCWSTR::null());

        let op: Vec<u16> = "open\0".encode_utf16().collect();
        let file: Vec<u16> = resolved_exe.encode_utf16().chain(Some(0)).collect();
        let params: Vec<u16> = args_part.encode_utf16().chain(Some(0)).collect();
        let params_ptr = if args_part.is_empty() {
            PCWSTR::null()
        } else {
            PCWSTR(params.as_ptr())
        };
        let _ = ShellExecuteW(
            HWND::default(),
            PCWSTR(op.as_ptr()),
            PCWSTR(file.as_ptr()),
            params_ptr,
            fallback_dir_ptr,
            SW_SHOWNORMAL,
        );
    }
}

fn is_any_media_playing() -> bool {
    // 1. Hardware CoreAudio Peak Meter check (catches ANY audible media: YouTube/browsers, Spotify, VLC, mpv, games)
    unsafe {
        use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
        use windows::Win32::Media::Audio::{
            eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator,
        };
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

        if let Ok(enumerator) =
            CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
        {
            if let Ok(device) = enumerator.GetDefaultAudioEndpoint(eRender, eConsole) {
                if let Ok(meter) = device.Activate::<IAudioMeterInformation>(CLSCTX_ALL, None) {
                    if let Ok(peak) = meter.GetPeakValue() {
                        if peak > 0.0005 {
                            return true;
                        }
                    }
                }
            }
        }
    }

    // 2. Windows GSMTC (GlobalSystemMediaTransportControlsSessionManager) playback status check
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSessionManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus,
    };
    if let Ok(mgr_op) = GlobalSystemMediaTransportControlsSessionManager::RequestAsync() {
        if let Ok(mgr) = mgr_op.get() {
            if let Ok(session) = mgr.GetCurrentSession() {
                if let Ok(info) = session.GetPlaybackInfo() {
                    if info.PlaybackStatus().ok()
                        == Some(GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing)
                    {
                        return true;
                    }
                }
            }
        }
    }

    false
}

fn spawn_hypridle_monitor() {
    let _ = std::thread::Builder::new()
        .name("hypridle-monitor".to_string())
        .spawn(|| {
            use windows::core::PCWSTR;
            use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
            use windows::Win32::System::Power::{
                SetThreadExecutionState, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
            };
            use windows::Win32::System::SystemInformation::GetTickCount;
            use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
            use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }

            let lock_cls: Vec<u16> = "HyprLockOverlayClass\0".encode_utf16().collect();
            let bar_cls: Vec<u16> = "HyprBarTopOverlay\0".encode_utf16().collect();
            let paper_cls: Vec<u16> = "HyprPaperBackdropClass\0".encode_utf16().collect();
            let launcher_cls: Vec<u16> = "HyprLauncherClass\0".encode_utf16().collect();
            let mut locked_for_current_idle = false;
            let mut last_active_tick = unsafe { GetTickCount() };

            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));

                unsafe {
                    // Self-healing watchdog: ensure `hyprbar.exe`, `hyprpaper.exe`, and `hyprlauncher.exe` are always alive alongside `hyprland-win`
                    let bar_alive = FindWindowW(PCWSTR(bar_cls.as_ptr()), PCWSTR::null())
                        .map(|h| !h.0.is_null())
                        .unwrap_or(false);
                    let paper_alive = FindWindowW(PCWSTR(paper_cls.as_ptr()), PCWSTR::null())
                        .map(|h| !h.0.is_null())
                        .unwrap_or(false);
                    let launcher_alive = FindWindowW(PCWSTR(launcher_cls.as_ptr()), PCWSTR::null())
                        .map(|h| !h.0.is_null())
                        .unwrap_or(false);
                    if !bar_alive || !paper_alive || !launcher_alive {
                        use std::os::windows::process::CommandExt;
                        if let Ok(cur_exe) = std::env::current_exe() {
                            if let Some(dir) = cur_exe.parent() {
                                if !bar_alive {
                                    let p = dir.join("hyprbar.exe");
                                    if p.exists() {
                                        let _ = std::process::Command::new(p)
                                            .creation_flags(0x0800_0000)
                                            .spawn();
                                    }
                                }
                                if !paper_alive {
                                    let p = dir.join("hyprpaper.exe");
                                    if p.exists() {
                                        let _ = std::process::Command::new(p)
                                            .creation_flags(0x0800_0000)
                                            .spawn();
                                    }
                                }
                                if !launcher_alive {
                                    let p = dir.join("hyprlauncher.exe");
                                    if p.exists() {
                                        let _ = std::process::Command::new(p)
                                            .creation_flags(0x0800_0000)
                                            .spawn();
                                    }
                                }
                            }
                        }
                    }

                    let now_tick = GetTickCount();

                    // 1. Respect `hyprbar` Idle Inhibitor coffee toggle (`%TEMP%\hyprland_win_idle_inhibit.flag`)
                    if std::env::temp_dir()
                        .join("hyprland_win_idle_inhibit.flag")
                        .is_file()
                    {
                        last_active_tick = now_tick;
                        locked_for_current_idle = false;
                        continue;
                    }

                    // 2. If any media/audio is actively playing, do NOT count as idle (reset idle timer to 0)
                    if is_any_media_playing() {
                        last_active_tick = now_tick;
                        locked_for_current_idle = false;
                        let _ =
                            SetThreadExecutionState(ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED);
                        continue;
                    }

                    // 3. Check physical keyboard/mouse input timestamp
                    let mut lii = LASTINPUTINFO {
                        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
                        dwTime: 0,
                    };
                    if GetLastInputInfo(&mut lii).as_bool() {
                        let raw_input_idle_ms = now_tick.wrapping_sub(lii.dwTime);
                        if raw_input_idle_ms < 5_000 {
                            last_active_tick = now_tick;
                            locked_for_current_idle = false;
                        }

                        // Effective idle time counts only from the most recent input OR media playback!
                        let effective_idle_ms = now_tick.wrapping_sub(last_active_tick);
                        if effective_idle_ms >= 300_000 && !locked_for_current_idle {
                            // 5 minutes of continuous non-media idle -> launch `hyprlock.exe` if not already open
                            let already_locked =
                                FindWindowW(PCWSTR(lock_cls.as_ptr()), PCWSTR::null())
                                    .map(|h| !h.0.is_null())
                                    .unwrap_or(false);
                            if !already_locked {
                                locked_for_current_idle = true;
                                spawn_unelevated_user_command("hyprlock.exe");
                            }
                        }
                    }
                }
            }
        });
}
