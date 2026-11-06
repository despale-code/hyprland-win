//! `Win32Wayland` Bridge — discovers top-level Win32 `HWND`s, extracts their
//! zero-copy `DwmGetDxSharedSurface` GPU texture handles (or DirectComposition
//! `WS_EX_NOREDIRECTIONBITMAP` visual trees), detects fullscreen Direct Scanout
//! games (CS2, Deadlock, Wuthering Waves), and wraps them as `wayland-nt`
//! `xdg_toplevel` surfaces.

use serde::{Deserialize, Serialize};
use std::path::Path;
use windows::Win32::Foundation::{BOOL, CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetForegroundWindow, GetSystemMetrics, GetWindowLongW,
    GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    SetWindowPos, GWL_EXSTYLE, GWL_STYLE, SM_CXSCREEN, SM_CYSCREEN, SWP_ASYNCWINDOWPOS,
    SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, WS_EX_APPWINDOW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::shared_surface::{query_window_shared_texture, SharedWindowTexture, SurfaceCaptureKind};
use wayland_nt::{SurfaceBuffer, SurfaceRole, WlSurfaceState};

/// Snapshot of a top-level Win32 window bridged into the Wayland compositor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgedWin32Window {
    pub hwnd: u64,
    pub pid: u32,
    pub title: String,
    pub class_name: String,
    pub exe_name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub is_focused: bool,
    pub is_fullscreen_candidate: bool,
    pub shared_surface_handle: u64,
    pub is_dcomp_visual: bool,
    pub dxgi_format: u32,
    pub win32k_update_id: u64,
}

impl BridgedWin32Window {
    /// Convert this bridged Win32 `HWND` into a standard `wayland-nt` `WlSurfaceState`
    /// (`xdg_toplevel` backed by `SurfaceBuffer::DxgiShared`).
    pub fn to_wayland_surface(&self) -> WlSurfaceState {
        WlSurfaceState {
            client_id: 0, // Reserved client_id 0 = Win32Wayland kernel bridge
            surface_id: (self.hwnd & 0xFFFF_FFFF) as u32,
            role: SurfaceRole::XdgToplevel {
                title: self.title.clone(),
                app_id: self.exe_name.clone(),
                fullscreen: self.is_fullscreen_candidate,
                maximized: false,
                floating: false,
            },
            buffer: if self.shared_surface_handle != 0 {
                Some(SurfaceBuffer::DxgiShared {
                    shared_handle: self.shared_surface_handle,
                    width: self.width.max(1) as u32,
                    height: self.height.max(1) as u32,
                    format: self.dxgi_format,
                })
            } else {
                None
            },
            damage: Vec::new(),
            buffer_scale: 1,
            frame_callback_id: None,
        }
    }

    /// Returns `true` if this window should bypass compositor shaders and engage hardware
    /// **Independent Flip (Direct Scanout)** (`0.0ms` added frame latency + 100% kernel anti-cheat safe).
    pub fn should_direct_scanout(&self, monitor_width: i32, monitor_height: i32) -> bool {
        let covers_monitor = self.width >= monitor_width && self.height >= monitor_height;
        let is_known_game = matches!(
            self.exe_name.to_ascii_lowercase().as_str(),
            "cs2.exe"
                | "project8.exe" // Deadlock
                | "deadlock.exe"
                | "client-win64-shipping.exe" // Wuthering Waves (UE4 shipping binary)
                | "wuthering waves.exe"
                | "valorant-win64-shipping.exe"
        );
        self.is_focused && (covers_monitor || (is_known_game && self.is_fullscreen_candidate))
    }
}

