//! Visual layout, `JetBrainsMono NF` typography, native DWM Acrylic backdrop,
//! 144Hz typing dot pop-in animations, ripple border glow, auth-failure spring shake,
//! and macOS Big Sur / Mojave full-screen login layout for `hyprlock`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use windows::core::{s, PCWSTR};
use windows::Win32::Foundation::{BOOL, COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    AddFontResourceExW, BitBlt, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateRectRgn,
    DeleteDC, DeleteObject, DrawTextW, GetDC, GetObjectW, GetTextExtentPoint32W, ReleaseDC,
    SelectObject, SetBkMode, SetBrushOrgEx, SetStretchBltMode, SetTextColor, SetWindowRgn,
    StretchBlt, AC_SRC_ALPHA, AC_SRC_OVER, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    BLENDFUNCTION, CLEARTYPE_NATURAL_QUALITY, DIB_RGB_COLORS, DT_CENTER, DT_LEFT, DT_NOPREFIX,
    DT_RIGHT, DT_SINGLELINE, DT_VCENTER, FR_PRIVATE, HALFTONE, HBITMAP, HDC, HFONT, HRGN,
    SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, LoadImageW, UpdateLayeredWindow, IMAGE_BITMAP, LR_LOADFROMFILE, ULW_ALPHA,
};

#[derive(Debug, Clone)]
pub struct LockRenderModel {
    pub width: i32,
    pub height: i32,
    pub time_str: String,
    pub seconds_str: String,
    pub date_str: String,
    pub username: String,
    pub input_len: usize,
    pub input_selected: bool,
    pub auth_failed: bool,
    /// Per-character animated scale factor (`0.1 -> 1.0`) for each typed dot
    pub dot_scales: Vec<f32>,
    /// Smoothly interpolated visual X width of the dot cluster for gliding caret/layout
    pub anim_dots_width: f32,
    /// Keystroke pulse (`1.0 -> 0.0`) that drives border luminance and avatar ring ripple
    pub typing_pulse: f32,
    /// Auth-failure damped horizontal shake progress (`1.0 -> 0.0`)
    pub fail_shake: f32,
    /// Unlocking state (`true` once password verified and slide-up curtain is active)
    pub unlocking: bool,
    /// Unlock slide-up progress (`0.0 -> 1.0`)
    pub unlock_progress: f32,
    /// First-time password setup step (`0` = normal unlock, `1` = create new password, `2` = confirm new password)
    pub setup_step: u8,
    pub wallpaper_path: Option<String>,
    pub avatar_path: Option<String>,
    pub kbd_layout_str: String,
    pub battery_percent: u8,
    pub raw_text: String,
    pub hovered_button: u8, // 0 = None, 1 = Shut Down, 2 = Restart, 3 = Sleep, 4 = Pill right button (? / >)
    pub show_hint_popover: bool,
    pub power_menu_mode: bool,
}

impl Default for LockRenderModel {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            time_str: "12:00".to_string(),
            seconds_str: "00".to_string(),
            date_str: String::new(),
            username: String::new(),
            input_len: 0,
            input_selected: false,
            auth_failed: false,
            dot_scales: Vec::new(),
            anim_dots_width: 0.0,
            typing_pulse: 0.0,
            fail_shake: 0.0,
            unlocking: false,
            unlock_progress: 0.0,
            setup_step: 0,
            wallpaper_path: None,
            avatar_path: None,
            kbd_layout_str: "U.S.".to_string(),
            battery_percent: 100,
            raw_text: String::new(),
            hovered_button: 0,
            show_hint_popover: false,
            power_menu_mode: false,
        }
    }
}

#[inline]
pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

pub const COLOR_BG0: COLORREF = rgb(18, 19, 23); // #121317 deep obsidian charcoal
pub const COLOR_BG0_U32: u32 = 0xFF12_1317;
pub const COLOR_CARD_BG: COLORREF = rgb(24, 26, 32); // #181A20 elevated surface
pub const COLOR_PILL_BG: COLORREF = rgb(30, 32, 40); // #1E2028 input & badge surface
pub const COLOR_BORDER: COLORREF = rgb(56, 59, 72); // #383B48 subtle zinc border
pub const COLOR_BORDER_ACTIVE: COLORREF = rgb(228, 228, 231); // #E4E4E7 crisp active border
pub const COLOR_BORDER_FAIL: COLORREF = rgb(243, 139, 168); // #F38BA8 coral red
pub const COLOR_BORDER_SUCCESS: COLORREF = rgb(166, 227, 161); // #A6E3A1 emerald green
pub const COLOR_FG_PRIMARY: COLORREF = rgb(244, 244, 245); // #F4F4F5 crisp white
pub const COLOR_FG_MUTED: COLORREF = rgb(161, 161, 170); // #A1A1AA zinc-400
pub const COLOR_PLACEHOLDER: COLORREF = rgb(113, 113, 122); // #71717A zinc-500

#[inline]
fn lerp_color(a: COLORREF, b: COLORREF, t: f32) -> COLORREF {
    let t = t.clamp(0.0, 1.0);
    let ar = (a.0 & 0xFF) as f32;
    let ag = ((a.0 >> 8) & 0xFF) as f32;
    let ab = ((a.0 >> 16) & 0xFF) as f32;
    let br = (b.0 & 0xFF) as f32;
    let bg = ((b.0 >> 8) & 0xFF) as f32;
    let bb = ((b.0 >> 16) & 0xFF) as f32;
    rgb(
        (ar + (br - ar) * t).round() as u8,
        (ag + (bg - ag) * t).round() as u8,
        (ab + (bb - ab) * t).round() as u8,
    )
}

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

unsafe fn load_image_hbitmap(path: &str) -> Option<HBITMAP> {
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

    let path_u16: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    let mut gp_bmp: usize = 0;
    if GdipCreateBitmapFromFile(path_u16.as_ptr(), &mut gp_bmp) == 0 && gp_bmp != 0 {
        let mut hbmp = HBITMAP::default();
        let status = GdipCreateHBITMAPFromBitmap(gp_bmp, &mut hbmp, 0xFF0D_0E13);
        let _ = GdipDisposeImage(gp_bmp);
        if status == 0 && !hbmp.is_invalid() && !hbmp.0.is_null() {
            return Some(hbmp);
        }
    }

    if let Ok(handle) = LoadImageW(
        None,
        PCWSTR(path_u16.as_ptr()),
        IMAGE_BITMAP,
        0,
        0,
        LR_LOADFROMFILE,
    ) {
        let hbmp = HBITMAP(handle.0);
        if !hbmp.is_invalid() && !hbmp.0.is_null() {
            return Some(hbmp);
        }
    }

    if let Some(parent) = Path::new(path).parent() {
        let bmp_sibling = parent.join("wallpaper.bmp");
        if bmp_sibling.exists() {
            let sib_u16: Vec<u16> = bmp_sibling
                .to_string_lossy()
                .encode_utf16()
                .chain(Some(0))
                .collect();
            if let Ok(handle) = LoadImageW(
                None,
                PCWSTR(sib_u16.as_ptr()),
                IMAGE_BITMAP,
                0,
                0,
                LR_LOADFROMFILE,
            ) {
                let hbmp = HBITMAP(handle.0);
                if !hbmp.is_invalid() && !hbmp.0.is_null() {
                    return Some(hbmp);
                }
            }
        }
    }

    None
}

