#![windows_subsystem = "windows"]
//! `hyprshot` — Native Screenshot Utility (`region` / `window` / `output`) for `Hyprland-Win`.
//!
//! Usage:
//! - `hyprshot -m region`   Interactive Liquid Glass dim-and-crosshair region selector (`SUPER + SHIFT + S` / `Print`).
//! - `hyprshot -m window`   Captures the currently focused tiled/floating window (`SUPER + Print`).
//! - `hyprshot -m output`   Captures the full primary monitor immediately (`SHIFT + Print`).
//! - `--clipboard-only`     Copies to Windows clipboard (`CF_BITMAP`) without saving to disk.
//! - `-o <dir>`             Output directory (defaults to `%USERPROFILE%\Pictures\Screenshots`).

mod ui;

use anyhow::Result;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection, DeleteDC,
    DeleteObject, EndPaint, GetDC, InvalidateRect, ReleaseDC, SelectObject, UpdateWindow,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, PAINTSTRUCT, SRCCOPY,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, CreateWindowExW, DefWindowProcW, DispatchMessageW,
    GetForegroundWindow, GetMessageW, GetSystemMetrics, GetWindowRect, LoadCursorW,
    PostQuitMessage, RegisterClassW, SetForegroundWindow, SetWindowPos, ShowWindow,
    TranslateMessage, ASFW_ANY, CS_HREDRAW, CS_VREDRAW, HWND_TOPMOST, IDC_CROSS, MSG, SM_CXSCREEN,
    SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SWP_SHOWWINDOW, SW_SHOWNORMAL, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONDOWN, WNDCLASSW, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

pub use ui::ShotOverlayState;

const CF_BITMAP_VAL: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShotMode {
    Region,
    Window,
    Output,
}

static OVERLAY_STATE: OnceLock<Mutex<ShotOverlayState>> = OnceLock::new();

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
    }

    let args: Vec<String> = std::env::args().collect();
    let mut mode = ShotMode::Region;
    let mut clipboard_only = false;
    let mut out_dir_opt: Option<PathBuf> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-m" | "--mode" => {
                if let Some(m) = args.get(i + 1) {
                    mode = match m.to_ascii_lowercase().as_str() {
                        "window" | "active" => ShotMode::Window,
                        "output" | "screen" | "full" => ShotMode::Output,
                        _ => ShotMode::Region,
                    };
                    i += 1;
                }
            }
            "--region" => {
                mode = ShotMode::Region;
            }
            "--window" => {
                mode = ShotMode::Window;
            }
            "--full" | "--output" => {
                mode = ShotMode::Output;
            }
            "--clipboard-only" | "-c" => {
                clipboard_only = true;
            }
            "-o" | "--output-folder" => {
                if let Some(p) = args.get(i + 1) {
                    out_dir_opt = Some(PathBuf::from(p));
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }

    let virt_x = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let virt_y = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let screen_w = unsafe {
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        if vw > 0 { vw } else { GetSystemMetrics(SM_CXSCREEN).max(1280) }
    };
    let screen_h = unsafe {
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if vh > 0 { vh } else { GetSystemMetrics(SM_CYSCREEN).max(720) }
    };

    // Capture full virtual desktop BGRA pixels once before showing any overlay
    let screen_pixels = unsafe { capture_screen_bgra(virt_x, virt_y, screen_w, screen_h) };

    match mode {
        ShotMode::Output => {
            finalize_capture(
                &screen_pixels,
                screen_w,
                screen_h,
                0,
                0,
                screen_w,
                screen_h,
                clipboard_only,
                out_dir_opt.as_deref(),
            )?;
        }
        ShotMode::Window => {
            let (wx, wy, ww, wh) =
                unsafe { query_focused_window_crop(virt_x, virt_y, screen_w, screen_h) };
            finalize_capture(
                &screen_pixels,
                screen_w,
                screen_h,
                wx,
                wy,
                ww,
                wh,
                clipboard_only,
                out_dir_opt.as_deref(),
            )?;
        }
        ShotMode::Region => {
            if let Some((rx, ry, rw, rh)) = unsafe {
                run_region_selector_overlay(virt_x, virt_y, screen_w, screen_h, screen_pixels.clone())?
            } {
                if rw >= 4 && rh >= 4 {
                    finalize_capture(
                        &screen_pixels,
                        screen_w,
                        screen_h,
                        rx,
                        ry,
                        rw,
                        rh,
                        clipboard_only,
                        out_dir_opt.as_deref(),
                    )?;
                }
            }
        }
    }

    Ok(())
}