/// Enumerate all manageable top-level application `HWND`s on the current desktop and
/// query their zero-copy `DwmGetDxSharedSurface` GPU texture handles.
pub fn enumerate_manageable_windows(compositor_hwnd: Option<u64>) -> Vec<BridgedWin32Window> {
    let mut list: Vec<BridgedWin32Window> = Vec::with_capacity(32);
    let fg_hwnd = unsafe { GetForegroundWindow() };

    struct EnumCtx {
        fg_hwnd: HWND,
        compositor_hwnd: Option<u64>,
        out: Vec<BridgedWin32Window>,
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut EnumCtx);
        if Some(hwnd.0 as u64) == ctx.compositor_hwnd {
            return BOOL(1);
        }

        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return BOOL(1);
        }

        // Skip windows cloaked by the shell (e.g. UWP background ghosts, TextInputHost)
        let mut cloaked: u32 = 0;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            return BOOL(1);
        }

        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;

        let is_app_window = (ex_style & WS_EX_APPWINDOW.0) != 0;
        let is_tool_window = (ex_style & WS_EX_TOOLWINDOW.0) != 0;
        let is_noactivate = (ex_style & WS_EX_NOACTIVATE.0) != 0;

        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return BOOL(1);
        }
        let mut width = rect.right - rect.left;
        let mut height = rect.bottom - rect.top;

        let mut class_buf = [0u16; 128];
        let class_len = GetClassNameW(hwnd, &mut class_buf);
        let class_name = String::from_utf16_lossy(&class_buf[..class_len as usize]);
        let is_open_with_class =
            class_name.starts_with("Open With") || class_name.contains("OpenWith");

        if !is_open_with_class && (width < 64 || height < 64) {
            return BOOL(1);
        }
        if width < 16 || height < 12 {
            return BOOL(1);
        }

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let exe_name = query_process_exe_name(pid).unwrap_or_else(|| class_name.clone());
        let exe_lower = exe_name.to_ascii_lowercase();
        let is_openwith = is_open_with_class || exe_lower == "openwith.exe";
        let is_openwith_or_dialog = is_openwith
            || exe_lower == "rundll32.exe"
            || (exe_lower == "dllhost.exe" && width < 900 && height < 900)
            || class_name == "#32770"
            || class_name == "Shell_Flyout"
            || class_name == "PopupWindowSiteBridge";

        // When explorer.exe is suspended/killed, Windows' `OpenWith.exe` ("How do you want to open this file?")
        // and shell flyout dialogs lose their shell placement anchor and spawn near `(0, 0)` or `(-w/2, -h/2)`.
        // Detect any such popup stranded near the top-left edge and immediately center it on the monitor!
        if is_openwith_or_dialog && (rect.left <= 160 && rect.top <= 160 || (is_openwith && height < 240)) {
            let mon_w = GetSystemMetrics(SM_CXSCREEN).max(1920);
            let mon_h = GetSystemMetrics(SM_CYSCREEN).max(1080);
            if is_openwith {
                let target_w = width.max(448);
                let target_h = if height < 240 { 540 } else { height };
                let cx = (mon_w - target_w) / 2;
                let cy = (mon_h - target_h) / 2;
                let _ = SetWindowPos(
                    hwnd,
                    HWND::default(),
                    cx,
                    cy,
                    target_w,
                    target_h,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                // Also expand the child DesktopChildSiteBridge XAML island so its content fills the dialog
                use windows::Win32::UI::WindowsAndMessaging::FindWindowExW;
                let child_cls: Vec<u16> = "Microsoft.UI.Content.DesktopChildSiteBridge\0"
                    .encode_utf16()
                    .collect();
                if let Ok(child_hwnd) = FindWindowExW(
                    hwnd,
                    HWND::default(),
                    windows::core::PCWSTR(child_cls.as_ptr()),
                    windows::core::PCWSTR::null(),
                ) {
                    if !child_hwnd.0.is_null() {
                        let _ = SetWindowPos(
                            child_hwnd,
                            HWND::default(),
                            0,
                            0,
                            (target_w - 16).max(432),
                            (target_h - 45).max(480),
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                }
                width = target_w;
                height = target_h;
                rect.left = cx;
                rect.top = cy;
                rect.right = cx + width;
                rect.bottom = cy + height;
            } else if width < mon_w - 80 && height < mon_h - 80 {
                let cx = (mon_w - width) / 2;
                let cy = (mon_h - height) / 2;
                let _ = SetWindowPos(
                    hwnd,
                    HWND::default(),
                    cx,
                    cy,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
                );
                rect.left = cx;
                rect.top = cy;
                rect.right = cx + width;
                rect.bottom = cy + height;
            }
        }

        if is_openwith {
            return BOOL(1);
        }

        if (is_tool_window || is_noactivate) && !is_app_window && exe_lower != "openwith.exe" {
            return BOOL(1);
        }

        let mut title_buf = [0u16; 256];
        let title_len = GetWindowTextW(hwnd, &mut title_buf);
        if title_len == 0 && exe_lower != "openwith.exe" {
            return BOOL(1);
        }
        let title = if title_len > 0 {
            String::from_utf16_lossy(&title_buf[..title_len as usize])
        } else {
            "Open With".to_string()
        };

        // Filter out desktop wallpaper/shell containers and Hyprland-Win's own status bar / overlays
        if exe_lower != "openwith.exe"
            && matches!(
                class_name.as_str(),
                "Progman"
                    | "WorkerW"
                    | "Shell_TrayWnd"
                    | "Shell_SecondaryTrayWnd"
                    | "Windows.UI.Core.CoreWindow"
                    | "XamlExplorerHostIslandWindow"
                    | "HyprBarTopOverlay"
                    | "HyprBarCalendarPopup"
                    | "HyprBarTooltipPopup"
                    | "HyprPaperBackdropClass"
                    | "HyprlandWinDCompOverlayClass"
                    | "HyprlandWinNativeDCompOverlay"
                    | "HyprLauncherClass"
                    | "HyprNotifyToastWindow"
                    | "HyprNotifyPopupClass"
                    | "HyprShotOverlayClass"
                    | "HyprLockOverlayClass"
            )
        {
            return BOOL(1);
        }

        let shared_tex: Option<SharedWindowTexture> = query_window_shared_texture(hwnd).ok();
        let is_popup = (style & WS_POPUP.0) != 0;
        let is_dcomp_visual = shared_tex
            .map(|t| t.kind == SurfaceCaptureKind::DirectCompositionVisual)
            .unwrap_or(false);

        ctx.out.push(BridgedWin32Window {
            hwnd: hwnd.0 as u64,
            pid,
            title,
            class_name,
            exe_name,
            x: rect.left,
            y: rect.top,
            width,
            height,
            is_focused: hwnd == ctx.fg_hwnd,
            is_fullscreen_candidate: is_popup && width >= 1280 && height >= 720,
            shared_surface_handle: shared_tex.map(|t| t.surface_handle).unwrap_or(0),
            is_dcomp_visual,
            dxgi_format: shared_tex.map(|t| t.dxgi_format).unwrap_or(87),
            win32k_update_id: shared_tex.map(|t| t.win32k_update_id).unwrap_or(0),
        });

        BOOL(1)
    }

    let mut ctx = EnumCtx {
        fg_hwnd,
        compositor_hwnd,
        out: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut EnumCtx as isize));
    }
    list.append(&mut ctx.out);
    list
}

fn query_process_exe_name(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    unsafe {
        let h_proc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            h_proc,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(h_proc);
        ok.ok()?;
        let full_path = String::from_utf16_lossy(&buf[..len as usize]);
        Path::new(&full_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
    }
}