unsafe fn rasterize_scaled_image(
    hdc: HDC,
    path: &str,
    dst_w: i32,
    dst_h: i32,
) -> Option<Vec<u32>> {
    let src_hbmp = load_image_hbitmap(path)?;

    let mut bmp_info = BITMAP::default();
    let got = GetObjectW(
        src_hbmp,
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut bmp_info as *mut _ as *mut core::ffi::c_void),
    );
    let src_w = if got > 0 && bmp_info.bmWidth > 0 {
        bmp_info.bmWidth
    } else {
        dst_w
    };
    let src_h = if got > 0 && bmp_info.bmHeight > 0 {
        bmp_info.bmHeight.abs()
    } else {
        dst_h
    };

    let dst_dc = CreateCompatibleDC(hdc);
    let src_dc = CreateCompatibleDC(hdc);
    if dst_dc.is_invalid() || src_dc.is_invalid() {
        if !dst_dc.is_invalid() {
            let _ = DeleteDC(dst_dc);
        }
        if !src_dc.is_invalid() {
            let _ = DeleteDC(src_dc);
        }
        let _ = DeleteObject(src_hbmp);
        return None;
    }

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: dst_w,
            biHeight: -dst_h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (dst_w * dst_h * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
    let Ok(dst_dib) = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
        let _ = DeleteDC(dst_dc);
        let _ = DeleteDC(src_dc);
        let _ = DeleteObject(src_hbmp);
        return None;
    };

    let old_dst = SelectObject(dst_dc, dst_dib);
    let old_src = SelectObject(src_dc, src_hbmp);

    let _ = SetStretchBltMode(dst_dc, HALFTONE);
    let _ = SetBrushOrgEx(dst_dc, 0, 0, None);

    let scale_x = dst_w as f64 / src_w.max(1) as f64;
    let scale_y = dst_h as f64 / src_h.max(1) as f64;
    let scale = scale_x.max(scale_y);

    let crop_w = ((dst_w as f64 / scale).round() as i32).clamp(1, src_w);
    let crop_h = ((dst_h as f64 / scale).round() as i32).clamp(1, src_h);
    let crop_x = ((src_w - crop_w) / 2).max(0);
    let crop_y = ((src_h - crop_h) / 2).max(0);

    let _ = StretchBlt(
        dst_dc,
        0,
        0,
        dst_w,
        dst_h,
        src_dc,
        crop_x,
        crop_y,
        crop_w,
        crop_h,
        SRCCOPY,
    );

    let mut out = vec![0u32; (dst_w * dst_h) as usize];
    let slice = std::slice::from_raw_parts(bits as *const u32, out.len());
    out.copy_from_slice(slice);

    SelectObject(src_dc, old_src);
    SelectObject(dst_dc, old_dst);
    let _ = DeleteObject(dst_dib);
    let _ = DeleteObject(src_hbmp);
    let _ = DeleteDC(src_dc);
    let _ = DeleteDC(dst_dc);

    Some(out)
}

fn generate_procedural_backdrop(width: i32, height: i32) -> Vec<u32> {
    let w = width.max(1) as usize;
    let h = height.max(1) as usize;
    let mut pixels = vec![0u32; w * h];
    let cx = width as f32 * 0.5;
    let cy = height as f32 * 0.5;
    let max_r = (cx * cx + cy * cy).sqrt().max(1.0);

    for y in 0..height {
        let y_idx = (y as usize) * w;
        let y_ratio = y as f32 / height as f32;
        for x in 0..width {
            let x_ratio = x as f32 / width as f32;
            let base_t = (x_ratio + y_ratio) * 0.5;
            let r0 = 13.0 + 6.0 * base_t;
            let g0 = 14.0 + 8.0 * base_t;
            let b0 = 19.0 + 14.0 * base_t;

            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let glow = (1.0 - (dist / max_r).min(1.0)).powi(2) * 14.0;

            let r = ((r0 + glow * 0.4).round() as u32).min(255);
            let g = ((g0 + glow * 0.7).round() as u32).min(255);
            let b = ((b0 + glow * 1.0).round() as u32).min(255);
            pixels[y_idx + (x as usize)] = 0xFF00_0000 | (r << 16) | (g << 8) | b;
        }
    }
    pixels
}

static WALLPAPER_CACHE: OnceLock<Mutex<Option<(i32, i32, Option<String>, Vec<u32>)>>> = OnceLock::new();

unsafe fn get_cached_wallpaper(hdc: HDC, width: i32, height: i32, path: Option<&str>) -> Vec<u32> {
    let cache_mutex = WALLPAPER_CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache_mutex.lock() {
        if let Some((w, h, ref cached_path, ref data)) = *guard {
            if w == width && h == height && cached_path.as_deref() == path {
                return data.clone();
            }
        }
    }

    let mut pixels = if let Some(p) = path {
        rasterize_scaled_image(hdc, p, width, height)
            .unwrap_or_else(|| generate_procedural_backdrop(width, height))
    } else {
        generate_procedural_backdrop(width, height)
    };

    // Apply ~18% dark readability tint and subtle vignette so white text and frosted glass pop
    let cx = width as f32 * 0.5;
    let cy = height as f32 * 0.5;
    let max_dist = (cx * cx + cy * cy).sqrt().max(1.0);
    for y in 0..height {
        let row = (y as usize) * (width as usize);
        let dy = y as f32 - cy;
        for x in 0..width {
            let dx = x as f32 - cx;
            let dist = (dx * dx + dy * dy).sqrt() / max_dist;
            let vignette = (1.0 - dist * 0.15).max(0.65);
            let factor = 0.82 * vignette;

            let p = pixels[row + x as usize];
            let r = (((p >> 16) & 0xFF) as f32 * factor).round() as u32;
            let g = (((p >> 8) & 0xFF) as f32 * factor).round() as u32;
            let b = ((p & 0xFF) as f32 * factor).round() as u32;
            pixels[row + x as usize] = 0xFF00_0000 | (r << 16) | (g << 8) | b;
        }
    }

    if let Ok(mut guard) = cache_mutex.lock() {
        *guard = Some((width, height, path.map(|s| s.to_string()), pixels.clone()));
    }

    pixels
}

static AVATAR_CACHE: OnceLock<Mutex<Option<(String, Vec<u32>)>>> = OnceLock::new();

unsafe fn get_cached_avatar(hdc: HDC, path: &str) -> Option<Vec<u32>> {
    let cache_mutex = AVATAR_CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache_mutex.lock() {
        if let Some((ref cached_path, ref data)) = *guard {
            if cached_path == path {
                return Some(data.clone());
            }
        }
    }

    let raw = rasterize_scaled_image(hdc, path, 96, 96)?;
    let mut masked = vec![0u32; 96 * 96];
    for ay in 0..96 {
        for ax in 0..96 {
            let dx = ax as f32 - 47.5;
            let dy = ay as f32 - 47.5;
            let dist = (dx * dx + dy * dy).sqrt();
            let cov = (48.0 - dist).clamp(0.0, 1.0);
            let src_p = raw[ay * 96 + ax];
            let a = (cov * 255.0).round() as u32;
            masked[ay * 96 + ax] = (a << 24) | (src_p & 0x00FF_FFFF);
        }
    }

    if let Ok(mut guard) = cache_mutex.lock() {
        *guard = Some((path.to_string(), masked.clone()));
    }

    Some(masked)
}

static FONT_LOADED: OnceLock<()> = OnceLock::new();