unsafe fn capture_screen_bgra(x: i32, y: i32, w: i32, h: i32) -> Vec<u32> {
    let w = w.max(1);
    let h = h.max(1);
    let mut out = vec![0xFF16_171Bu32; (w * h) as usize];

    let screen_dc = GetDC(HWND::default());
    if screen_dc.is_invalid() {
        return out;
    }
    let mem_dc = CreateCompatibleDC(screen_dc);
    if mem_dc.is_invalid() {
        let _ = ReleaseDC(HWND::default(), screen_dc);
        return out;
    }

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
    if let Ok(dib) = CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
        if !dib.is_invalid() && !bits.is_null() {
            let old = SelectObject(mem_dc, dib);
            let _ = BitBlt(mem_dc, 0, 0, w, h, screen_dc, x, y, SRCCOPY);
            let slice = std::slice::from_raw_parts(bits as *const u32, (w * h) as usize);
            out.copy_from_slice(slice);
            SelectObject(mem_dc, old);
            let _ = DeleteObject(dib);
        }
    }

    let _ = DeleteDC(mem_dc);
    let _ = ReleaseDC(HWND::default(), screen_dc);
    out
}

unsafe fn query_focused_window_crop(
    virt_x: i32,
    virt_y: i32,
    screen_w: i32,
    screen_h: i32,
) -> (i32, i32, i32, i32) {
    let fg = GetForegroundWindow();
    if !fg.0.is_null() {
        let mut rc = RECT::default();
        if GetWindowRect(fg, &mut rc).is_ok() {
            let rel_l = (rc.left - virt_x).clamp(0, screen_w - 1);
            let rel_t = (rc.top - virt_y).clamp(0, screen_h - 1);
            let rel_r = (rc.right - virt_x).clamp(rel_l + 1, screen_w);
            let rel_b = (rc.bottom - virt_y).clamp(rel_t + 1, screen_h);
            if rel_r - rel_l >= 16 && rel_b - rel_t >= 16 {
                return (rel_l, rel_t, rel_r - rel_l, rel_b - rel_t);
            }
        }
    }
    (0, 0, screen_w, screen_h)
}

unsafe fn run_region_selector_overlay(
    virt_x: i32,
    virt_y: i32,
    screen_w: i32,
    screen_h: i32,
    screen_pixels: Vec<u32>,
) -> Result<Option<(i32, i32, i32, i32)>> {
    let st = ShotOverlayState::new(screen_w, screen_h, screen_pixels);
    let _ = OVERLAY_STATE.set(Mutex::new(st));

    let instance = GetModuleHandleW(None)?;
    let class_name: Vec<u16> = "HyprShotOverlayClass\0".encode_utf16().collect();
    let title_name: Vec<u16> = "HyprShot\0".encode_utf16().collect();

    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(shot_wnd_proc),
        hInstance: instance.into(),
        hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    RegisterClassW(&wc);

    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        PCWSTR(class_name.as_ptr()),
        PCWSTR(title_name.as_ptr()),
        WS_POPUP | WS_VISIBLE,
        virt_x,
        virt_y,
        screen_w,
        screen_h,
        HWND::default(),
        None,
        instance,
        None,
    )?;

    let _ = AllowSetForegroundWindow(ASFW_ANY);
    let _ = SetWindowPos(
        hwnd,
        HWND_TOPMOST,
        virt_x,
        virt_y,
        screen_w,
        screen_h,
        SWP_SHOWWINDOW,
    );
    let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
    let _ = BringWindowToTop(hwnd);
    let _ = SetForegroundWindow(hwnd);
    let _ = SetFocus(hwnd);
    let _ = InvalidateRect(hwnd, None, false);
    let _ = UpdateWindow(hwnd);

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    let result = OVERLAY_STATE
        .get()
        .and_then(|m| m.lock().ok())
        .and_then(|s| s.selected_crop());
    Ok(result)
}

