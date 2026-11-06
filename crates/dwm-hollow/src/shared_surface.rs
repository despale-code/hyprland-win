//! Zero-copy DXGI Shared Texture extraction from `win32k` / `dwmcore` via `DwmGetDxSharedSurface`,
//! with child-HWND swapchain discovery and DirectComposition (`WS_EX_NOREDIRECTIONBITMAP`) support.

use anyhow::{bail, Result};
use std::sync::OnceLock;
use windows::core::s;
use windows::Win32::Foundation::{BOOL, HANDLE, HWND, LPARAM, LUID};
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetWindowLongW, GWL_EXSTYLE, WS_EX_NOREDIRECTIONBITMAP,
};

/// Function signature of `user32!DwmGetDxSharedSurface`.
type DwmGetDxSharedSurfaceFn = unsafe extern "system" fn(
    hwnd: HWND,
    ph_surface: *mut HANDLE,
    p_adapter_luid: *mut LUID,
    p_fmt_window: *mut u32,
    p_present_flags: *mut u32,
    p_win32k_update_id: *mut u64,
) -> i32; // BOOL

static DWM_GET_DX_SHARED_SURFACE: OnceLock<Option<DwmGetDxSharedSurfaceFn>> = OnceLock::new();

fn resolve_dwm_get_dx_shared_surface() -> Option<DwmGetDxSharedSurfaceFn> {
    *DWM_GET_DX_SHARED_SURFACE.get_or_init(|| unsafe {
        let user32 = GetModuleHandleA(s!("user32.dll")).ok()?;
        let proc = GetProcAddress(user32, s!("DwmGetDxSharedSurface"))?;
        Some(std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            DwmGetDxSharedSurfaceFn,
        >(proc))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceCaptureKind {
    /// Direct top-level `win32k` shared DXGI surface (`DwmGetDxSharedSurface` on parent `HWND`).
    TopLevelShared,
    /// Shared DXGI surface attached to a child swapchain `HWND` (e.g. `Intermediate D3D Window`).
    ChildSwapchainShared,
    /// Window uses `WS_EX_NOREDIRECTIONBITMAP` (DirectComposition visual tree such as
    /// `WindowsTerminal.exe` or Electron/Chromium `IDCompositionVisual`), captured via
    /// `dwmcore` DirectComposition render target.
    DirectCompositionVisual,
}

/// Metadata and zero-copy GPU shared handle for a redirected Win32 `HWND`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedWindowTexture {
    /// Shared DXGI / D3D11 resource `HANDLE` (`ID3D11Device::OpenSharedResource`).
    pub surface_handle: u64,
    /// GPU adapter LUID owning the allocation.
    pub adapter_luid: u64,
    /// `DXGI_FORMAT` of the window's redirection surface (typically `DXGI_FORMAT_B8G8R8A8_UNORM = 87`).
    pub dxgi_format: u32,
    /// Present flags reported by `win32k`.
    pub present_flags: u32,
    /// Monotonic frame update counter maintained by `win32k`.
    pub win32k_update_id: u64,
    /// How this window's GPU surface was acquired.
    pub kind: SurfaceCaptureKind,
}

fn query_single_hwnd_shared_texture(
    func: DwmGetDxSharedSurfaceFn,
    hwnd: HWND,
    kind: SurfaceCaptureKind,
) -> Option<SharedWindowTexture> {
    let mut h_surface = HANDLE::default();
    let mut adapter_luid = LUID::default();
    let mut fmt_window: u32 = 0;
    let mut present_flags: u32 = 0;
    let mut update_id: u64 = 0;

    let ok = unsafe {
        func(
            hwnd,
            &mut h_surface,
            &mut adapter_luid,
            &mut fmt_window,
            &mut present_flags,
            &mut update_id,
        )
    };

    if ok == 0 || h_surface.is_invalid() || h_surface.0.is_null() {
        return None;
    }

    let luid_u64 = ((adapter_luid.HighPart as u64) << 32) | (adapter_luid.LowPart as u32 as u64);
    Some(SharedWindowTexture {
        surface_handle: h_surface.0 as u64,
        adapter_luid: luid_u64,
        dxgi_format: fmt_window,
        present_flags,
        win32k_update_id: update_id,
        kind,
    })
}

/// Query `win32k` for the zero-copy shared GPU texture backing `hwnd`.
///
/// 1. First queries the top-level `hwnd` via `DwmGetDxSharedSurface`.
/// 2. If the top-level `hwnd` uses `WS_EX_NOREDIRECTIONBITMAP` (e.g. `WindowsTerminal.exe`,
///    `Antigravity.exe` / Electron / WinUI 3), scans its child `HWND`s (`Intermediate D3D Window`,
///    `DesktopWindowContentBridge`, etc.) for a bound DXGI swapchain surface.
/// 3. If the window uses pure windowless DirectComposition visuals, marks it as
///    `SurfaceCaptureKind::DirectCompositionVisual` for `dwmcore` GPU target capture.
pub fn query_window_shared_texture(hwnd: HWND) -> Result<SharedWindowTexture> {
    let Some(func) = resolve_dwm_get_dx_shared_surface() else {
        bail!("user32.dll!DwmGetDxSharedSurface export not found on this OS");
    };

    // 1. Try top-level HWND directly
    if let Some(tex) = query_single_hwnd_shared_texture(func, hwnd, SurfaceCaptureKind::TopLevelShared) {
        return Ok(tex);
    }

    // 2. Scan child HWNDs (Chromium/Electron `Intermediate D3D Window` or WinUI XAML Islands)
    struct ChildSearch {
        func: DwmGetDxSharedSurfaceFn,
        found: Option<SharedWindowTexture>,
    }

    unsafe extern "system" fn enum_child_proc(child_hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = &mut *(lparam.0 as *mut ChildSearch);
        if let Some(tex) = query_single_hwnd_shared_texture(
            search.func,
            child_hwnd,
            SurfaceCaptureKind::ChildSwapchainShared,
        ) {
            search.found = Some(tex);
            return BOOL(0); // Stop enumeration, found the DXGI child surface!
        }
        BOOL(1)
    }

    let mut search = ChildSearch { func, found: None };
    unsafe {
        let _ = EnumChildWindows(
            hwnd,
            Some(enum_child_proc),
            LPARAM(&mut search as *mut ChildSearch as isize),
        );
    }
    if let Some(tex) = search.found {
        return Ok(tex);
    }

    // 3. Check if the window explicitly opted into WS_EX_NOREDIRECTIONBITMAP (DirectComposition)
    let ex_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 };
    if (ex_style & WS_EX_NOREDIRECTIONBITMAP.0) != 0 {
        return Ok(SharedWindowTexture {
            surface_handle: 0,
            adapter_luid: 0,
            dxgi_format: 87, // DXGI_FORMAT_B8G8R8A8_UNORM
            present_flags: 0,
            win32k_update_id: 1,
            kind: SurfaceCaptureKind::DirectCompositionVisual,
        });
    }

    bail!("No shared surface or DirectComposition visual tree found for HWND {:?}", hwnd)
}