pub unsafe fn ensure_fonts_loaded() {
    FONT_LOADED.get_or_init(|| {
        let userprofile = std::env::var("USERPROFILE").unwrap_or_default();
        let localappdata = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let font_dirs = [
            format!("{userprofile}\\Downloads\\JetBrainsMono"),
            "assets\\fonts".to_string(),
            "C:\\Users\\mbura\\Projects\\Other\\hyprland-win\\assets\\fonts".to_string(),
            "C:\\Users\\mbura\\Projects\\Other\\hyprland-win\\assets\\fonts".to_string(),
            format!("{localappdata}\\Microsoft\\Windows\\Fonts"),
            "C:\\Windows\\Fonts".to_string(),
        ];
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

unsafe fn create_font(family: &str, height: i32, weight: i32) -> HFONT {
    let normalized = if family.eq_ignore_ascii_case("JetBrainsMono Nerd Font")
        || family.eq_ignore_ascii_case("JetBrains Mono")
    {
        "JetBrainsMono NF"
    } else if family.is_empty() {
        "SF Pro Display"
    } else {
        family
    };
    let wide: Vec<u16> = normalized.encode_utf16().chain(Some(0)).collect();
    CreateFontW(
        height,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        1,
        0,
        0,
        CLEARTYPE_NATURAL_QUALITY,
        0,
        PCWSTR(wide.as_ptr()),
    )
}

unsafe fn draw_text_with_shadow(
    hdc: HDC,
    text: &str,
    rc: &RECT,
    flags: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
    text_color: COLORREF,
) {
    let mut shadow_rc = RECT {
        left: rc.left + 1,
        top: rc.top + 1,
        right: rc.right + 1,
        bottom: rc.bottom + 1,
    };
    let mut main_rc = *rc;
    let mut utf16_shadow: Vec<u16> = text.encode_utf16().collect();
    let mut utf16_main: Vec<u16> = text.encode_utf16().collect();
    SetTextColor(hdc, rgb(10, 12, 18));
    DrawTextW(hdc, &mut utf16_shadow, &mut shadow_rc, flags);
    SetTextColor(hdc, text_color);
    DrawTextW(hdc, &mut utf16_main, &mut main_rc, flags);
}

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

pub unsafe fn set_overlay_lock_cutout(screen_w: i32, screen_h: i32, locked: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE, SW_SHOWNOACTIVATE};
    let overlay_classes = ["HyprlandWinNativeDCompOverlay\0", "HyprlandWinNativeDCompOverlay\0"];
    for cls_str in overlay_classes {
        let cls: Vec<u16> = cls_str.encode_utf16().collect();
        if let Ok(ov_hwnd) = FindWindowW(PCWSTR(cls.as_ptr()), PCWSTR::null()) {
            if !ov_hwnd.0.is_null() {
                if locked {
                    let rgn_empty = CreateRectRgn(0, 0, 0, 0);
                    SetWindowRgn(ov_hwnd, rgn_empty, true);
                } else {
                    SetWindowRgn(ov_hwnd, HRGN::default(), true);
                }
                break;
            }
        }
    }
    let bar_classes = [
        "HyprBarTopOverlay\0",
        "HyprBarScreenFrameOverlay\0",
        "HyprBarCalendarPopup\0",
        "HyprNotifyPopupClass\0",
        "HyprLauncherClass\0",
    ];
    for c in bar_classes {
        let wide: Vec<u16> = c.encode_utf16().collect();
        if let Ok(h) = FindWindowW(PCWSTR(wide.as_ptr()), PCWSTR::null()) {
            if !h.0.is_null() {
                if locked {
                    let _ = ShowWindow(h, SW_HIDE);
                } else if c == "HyprBarTopOverlay\0" || c == "HyprBarScreenFrameOverlay\0" {
                    let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
                }
            }
        }
    }
    let _ = (screen_w, screen_h);
}

#[inline]
fn alpha_blend_bgra(bg: u32, fg_color: COLORREF, alpha_u8: u8) -> u32 {
    if alpha_u8 == 0 {
        return bg;
    }
    let fg_r = fg_color.0 & 0xFF;
    let fg_g = (fg_color.0 >> 8) & 0xFF;
    let fg_b = (fg_color.0 >> 16) & 0xFF;
    if alpha_u8 == 255 {
        return 0xFF00_0000 | (fg_r << 16) | (fg_g << 8) | fg_b;
    }
    let bg_r = (bg >> 16) & 0xFF;
    let bg_g = (bg >> 8) & 0xFF;
    let bg_b = bg & 0xFF;
    let a = alpha_u8 as u32;
    let inv_a = 255 - a;
    let out_r = (fg_r * a + bg_r * inv_a) / 255;
    let out_g = (fg_g * a + bg_g * inv_a) / 255;
    let out_b = (fg_b * a + bg_b * inv_a) / 255;
    0xFF00_0000 | (out_r << 16) | (out_g << 8) | out_b
}

#[allow(clippy::too_many_arguments)]
fn fill_aa_round_rect_layered(
    pixels: &mut [u32],
    alpha_map: &mut [u8],
    stride: usize,
    max_w: i32,
    max_h: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    radius: f32,
    fill_rgb: COLORREF,
    fill_alpha_u8: u8,
    border_rgb: COLORREF,
    border_alpha_u8: u8,
    border_thickness: f32,
) {
    if w <= 0 || h <= 0 {
        return;
    }
    let half_w = w as f32 * 0.5;
    let half_h = h as f32 * 0.5;
    let center_x = x as f32 + half_w;
    let center_y = y as f32 + half_h;
    let r = radius.min(half_w).min(half_h).max(0.0);

    let min_x = (x - 1).max(0);
    let max_x = (x + w + 1).min(max_w);
    let min_y = (y - 1).max(0);
    let max_y = (y + h + 1).min(max_h);

    for py in min_y..max_y {
        let row_offset = (py as usize) * stride;
        let py_f = py as f32 + 0.5;
        let qy = (py_f - center_y).abs() - half_h + r;

        for px in min_x..max_x {
            let px_f = px as f32 + 0.5;
            let qx = (px_f - center_x).abs() - half_w + r;

            let qx_clamped = qx.max(0.0);
            let qy_clamped = qy.max(0.0);
            let dist =
                qx.max(qy).min(0.0) + (qx_clamped * qx_clamped + qy_clamped * qy_clamped).sqrt() - r;

            let outer_cov = (0.5 - dist).clamp(0.0, 1.0);
            if outer_cov <= 0.001 {
                continue;
            }

            let idx = row_offset + (px as usize);
            if idx >= pixels.len() {
                continue;
            }

            let bg = pixels[idx];
            let inner_dist = dist + border_thickness;
            let inner_cov = (0.5 - inner_dist).clamp(0.0, 1.0);
            let border_cov = (outer_cov - inner_cov).max(0.0);

            let b_a = (border_cov * (border_alpha_u8 as f32)).round() as u8;
            let f_a = (inner_cov * (fill_alpha_u8 as f32)).round() as u8;

            let with_border = alpha_blend_bgra(bg, border_rgb, b_a);
            pixels[idx] = alpha_blend_bgra(with_border, fill_rgb, f_a);

            let target_a = (inner_cov * (fill_alpha_u8 as f32) + border_cov * (border_alpha_u8 as f32))
                .round()
                .clamp(0.0, 255.0) as u8;
            alpha_map[idx] = alpha_map[idx].max(target_a);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_aa_circle_layered(
    pixels: &mut [u32],
    alpha_map: &mut [u8],
    stride: usize,
    max_w: i32,
    max_h: i32,
    center_x: f32,
    center_y: f32,
    radius: f32,
    fill_rgb: COLORREF,
    fill_alpha_u8: u8,
    border_rgb: COLORREF,
    border_alpha_u8: u8,
    border_thickness: f32,
) {
    if radius <= 0.1 {
        return;
    }
    let min_x = ((center_x - radius - 2.0).floor() as i32).max(0);
    let max_x = ((center_x + radius + 2.0).ceil() as i32).min(max_w);
    let min_y = ((center_y - radius - 2.0).floor() as i32).max(0);
    let max_y = ((center_y + radius + 2.0).ceil() as i32).min(max_h);

    for py in min_y..max_y {
        let row_offset = (py as usize) * stride;
        let dy = (py as f32 + 0.5) - center_y;
        for px in min_x..max_x {
            let dx = (px as f32 + 0.5) - center_x;
            let dist = (dx * dx + dy * dy).sqrt() - radius;
            let outer_cov = (0.5 - dist).clamp(0.0, 1.0);
            if outer_cov <= 0.001 {
                continue;
            }
            let idx = row_offset + (px as usize);
            if idx >= pixels.len() {
                continue;
            }
            let bg = pixels[idx];
            let inner_dist = dist + border_thickness;
            let inner_cov = (0.5 - inner_dist).clamp(0.0, 1.0);
            let border_cov = (outer_cov - inner_cov).max(0.0);

            let b_a = (border_cov * (border_alpha_u8 as f32)).round() as u8;
            let f_a = (inner_cov * (fill_alpha_u8 as f32)).round() as u8;

            let with_border = alpha_blend_bgra(bg, border_rgb, b_a);
            pixels[idx] = alpha_blend_bgra(with_border, fill_rgb, f_a);

            let target_a = (inner_cov * (fill_alpha_u8 as f32) + border_cov * (border_alpha_u8 as f32))
                .round()
                .clamp(0.0, 255.0) as u8;
            alpha_map[idx] = alpha_map[idx].max(target_a);
        }
    }
}

#[derive(Clone)]
struct CachedGlyph {
    ink_w: i32,
    ink_h: i32,
    mask: Vec<u8>,
}

static GLYPH_INK_CACHE: OnceLock<Mutex<HashMap<String, CachedGlyph>>> = OnceLock::new();

/// Renders a single Nerd Font icon glyph onto a scratch 48x48 DIB once, caches its exact non-zero
/// ink pixel bounding box `(ink_w, ink_h, mask)`, and alpha-blends the glyph pixel-accurately
/// centered inside the destination surface.
unsafe fn draw_icon_ink_centered(
    pixels: &mut [u32],
    alpha_map: &mut [u8],
    stride: usize,
    max_w: i32,
    max_h: i32,
    glyph: &str,
    target_cx: f32,
    target_cy: f32,
    color: COLORREF,
    font: HFONT,
) {
    if glyph.is_empty() {
        return;
    }

    let cache = GLYPH_INK_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(cached) = guard.get(glyph) {
            let ink_w = cached.ink_w;
            let ink_h = cached.ink_h;
            if ink_w > 0 && ink_h > 0 {
                let dst_x0 = (target_cx - (ink_w as f32) * 0.5).round() as i32;
                let dst_y0 = (target_cy - (ink_h as f32) * 0.5).round() as i32;
                for iy in 0..ink_h {
                    let dy = dst_y0 + iy;
                    if dy < 0 || dy >= max_h {
                        continue;
                    }
                    let mask_row = (iy * ink_w) as usize;
                    for ix in 0..ink_w {
                        let dx = dst_x0 + ix;
                        if dx < 0 || dx >= max_w {
                            continue;
                        }
                        let cov = cached.mask[mask_row + ix as usize];
                        if cov > 0 {
                            let idx = (dy as usize) * stride + (dx as usize);
                            pixels[idx] = alpha_blend_bgra(pixels[idx], color, cov);
                            alpha_map[idx] = alpha_map[idx].max(cov);
                        }
                    }
                }
            }
            return;
        }
    }

    const PAD_SZ: i32 = 48;
    let screen_dc = GetDC(HWND::default());
    if screen_dc.is_invalid() {
        return;
    }
    let dc = CreateCompatibleDC(screen_dc);
    if dc.is_invalid() {
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
    if let Ok(bmp) = CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
        if !bmp.is_invalid() && !bits.is_null() {
            let scratch: &mut [u32] =
                std::slice::from_raw_parts_mut(bits as *mut u32, (PAD_SZ * PAD_SZ) as usize);
            scratch.fill(0x0000_0000);

            let old_bmp = SelectObject(dc, bmp);
            let old_font = SelectObject(dc, font);
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

            for y in 0..PAD_SZ {
                for x in 0..PAD_SZ {
                    let p = scratch[(y * PAD_SZ + x) as usize];
                    let lum = ((p >> 16) & 0xFF).max((p >> 8) & 0xFF).max(p & 0xFF);
                    if lum > 16 {
                        if x < min_x {
                            min_x = x;
                        }
                        if x > max_x {
                            max_x = x;
                        }
                        if y < min_y {
                            min_y = y;
                        }
                        if y > max_y {
                            max_y = y;
                        }
                    }
                }
            }

            if max_x >= min_x && max_y >= min_y {
                let ink_w = max_x - min_x + 1;
                let ink_h = max_y - min_y + 1;
                let mut mask = vec![0u8; (ink_w * ink_h) as usize];
                let dst_x0 = (target_cx - (ink_w as f32) * 0.5).round() as i32;
                let dst_y0 = (target_cy - (ink_h as f32) * 0.5).round() as i32;

                for iy in 0..ink_h {
                    let dy = dst_y0 + iy;
                    let sy = min_y + iy;
                    for ix in 0..ink_w {
                        let sx = min_x + ix;
                        let p = scratch[(sy * PAD_SZ + sx) as usize];
                        let cov = ((p >> 16) & 0xFF).max((p >> 8) & 0xFF).max(p & 0xFF) as u8;
                        mask[(iy * ink_w + ix) as usize] = cov;
                        let dx = dst_x0 + ix;
                        if cov > 0 && dy >= 0 && dy < max_h && dx >= 0 && dx < max_w {
                            let idx = (dy as usize) * stride + (dx as usize);
                            pixels[idx] = alpha_blend_bgra(pixels[idx], color, cov);
                            alpha_map[idx] = alpha_map[idx].max(cov);
                        }
                    }
                }

                if let Ok(mut guard) = cache.lock() {
                    guard.insert(
                        glyph.to_string(),
                        CachedGlyph {
                            ink_w,
                            ink_h,
                            mask,
                        },
                    );
                }
            }

            SelectObject(dc, old_font);
            SelectObject(dc, old_bmp);
            let _ = DeleteObject(bmp);
        }
    }

    let _ = DeleteDC(dc);
    let _ = ReleaseDC(HWND::default(), screen_dc);
}

/// Steps 144Hz typing dot scale, keystroke border pulse, auth-failure shake, and slide-up unlock curtain.
/// Returns `(needs_repaint, unlock_completed)`.
pub fn step_lock_animations(model: &mut LockRenderModel, dt: f32) -> (bool, bool) {
    let mut active = false;
    let fast_alpha = 1.0 - (-26.0 * dt).exp();

    // Sync dot_scales length with input_len (capped at 20 visible dots)
    let visible_dots = model.input_len.min(20);
    while model.dot_scales.len() < visible_dots {
        model.dot_scales.push(0.15);
        active = true;
    }
    while model.dot_scales.len() > visible_dots {
        model.dot_scales.pop();
        active = true;
    }

    for scale in &mut model.dot_scales {
        let diff = 1.0 - *scale;
        if diff.abs() > 0.005 {
            *scale += diff * fast_alpha;
            active = true;
        } else {
            *scale = 1.0;
        }
    }

    let dot_spacing = if visible_dots > 14 { 10.0 } else { 12.0 };
    let target_dots_w = if visible_dots == 0 {
        0.0
    } else {
        (visible_dots as f32 - 1.0) * dot_spacing
    };
    let w_diff = target_dots_w - model.anim_dots_width;
    if w_diff.abs() > 0.25 {
        model.anim_dots_width += w_diff * (1.0 - (-28.0 * dt).exp());
        active = true;
    } else {
        model.anim_dots_width = target_dots_w;
    }

    if model.typing_pulse > 0.005 {
        model.typing_pulse = (model.typing_pulse - dt * 4.5).max(0.0);
        active = true;
    } else {
        model.typing_pulse = 0.0;
    }

    if model.fail_shake > 0.005 {
        model.fail_shake = (model.fail_shake - dt * 2.8).max(0.0);
        active = true;
    } else {
        model.fail_shake = 0.0;
    }

    let mut unlock_done = false;
    if model.unlocking {
        model.unlock_progress = (model.unlock_progress + dt * 3.6).min(1.0);
        active = true;
        if model.unlock_progress >= 1.0 {
            unlock_done = true;
        }
    }

    (active, unlock_done)
}

#[inline]
pub fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) * 0.5
    }
}

pub unsafe fn render_lockscreen(hwnd: HWND, model: &LockRenderModel) {
    let width = model.width.max(640);
    let height = model.height.max(480);

    ensure_fonts_loaded();

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
    let Ok(dib_bmp) = CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(HWND::default(), screen_dc);
        return;
    };

    let total_px = (width * height) as usize;
    let pixels: &mut [u32] = std::slice::from_raw_parts_mut(bits as *mut u32, total_px);
    let mut alpha_map = vec![255u8; total_px];

    // 1. Wallpaper background with HALFTONE scaling & readability tint
    let wp = get_cached_wallpaper(screen_dc, width, height, model.wallpaper_path.as_deref());
    if wp.len() == total_px {
        pixels.copy_from_slice(&wp);
    } else {
        pixels.fill(COLOR_BG0_U32);
    }

    if model.power_menu_mode {
        for p in pixels.iter_mut() {
            *p = alpha_blend_bgra(*p, rgb(10, 11, 15), 145);
        }
    }

    let cx = width / 2;
    let cy = height / 2;

    // Damped horizontal shake offset when wrong password is submitted
    let shake_dx = if model.fail_shake > 0.001 {
        ((model.fail_shake * std::f32::consts::TAU * 3.0).sin() * 14.0 * model.fail_shake).round()
            as i32
    } else {
        0
    };
    let center_x = cx + shake_dx;

    // 2. Top-Right Status Bar Icons (SDF Pass): Battery (\u{F240}), Wi-Fi (\u{F1EB}), Keyboard (\u{F11C})
    let status_cy = 26.0;
    let time_right = width - 24;
    let time_left = time_right - 58;
    let battery_text_right = time_left - 16;
    let battery_text_left = battery_text_right - 44;
    let battery_icon_x = (battery_text_left - 14) as f32;
    let wifi_icon_x = battery_icon_x - 26.0;
    let kbd_icon_x = wifi_icon_x - 28.0;
    let kbd_text_right = (kbd_icon_x - 14.0).round() as i32;
    let kbd_text_left = kbd_text_right - 46;

    let font_icon_sm = create_font("JetBrainsMono NF", -14, 600);
    draw_icon_ink_centered(
        pixels,
        &mut alpha_map,
        width as usize,
        width,
        height,
        "\u{F240}",
        battery_icon_x,
        status_cy,
        rgb(240, 244, 252),
        font_icon_sm,
    );
    draw_icon_ink_centered(
        pixels,
        &mut alpha_map,
        width as usize,
        width,
        height,
        "\u{F1EB}",
        wifi_icon_x,
        status_cy,
        rgb(240, 244, 252),
        font_icon_sm,
    );
    draw_icon_ink_centered(
        pixels,
        &mut alpha_map,
        width as usize,
        width,
        height,
        "\u{F11C}",
        kbd_icon_x,
        status_cy,
        rgb(240, 244, 252),
        font_icon_sm,
    );
    let _ = DeleteObject(font_icon_sm);

    let inp_w = 216;
    let inp_h = 32;
    let inp_x = center_x - inp_w / 2;
    let inp_y = cy + 12;
    let btn_cx = (inp_x + inp_w + 20) as f32;
    let btn_cy = (inp_y + 16) as f32;

    if model.power_menu_mode {
        // 4 large circular buttons (76x76, r = 38.0, gap = 64) centered at cy = height / 2
        let pwr_items = [
            (1u8, "\u{F011}", rgb(243, 139, 168)), // Shut Down (coral red)
            (2u8, "\u{F01E}", rgb(249, 226, 175)), // Reboot (warm amber)
            (3u8, "\u{F186}", rgb(205, 214, 244)), // Sleep (lavender)
            (5u8, "\u{F023}", rgb(166, 227, 161)), // Lock (emerald)
        ];
        let font_pwr_lg = create_font("JetBrainsMono NF", -28, 600);
        for (i, &(id, glyph, accent)) in pwr_items.iter().enumerate() {
            let b_cx = (cx - 248 + (i as i32) * 140 + 38) as f32;
            let b_cy = cy as f32;
            let is_hover = model.hovered_button == id;
            let (btn_fill_col, btn_fill_a, btn_border_col, btn_border_a, btn_border_th, icon_col) =
                if is_hover {
                    (rgb(48, 54, 70), 220, accent, 245, 2.0, accent)
                } else {
                    (rgb(24, 26, 34), 160, rgb(255, 255, 255), 120, 1.2, rgb(240, 244, 252))
                };
            fill_aa_circle_layered(
                pixels,
                &mut alpha_map,
                width as usize,
                width,
                height,
                b_cx,
                b_cy,
                38.0,
                btn_fill_col,
                btn_fill_a,
                btn_border_col,
                btn_border_a,
                btn_border_th,
            );
            draw_icon_ink_centered(
                pixels,
                &mut alpha_map,
                width as usize,
                width,
                height,
                glyph,
                b_cx,
                b_cy,
                icon_col,
                font_pwr_lg,
            );
        }
        let _ = DeleteObject(font_pwr_lg);
    } else {
        // 3. Circular User Avatar (96x96, radius = 48.0) centered at (center_x, cy - 96)
    let avatar_cx = center_x as f32;
    let avatar_cy = (cy - 96) as f32;

    // Keystroke expanding ripple ring
    if model.typing_pulse > 0.01 {
        let ripple_r = 48.0 + (1.0 - model.typing_pulse) * 16.0;
        let ripple_a = (model.typing_pulse * 120.0).round() as u8;
        fill_aa_circle_layered(
            pixels,
            &mut alpha_map,
            width as usize,
            width,
            height,
            avatar_cx,
            avatar_cy,
            ripple_r,
            rgb(24, 26, 34),
            0,
            rgb(255, 255, 255),
            ripple_a,
            1.8,
        );
    }

    // Soft dark outer shadow ring (radius = 50.0, fill_alpha = 65)
    fill_aa_circle_layered(
        pixels,
        &mut alpha_map,
        width as usize,
        width,
        height,
        avatar_cx,
        avatar_cy,
        50.0,
        rgb(0, 0, 0),
        65,
        rgb(0, 0, 0),
        0,
        0.0,
    );

    let mut avatar_drawn = false;
    if let Some(ref av_path) = model.avatar_path {
        if let Some(avatar_data) = get_cached_avatar(screen_dc, av_path) {
            let av_x0 = center_x - 48;
            let av_y0 = cy - 96 - 48;
            for ay in 0..96 {
                let py = av_y0 + ay;
                if py < 0 || py >= height {
                    continue;
                }
                let row_idx = (py as usize) * (width as usize);
                for ax in 0..96 {
                    let px = av_x0 + ax;
                    if px < 0 || px >= width {
                        continue;
                    }
                    let p = avatar_data[(ay * 96 + ax) as usize];
                    let cov = (p >> 24) as u8;
                    if cov > 0 {
                        let idx = row_idx + (px as usize);
                        pixels[idx] = alpha_blend_bgra(pixels[idx], COLORREF(p & 0x00FF_FFFF), cov);
                    }
                }
            }
            avatar_drawn = true;
        }
    }

    if !avatar_drawn {
        // Frosted dark circle fallback with user silhouette icon
        fill_aa_circle_layered(
            pixels,
            &mut alpha_map,
            width as usize,
            width,
            height,
            avatar_cx,
            avatar_cy,
            48.0,
            rgb(40, 44, 56),
            175,
            rgb(255, 255, 255),
            160,
            1.5,
        );
        let font_av = create_font("JetBrainsMono NF", -36, 600);
        draw_icon_ink_centered(
            pixels,
            &mut alpha_map,
            width as usize,
            width,
            height,
            "\u{F007}",
            avatar_cx,
            avatar_cy,
            rgb(240, 244, 252),
            font_av,
        );
        let _ = DeleteObject(font_av);
    }

    // Crisp 1.5px white avatar border ring (border_alpha = 150)
    fill_aa_circle_layered(
        pixels,
        &mut alpha_map,
        width as usize,
        width,
        height,
        avatar_cx,
        avatar_cy,
        48.0,
        rgb(0, 0, 0),
        0,
        rgb(255, 255, 255),
        150,
        1.5,
    );

    // 4. Dark Smoked Frosted Password Input Pill (width 216, height 32, radius 16) at cy + 12
    let inp_w = 216;
    let inp_h = 32;
    let inp_x = center_x - inp_w / 2;
    let inp_y = cy + 12;

    let inp_border = if model.unlocking {
        rgb(48, 209, 88) // Emerald
    } else if model.auth_failed {
        rgb(255, 69, 58) // Coral
    } else if model.typing_pulse > 0.02 {
        rgb(255, 255, 255)
    } else {
        rgb(255, 255, 255)
    };
    let inp_border_alpha = if model.unlocking || model.auth_failed {
        240
    } else if model.typing_pulse > 0.02 {
        210
    } else {
        95
    };

    // Subtle outer glow around password pill when typing or unlocking
    if model.typing_pulse > 0.02 || model.unlocking {
        let glow_col = if model.unlocking {
            rgb(48, 209, 88)
        } else {
            rgb(255, 255, 255)
        };
        let glow_a = if model.unlocking {
            100
        } else {
            (model.typing_pulse * 80.0).round() as u8
        };
        fill_aa_round_rect_layered(
            pixels,
            &mut alpha_map,
            width as usize,
            width,
            height,
            inp_x - 2,
            inp_y - 2,
            inp_w + 4,
            inp_h + 4,
            18.0,
            rgb(24, 26, 34),
            0,
            glow_col,
            glow_a,
            1.8,
        );
    }

    // Dark smoked frosted glass pill background (~65% alpha over wallpaper)
    fill_aa_round_rect_layered(
        pixels,
        &mut alpha_map,
        width as usize,
        width,
        height,
        inp_x,
        inp_y,
        inp_w,
        inp_h,
        16.0,
        rgb(24, 26, 34),
        165,
        inp_border,
        inp_border_alpha,
        1.2,
    );

    // Ctrl+A selection highlight pill inside password capsule
    if model.input_selected && model.input_len > 0 && !model.unlocking {
        let sel_w = (model.anim_dots_width.round() as i32 + 20).min(inp_w - 24);
        fill_aa_round_rect_layered(
            pixels,
            &mut alpha_map,
            width as usize,
            width,
            height,
            center_x - sel_w / 2,
            inp_y + 4,
            sel_w,
            inp_h - 8,
            12.0,
            rgb(60, 90, 160),
            190,
            rgb(140, 180, 255),
            220,
            1.0,
        );
    }

    // Animated SDF Password Dots & Gliding Caret inside Password Pill (non-command mode)
    let is_command_mode = model.raw_text.starts_with('>');
    if !is_command_mode && !model.auth_failed && !model.unlocking && model.input_len > 0 {
        let visible_dots = model.dot_scales.len();
        let dot_spacing = if visible_dots > 14 { 10.0 } else { 12.0 };
        // Smooth 144Hz centering glide using anim_dots_width
        let start_dot_x = center_x as f32 - model.anim_dots_width * 0.5;
        let base_dot_cy = inp_y as f32 + inp_h as f32 * 0.5;

        for (i, &scale) in model.dot_scales.iter().enumerate() {
            let is_newest = i + 1 == visible_dots;
            let pulse_boost = if is_newest {
                1.0 + model.typing_pulse * 0.25
            } else {
                1.0
            };
            let y_pop = (1.0 - scale) * 7.0;
            let dot_cy = base_dot_cy + y_pop;
            let r = (4.2 * scale * pulse_boost).clamp(1.2, 5.5);
            let dot_x = start_dot_x + (i as f32) * dot_spacing;
            let dot_alpha = (scale.clamp(0.0, 1.0) * 255.0).round() as u8;
            fill_aa_circle_layered(
                pixels,
                &mut alpha_map,
                width as usize,
                width,
                height,
                dot_x,
                dot_cy,
                r,
                rgb(255, 255, 255),
                dot_alpha,
                rgb(255, 255, 255),
                dot_alpha,
                0.0,
            );
        }

        // Gliding vertical caret to the right of the last dot
        let caret_x = (start_dot_x + model.anim_dots_width + 6.5)
            .round()
            .min((inp_x + inp_w - 12) as f32) as i32;
        fill_aa_round_rect_layered(
            pixels,
            &mut alpha_map,
            width as usize,
            width,
            height,
            caret_x,
            inp_y + 7,
            2,
            inp_h - 14,
            1.0,
            rgb(255, 255, 255),
            220,
            rgb(255, 255, 255),
            220,
            0.0,
        );
    }

    // 5. Circular Right Button (? / >) at (inp_x + inp_w + 20, inp_y + inp_h / 2), radius 13
    let btn_cx = (inp_x + inp_w + 20) as f32;
    let btn_cy = (inp_y + 16) as f32;
    let is_btn_hovered = model.hovered_button == 4;
    let (btn_fill_col, btn_fill_a, btn_border_a, btn_border_th) = if is_btn_hovered {
        (rgb(58, 64, 82), 210, 240, 1.4)
    } else {
        (rgb(24, 26, 34), 155, 135, 1.2)
    };
    fill_aa_circle_layered(
        pixels,
        &mut alpha_map,
        width as usize,
        width,
        height,
        btn_cx,
        btn_cy,
        13.0,
        btn_fill_col,
        btn_fill_a,
        rgb(255, 255, 255),
        btn_border_a,
        btn_border_th,
    );

    let right_btn_glyph = if model.input_len > 0 {
        "\u{F054}" // Submit chevron >
    } else {
        "\u{F128}" // Question mark ?
    };
    let font_pill_btn = create_font("JetBrainsMono NF", -13, 700);
    draw_icon_ink_centered(
        pixels,
        &mut alpha_map,
        width as usize,
        width,
        height,
        right_btn_glyph,
        btn_cx,
        btn_cy,
        rgb(255, 255, 255),
        font_pill_btn,
    );
    let _ = DeleteObject(font_pill_btn);

    // Frosted glass hint popover background if toggled
    if model.show_hint_popover {
        let tip_w = 230;
        let tip_h = 36;
        let tip_x = (btn_cx as i32 + 18).min(width - tip_w - 16);
        let tip_y = btn_cy as i32 - tip_h / 2;
        fill_aa_round_rect_layered(
            pixels,
            &mut alpha_map,
            width as usize,
            width,
            height,
            tip_x,
            tip_y,
            tip_w,
            tip_h,
            8.0,
            rgb(24, 26, 34),
            215,
            rgb(255, 255, 255),
            180,
            1.0,
        );
    }

    // 6. Bottom Center Power Buttons: Shut Down (\u{F011}), Restart (\u{F01E}), Sleep (\u{F186})
    let cy_power = height - 86;
    let power_btns = [
        (1u8, cx - 96, "\u{F011}"),
        (2u8, cx, "\u{F01E}"),
        (3u8, cx + 96, "\u{F186}"),
    ];
    let font_pwr = create_font("JetBrainsMono NF", -18, 600);
    for &(id, btn_x, glyph) in &power_btns {
        let is_hover = model.hovered_button == id;
        let (btn_fill_col, btn_fill_a, btn_border_a, btn_border_th) = if is_hover {
            (rgb(58, 64, 82), 210, 240, 1.4)
        } else {
            (rgb(24, 26, 34), 155, 135, 1.2)
        };
        fill_aa_circle_layered(
            pixels,
            &mut alpha_map,
            width as usize,
            width,
            height,
            btn_x as f32,
            cy_power as f32,
            21.0,
            btn_fill_col,
            btn_fill_a,
            rgb(255, 255, 255),
            btn_border_a,
            btn_border_th,
        );
        draw_icon_ink_centered(
            pixels,
            &mut alpha_map,
            width as usize,
            width,
            height,
            glyph,
            btn_x as f32,
            cy_power as f32,
            rgb(255, 255, 255),
            font_pwr,
        );
    }
        let _ = DeleteObject(font_pwr);
    }

    let mut pre_gdi_rgb = vec![0u32; total_px];
    for i in 0..total_px {
        let rgb_only = pixels[i] & 0x00FF_FFFF;
        pre_gdi_rgb[i] = rgb_only;
        pixels[i] = 0x0100_0000 | rgb_only;
    }

    let old_bmp = SelectObject(mem_dc, dib_bmp);
    SetBkMode(mem_dc, TRANSPARENT);

    let font_status = create_font("SF Pro Text", -13, 600);
    let font_user = create_font("SF Pro Display", -22, 600);
    let font_input = create_font("SF Pro Text", -13, 500);
    let font_sub = create_font("SF Pro Text", -12, 500);
    let font_pwr_lbl = create_font("SF Pro Text", -12, 600);

    // Draw Top-Right Status Bar Text (y = 14..38) with drop shadows
    let old_font = SelectObject(mem_dc, font_status);
    let time_rc = RECT {
        left: time_left,
        top: 14,
        right: time_right,
        bottom: 38,
    };
    draw_text_with_shadow(
        mem_dc,
        &model.time_str,
        &time_rc,
        DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        rgb(240, 244, 252),
    );

    let battery_rc = RECT {
        left: battery_text_left,
        top: 14,
        right: battery_text_right,
        bottom: 38,
    };
    draw_text_with_shadow(
        mem_dc,
        &format!("{}%", model.battery_percent),
        &battery_rc,
        DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        rgb(240, 244, 252),
    );

    let kbd_rc = RECT {
        left: kbd_text_left,
        top: 14,
        right: kbd_text_right,
        bottom: 38,
    };
    draw_text_with_shadow(
        mem_dc,
        &model.kbd_layout_str,
        &kbd_rc,
        DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        rgb(240, 244, 252),
    );

    if model.power_menu_mode {
        // Center Power Menu Title: "System Power" (cy - 115)
        SelectObject(mem_dc, font_user);
        let title_rc = RECT {
            left: cx - 250,
            top: cy - 128,
            right: cx + 250,
            bottom: cy - 92,
        };
        draw_text_with_shadow(
            mem_dc,
            "System Power",
            &title_rc,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            rgb(245, 247, 252),
        );

        // Subtitle: "Choose a power action or press Esc to cancel" (cy - 82)
        SelectObject(mem_dc, font_sub);
        let sub_rc = RECT {
            left: cx - 250,
            top: cy - 92,
            right: cx + 250,
            bottom: cy - 68,
        };
        draw_text_with_shadow(
            mem_dc,
            "Choose a power action or press Esc to cancel",
            &sub_rc,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            rgb(180, 186, 202),
        );

        // 4 Button Labels below Circles (cy + 48..cy + 74)
        SelectObject(mem_dc, font_pwr_lbl);
        let labels = [
            (1u8, "Shut Down", rgb(243, 139, 168)),
            (2u8, "Reboot", rgb(249, 226, 175)),
            (3u8, "Sleep", rgb(205, 214, 244)),
            (5u8, "Lock", rgb(166, 227, 161)),
        ];
        for (i, &(id, text, accent)) in labels.iter().enumerate() {
            let b_cx = cx - 248 + (i as i32) * 140 + 38;
            let is_hover = model.hovered_button == id;
            let col = if is_hover { accent } else { rgb(215, 220, 232) };
            let rc = RECT {
                left: b_cx - 60,
                top: cy + 48,
                right: b_cx + 60,
                bottom: cy + 74,
            };
            draw_text_with_shadow(
                mem_dc,
                text,
                &rc,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                col,
            );
        }
    } else {
        // Draw Username below Circular Avatar at cy - 34..cy - 2 with drop shadow
        SelectObject(mem_dc, font_user);
        let user_rc = RECT {
            left: center_x - 300,
            top: cy - 34,
            right: center_x + 300,
            bottom: cy - 2,
        };
        draw_text_with_shadow(
            mem_dc,
            &model.username,
            &user_rc,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            rgb(255, 255, 255),
        );

        // Draw Password Input Capsule Text / Status / Placeholder / 144Hz Command Mode
        SelectObject(mem_dc, font_input);
        let inp_rc = RECT {
            left: inp_x + 14,
            top: inp_y,
            right: inp_x + inp_w - 14,
            bottom: inp_y + inp_h,
        };
        if model.raw_text.starts_with('>') {
            let chars: Vec<char> = model.raw_text.chars().collect();
            let mut cur_x = inp_x + 14;
            for (i, &ch) in chars.iter().enumerate() {
                let scale = model.dot_scales.get(i).copied().unwrap_or(1.0);
                let y_pop = ((1.0 - scale) * 6.0).round() as i32;
                let ch_u16 = [ch as u16];
                let mut sz = SIZE::default();
                let _ = GetTextExtentPoint32W(mem_dc, &ch_u16, &mut sz);
                let ch_w = sz.cx.max(1);
                let ch_rc = RECT {
                    left: cur_x,
                    top: inp_y + y_pop,
                    right: cur_x + ch_w,
                    bottom: inp_y + inp_h + y_pop,
                };
                draw_text_with_shadow(
                    mem_dc,
                    &ch.to_string(),
                    &ch_rc,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                    rgb(255, 255, 255),
                );
                cur_x += ch_w;
            }
            let caret_rc = RECT {
                left: (cur_x + 1).min(inp_x + inp_w - 8),
                top: inp_y + 7,
                right: (cur_x + 3).min(inp_x + inp_w - 6),
                bottom: inp_y + inp_h - 7,
            };
            draw_text_with_shadow(
                mem_dc,
                "|",
                &caret_rc,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                rgb(255, 255, 255),
            );
        } else if model.unlocking {
            draw_text_with_shadow(
                mem_dc,
                "Welcome back",
                &inp_rc,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                rgb(78, 222, 128),
            );
        } else if model.auth_failed {
            draw_text_with_shadow(
                mem_dc,
                "Incorrect password",
                &inp_rc,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                rgb(255, 105, 97),
            );
        } else if model.input_len == 0 {
            let ph = if model.setup_step == 1 {
                "Enter new password..."
            } else if model.setup_step == 2 {
                "Confirm password..."
            } else {
                "Enter Password"
            };
            draw_text_with_shadow(
                mem_dc,
                ph,
                &inp_rc,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                rgb(220, 224, 235),
            );
        }

        // Draw Subtitle below Password Input Pill with drop shadow
        SelectObject(mem_dc, font_sub);
        let sub_y = inp_y + inp_h + 14;
        let sub_rc = RECT {
            left: center_x - 240,
            top: sub_y,
            right: center_x + 240,
            bottom: sub_y + 40,
        };
        let (sub_text, sub_color) = if model.auth_failed {
            ("Incorrect password — try again", rgb(255, 105, 97))
        } else if model.unlocking {
            ("Unlocking session...", rgb(78, 222, 128))
        } else if model.setup_step == 1 {
            ("Step 1 of 2: Create a new password", rgb(235, 238, 248))
        } else if model.setup_step == 2 {
            ("Step 2 of 2: Confirm your new password", rgb(235, 238, 248))
        } else if model.raw_text.starts_with('>') {
            ("Press Enter to run power command", rgb(235, 238, 248))
        } else {
            ("Your password is required to log in", rgb(235, 238, 248))
        };
        draw_text_with_shadow(
            mem_dc,
            sub_text,
            &sub_rc,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            sub_color,
        );

        // Draw Power Button Labels below Circles (height - 56..height - 36) with drop shadows
        SelectObject(mem_dc, font_pwr_lbl);
        let pwr_labels = [
            (1u8, cx - 96, "Shut Down"),
            (2u8, cx, "Restart"),
            (3u8, cx + 96, "Sleep"),
        ];
        for &(id, btn_x, label) in &pwr_labels {
            let is_hover = model.hovered_button == id;
            let col = if is_hover { rgb(255, 255, 255) } else { rgb(220, 225, 235) };
            let rc = RECT {
                left: btn_x - 50,
                top: height - 56,
                right: btn_x + 50,
                bottom: height - 36,
            };
            draw_text_with_shadow(
                mem_dc,
                label,
                &rc,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                col,
            );
        }

        // Draw Hint Popover Text if visible
        if model.show_hint_popover {
            let tip_w = 230;
            let tip_h = 36;
            let tip_x = (btn_cx as i32 + 18).min(width - tip_w - 16);
            let tip_y = btn_cy as i32 - tip_h / 2;
            let tip_rc = RECT {
                left: tip_x + 8,
                top: tip_y,
                right: tip_x + tip_w - 8,
                bottom: tip_y + tip_h,
            };
            SelectObject(mem_dc, font_status);
            draw_text_with_shadow(
                mem_dc,
                "Hint: Windows password or PIN",
                &tip_rc,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                rgb(240, 244, 252),
            );
        }
    }

    SelectObject(mem_dc, old_font);

    for i in 0..total_px {
        let cur = pixels[i];
        let cur_rgb = cur & 0x00FF_FFFF;
        let base_rgb = pre_gdi_rgb[i];
        let base_a = alpha_map[i];

        let out_a = if cur_rgb != base_rgb || (cur >> 24) != 1 {
            let r = (cur >> 16) & 0xFF;
            let g = (cur >> 8) & 0xFF;
            let b = cur & 0xFF;
            let br = (base_rgb >> 16) & 0xFF;
            let bg_c = (base_rgb >> 8) & 0xFF;
            let bb = base_rgb & 0xFF;
            let diff = r.abs_diff(br).max(g.abs_diff(bg_c)).max(b.abs_diff(bb));
            let boost_a = (diff * 3).min(255) as u8;
            base_a.max(boost_a) as u32
        } else {
            base_a as u32
        };

        let r = ((cur >> 16) & 0xFF) * out_a / 255;
        let g = ((cur >> 8) & 0xFF) * out_a / 255;
        let b = (cur & 0xFF) * out_a / 255;
        pixels[i] = (out_a << 24) | (r << 16) | (g << 8) | b;
    }

    let pt_src = POINT { x: 0, y: 0 };
    let size = SIZE {
        cx: width,
        cy: height,
    };
    // Subtle fade as curtain nears the top of the screen during slide-up
    let curtain_alpha = if model.unlocking {
        ((1.0 - (model.unlock_progress * 0.35)) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8
    } else {
        255
    };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: curtain_alpha,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let _ = UpdateLayeredWindow(
        hwnd,
        screen_dc,
        None,
        Some(&size),
        mem_dc,
        Some(&pt_src),
        COLORREF(0),
        Some(&blend),
        ULW_ALPHA,
    );

    SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(font_status);
    let _ = DeleteObject(font_user);
    let _ = DeleteObject(font_input);
    let _ = DeleteObject(font_sub);
    let _ = DeleteObject(font_pwr_lbl);
    let _ = DeleteObject(dib_bmp);
    let _ = DeleteDC(mem_dc);
    let _ = ReleaseDC(HWND::default(), screen_dc);
}

/// Hit-tests pointer coordinates against macOS Big Sur lockscreen interactive buttons.
///
/// Hit-tests the interactive power / action buttons on the lockscreen or power menu.
///
/// Returns:
/// - In normal lock mode:
///   - `1`: Shut Down button
///   - `2`: Restart button
///   - `3`: Sleep button
///   - `4`: Circular Right Pill Button (`?` hint or `>` submit)
///   - `0`: No button hit
/// - In power menu mode:
///   - `1`: Shut Down button (large circle)
///   - `2`: Reboot button (large circle)
///   - `3`: Sleep button (large circle)
///   - `5`: Lock button (large circle)
///   - `0`: No button hit
pub fn hit_test_lock_button_with_mode(
    width: i32,
    height: i32,
    mx: i32,
    my: i32,
    power_menu_mode: bool,
) -> u8 {
    let cx = width / 2;
    let cy = height / 2;

    if power_menu_mode {
        // 4 large circular buttons: Shut Down (1), Reboot (2), Sleep (3), Lock (5)
        // btn_size = 76, btn_r = 38, gap = 64. Total width = 496. start_x = cx - 248.
        let btns = [(0, 1u8), (1, 2u8), (2, 3u8), (3, 5u8)];
        for (i, id) in btns {
            let btn_cx = cx - 248 + i * 140 + 38;
            let btn_cy = cy;
            let dx = mx - btn_cx;
            let dy = my - btn_cy;
            if dx * dx + dy * dy <= 42 * 42
                || (mx >= btn_cx - 50 && mx <= btn_cx + 50 && my >= cy + 42 && my <= cy + 76)
            {
                return id;
            }
        }
        return 0;
    }

    let cy_power = height - 86;

    // 1. Shut Down: circle at (cx - 96, cy_power) r=21, label below
    let dx1 = mx - (cx - 96);
    let dy1 = my - cy_power;
    if dx1 * dx1 + dy1 * dy1 <= 24 * 24
        || (mx >= cx - 96 - 45 && mx <= cx - 96 + 45 && my >= height - 60 && my <= height - 36)
    {
        return 1;
    }

    // 2. Restart: circle at (cx, cy_power) r=21, label below
    let dx2 = mx - cx;
    let dy2 = my - cy_power;
    if dx2 * dx2 + dy2 * dy2 <= 24 * 24
        || (mx >= cx - 45 && mx <= cx + 45 && my >= height - 60 && my <= height - 36)
    {
        return 2;
    }

    // 3. Sleep: circle at (cx + 96, cy_power) r=21, label below
    let dx3 = mx - (cx + 96);
    let dy3 = my - cy_power;
    if dx3 * dx3 + dy3 * dy3 <= 24 * 24
        || (mx >= cx + 96 - 45 && mx <= cx + 96 + 45 && my >= height - 60 && my <= height - 36)
    {
        return 3;
    }

    // 4. Pill right button (? / >): circle at (cx - 108 + 216 + 20, height / 2 + 12 + 16) r=13
    let inp_x = cx - 108;
    let inp_y = cy + 12;
    let btn_cx = inp_x + 216 + 20;
    let btn_cy = inp_y + 16;
    let dx4 = mx - btn_cx;
    let dy4 = my - btn_cy;
    if dx4 * dx4 + dy4 * dy4 <= 18 * 18 {
        return 4;
    }

    0
}

pub fn hit_test_lock_button(width: i32, height: i32, mx: i32, my: i32) -> u8 {
    hit_test_lock_button_with_mode(width, height, mx, my, false)
}