unsafe extern "system" fn shot_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_KEYDOWN => {
            if wparam.0 as u16 == VK_ESCAPE.0 {
                if let Some(m) = OVERLAY_STATE.get() {
                    if let Ok(mut s) = m.lock() {
                        s.cancelled = true;
                    }
                }
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            if let Some(m) = OVERLAY_STATE.get() {
                if let Ok(mut s) = m.lock() {
                    s.cancelled = true;
                }
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let _ = SetCapture(hwnd);
            if let Some(m) = OVERLAY_STATE.get() {
                if let Ok(mut s) = m.lock() {
                    s.dragging = true;
                    s.start_pt = (x, y);
                    s.cur_pt = (x, y);
                }
            }
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            if let Some(m) = OVERLAY_STATE.get() {
                if let Ok(mut s) = m.lock() {
                    s.hover_pt = (x, y);
                    if s.dragging {
                        s.cur_pt = (x, y);
                    }
                }
            }
            let _ = InvalidateRect(hwnd, None, false);
            let _ = UpdateWindow(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let _ = ReleaseCapture();
            if let Some(m) = OVERLAY_STATE.get() {
                if let Ok(mut s) = m.lock() {
                    s.dragging = false;
                    s.cur_pt = (x, y);
                    s.finished = true;
                }
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            if let Some(m) = OVERLAY_STATE.get() {
                if let Ok(s) = m.lock() {
                    ui::render_shot_overlay(hdc, &s);
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

fn finalize_capture(
    full_pixels: &[u32],
    screen_w: i32,
    screen_h: i32,
    cx: i32,
    cy: i32,
    cw: i32,
    ch: i32,
    clipboard_only: bool,
    custom_dir: Option<&Path>,
) -> Result<()> {
    let x0 = cx.clamp(0, screen_w - 1);
    let y0 = cy.clamp(0, screen_h - 1);
    let w = cw.clamp(1, screen_w - x0);
    let h = ch.clamp(1, screen_h - y0);

    let mut cropped = vec![0u32; (w * h) as usize];
    for row in 0..h {
        let src_start = ((y0 + row) as usize) * (screen_w as usize) + (x0 as usize);
        let dst_start = (row as usize) * (w as usize);
        cropped[dst_start..dst_start + (w as usize)]
            .copy_from_slice(&full_pixels[src_start..src_start + (w as usize)]);
    }

    // 1. Copy to Windows Clipboard (`CF_BITMAP`)
    unsafe {
        copy_bgra_to_clipboard(&cropped, w, h);
    }

    // 2. Save to disk unless `--clipboard-only`
    let mut saved_path_str = String::from("Clipboard");
    if !clipboard_only {
        let dir = custom_dir.map(|p| p.to_path_buf()).unwrap_or_else(|| {
            let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join("Pictures").join("Screenshots")
        });
        let _ = std::fs::create_dir_all(&dir);
        let lt = unsafe { GetLocalTime() };
        let filename = format!(
            "HyprShot_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}.bmp",
            lt.wYear, lt.wMonth, lt.wDay, lt.wHour, lt.wMinute, lt.wSecond
        );
        let file_path = dir.join(filename);
        if write_bmp_32(&file_path, &cropped, w, h).is_ok() {
            saved_path_str = file_path.to_string_lossy().to_string();
        }
    }

    // 3. Send desktop toast notification via `\\.\pipe\hyprnotify-ipc`
    send_hyprnotify_toast(
        "HyprShot Captured",
        &format!("{}x{} copied to clipboard ({})", w, h, saved_path_str),
    );

    Ok(())
}

unsafe fn copy_bgra_to_clipboard(pixels: &[u32], w: i32, h: i32) {
    let screen_dc = GetDC(HWND::default());
    if screen_dc.is_invalid() {
        return;
    }
    let mem_dc = CreateCompatibleDC(screen_dc);
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
    if let Ok(dib) = CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
        if !dib.is_invalid() && !bits.is_null() {
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u32, (w * h) as usize);
            let ddb = CreateCompatibleBitmap(screen_dc, w, h);
            if !ddb.is_invalid() {
                let src_dc = CreateCompatibleDC(screen_dc);
                let old_s = SelectObject(src_dc, dib);
                let old_d = SelectObject(mem_dc, ddb);
                let _ = BitBlt(mem_dc, 0, 0, w, h, src_dc, 0, 0, SRCCOPY);
                SelectObject(mem_dc, old_d);
                SelectObject(src_dc, old_s);
                let _ = DeleteDC(src_dc);

                if OpenClipboard(HWND::default()).is_ok() {
                    let _ = EmptyClipboard();
                    let _ = SetClipboardData(CF_BITMAP_VAL, HANDLE(ddb.0));
                    let _ = CloseClipboard();
                } else {
                    let _ = DeleteObject(ddb);
                }
            }
            let _ = DeleteObject(dib);
        }
    }
    let _ = DeleteDC(mem_dc);
    let _ = ReleaseDC(HWND::default(), screen_dc);
}

fn write_bmp_32(path: &Path, pixels: &[u32], w: i32, h: i32) -> Result<()> {
    let mut f = std::fs::File::create(path)?;
    let data_size = (w * h * 4) as u32;
    let file_size = 14 + 40 + data_size;

    // BITMAPFILEHEADER (14 bytes)
    let mut header = [0u8; 54];
    header[0] = b'B';
    header[1] = b'M';
    header[2..6].copy_from_slice(&file_size.to_le_bytes());
    header[10..14].copy_from_slice(&54u32.to_le_bytes());

    // BITMAPINFOHEADER (40 bytes)
    header[14..18].copy_from_slice(&40u32.to_le_bytes());
    header[18..22].copy_from_slice(&w.to_le_bytes());
    header[22..26].copy_from_slice(&(-h).to_le_bytes()); // top-down
    header[26..28].copy_from_slice(&1u16.to_le_bytes());
    header[28..30].copy_from_slice(&32u16.to_le_bytes());
    header[34..38].copy_from_slice(&data_size.to_le_bytes());

    f.write_all(&header)?;
    let byte_slice =
        unsafe { std::slice::from_raw_parts(pixels.as_ptr() as *const u8, data_size as usize) };
    f.write_all(byte_slice)?;
    Ok(())
}

fn send_hyprnotify_toast(title: &str, body: &str) {
    if let Ok(mut pipe) = OpenOptions::new()
        .read(true)
        .write(true)
        .open(r"\\.\pipe\hyprnotify-ipc")
    {
        let payload = serde_json::json!({
            "summary": title,
            "body": body,
            "urgency": 1,
            "timeout_ms": 3500
        });
        if let Ok(bytes) = serde_json::to_vec(&payload) {
            let _ = pipe.write_all(&bytes);
            let _ = pipe.flush();
        }
    }
}
