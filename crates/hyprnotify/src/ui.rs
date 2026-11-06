//! Visual layout, styling, Apple SF Pro typography, per-pixel SDF anti-aliased
//! toast cards, sub-pixel progress bar, and DComp overlay cutout for `hyprnotify`.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    AddFontResourceExW, CombineRgn, CreateCompatibleDC, CreateDIBSection, CreateFontIndirectW,
    CreateFontW, CreateRectRgn, DeleteDC, DeleteObject, DrawTextW, GetDC, GetObjectW, ReleaseDC,
    SelectObject, SetBkMode, SetTextColor, SetWindowRgn, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, CLEARTYPE_NATURAL_QUALITY, DIB_RGB_COLORS,
    DRAW_TEXT_FORMAT, DT_CALCRECT, DT_CENTER, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, DT_WORDBREAK, FR_PRIVATE, HBITMAP, HDC, HFONT, HRGN, LOGFONTW, RGN_DIFF,
    TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, UpdateLayeredWindow, ULW_ALPHA,
};

// ----------------------------------------------------------------------------
// Public Constants & Types Required by hyprnotify
// ----------------------------------------------------------------------------

pub const TOAST_WIDTH: i32 = 348;
pub const TOAST_MIN_HEIGHT: i32 = 72;
pub const TOAST_CARD_HEIGHT: i32 = 72;
pub const TOAST_GAP: i32 = 10;

#[derive(Debug, Clone)]
pub struct ToastCardView {
    pub summary: String,
    pub body: String,
    pub app_name: String,
    pub urgency: u8,
    pub progress: f32, // 1.0 -> 0.0 remaining life
    pub card_h: i32,
    pub offset_x: f32, // Horizontal slide offset in px (0.0 = resting, >0.0 = sliding from/to right)
    pub offset_y: f32, // Animated vertical stack position in px
    pub opacity: f32,  // 0.0 -> 1.0 card + text opacity
    pub hovered: bool,
}

impl Default for ToastCardView {
    fn default() -> Self {
        Self {
            summary: String::new(),
            body: String::new(),
            app_name: String::new(),
            urgency: 1,
            progress: 1.0,
            card_h: TOAST_MIN_HEIGHT,
            offset_x: 0.0,
            offset_y: 0.0,
            opacity: 1.0,
            hovered: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NotifyRenderModel {
    pub width: i32,
    pub height: i32,
    pub cards: Vec<ToastCardView>,
}

// ----------------------------------------------------------------------------
// Color Palette & Utilities
// ----------------------------------------------------------------------------

#[inline]
pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

pub const COLOR_BG: COLORREF = rgb(18, 19, 24);              // #121318 100% opaque dark charcoal background
pub const COLOR_BG_U32: u32 = 0xFF12_1318;
pub const COLOR_GAP_U32: u32 = 0x0000_0000;                 // Transparent background between cards
pub const COLOR_CARD_BORDER: COLORREF = rgb(42, 44, 54);     // #2A2C36 subtle 1.0px border for stacked separation
pub const COLOR_BORDER: COLORREF = rgb(42, 44, 54);          // #2A2C36
pub const COLOR_TRACK_BG: COLORREF = rgb(38, 40, 50);        // #262832 progress bar track
pub const COLOR_TEXT_WHITE: COLORREF = rgb(244, 244, 246);  // #F4F4F6
pub const COLOR_TEXT_MUTED: COLORREF = rgb(188, 190, 202);  // #BCBECA SF Pro secondary body text

// Urgent / Normal Accents (#B4BEFE lavender-blue, #F38BA8 coral)
pub const COLOR_URGENCY_LOW: COLORREF = rgb(180, 190, 254);     // 0: #B4BEFE lavender-blue
pub const COLOR_URGENCY_NORMAL: COLORREF = rgb(180, 190, 254);  // 1: #B4BEFE lavender-blue
pub const COLOR_URGENCY_CRIT: COLORREF = rgb(243, 139, 168);    // 2: #F38BA8 Coral (critical alerts)

#[inline]
pub fn get_urgency_color(urgency: u8) -> COLORREF {
    if urgency >= 2 {
        COLOR_URGENCY_CRIT
    } else {
        COLOR_URGENCY_NORMAL
    }
}

// ----------------------------------------------------------------------------
// Typography Helpers (Apple SF Pro & Nerd Font Loaders)
// ----------------------------------------------------------------------------

static FONTS_LOADED: OnceLock<()> = OnceLock::new();

pub unsafe fn ensure_fonts_loaded() {
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

        let explicit_font_files = [
            "SF-Pro-Display-Regular.otf",
            "SF-Pro-Display-Medium.otf",
            "SF-Pro-Display-Semibold.otf",
            "SF-Pro-Display-Bold.otf",
            "SF-Pro-Text-Regular.otf",
            "SF-Pro-Text-Medium.otf",
            "SF-Pro-Text-Semibold.otf",
            "SF-Pro-Text-Bold.otf",
            "SFProDisplay-Regular.ttf",
            "SFProDisplay-Medium.ttf",
            "SFProDisplay-Semibold.ttf",
            "SFProDisplay-Bold.ttf",
            "SFProText-Regular.ttf",
            "SFProText-Medium.ttf",
            "SFProText-Semibold.ttf",
            "SFProText-Bold.ttf",
            "JetBrainsMonoNerdFont-Regular.ttf",
            "JetBrainsMonoNerdFont-Medium.ttf",
            "JetBrainsMonoNerdFont-SemiBold.ttf",
            "JetBrainsMonoNerdFont-Bold.ttf",
            "JetBrainsMonoNerdFontPropo-Regular.ttf",
            "JetBrainsMonoNerdFontPropo-Bold.ttf",
        ];

        for dir_str in &font_dirs {
            let path = std::path::Path::new(dir_str);
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    let file_path = entry.path();
                    if let Some(name) = file_path.file_name().and_then(|n| n.to_str()) {
                        let lower = name.to_ascii_lowercase();
                        let is_target = (lower.starts_with("sf-pro")
                            || lower.starts_with("sfpro")
                            || lower.starts_with("jetbrainsmono"))
                            && (lower.ends_with(".otf") || lower.ends_with(".ttf"));
                        if is_target {
                            let wide: Vec<u16> = file_path
                                .to_string_lossy()
                                .encode_utf16()
                                .chain(Some(0))
                                .collect();
                            let _ = AddFontResourceExW(PCWSTR(wide.as_ptr()), FR_PRIVATE, None);
                        }
                    }
                }
            }
            for file in &explicit_font_files {
                let full_path = format!("{dir_str}\\{file}");
                if std::path::Path::new(&full_path).is_file() {
                    let wide: Vec<u16> = full_path.encode_utf16().chain(Some(0)).collect();
                    let _ = AddFontResourceExW(PCWSTR(wide.as_ptr()), FR_PRIVATE, None);
                }
            }
        }
    });
}

pub unsafe fn ensure_departure_font_loaded() {
    ensure_fonts_loaded();
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
        0, // OUT_DEFAULT_PRECIS
        0, // CLIP_DEFAULT_PRECIS
        CLEARTYPE_NATURAL_QUALITY,
        0, // DEFAULT_PITCH | FF_DONTCARE
        name,
    )
}

unsafe fn create_font_with_fallback(primary_name: &str, height: i32, weight: i32) -> HFONT {
    let candidates: Vec<&str> = if primary_name.eq_ignore_ascii_case("SF Pro Display") {
        vec![
            "SF Pro Display",
            "SF Pro Text",
            "Segoe UI Variable Display",
            "Segoe UI",
            "JetBrainsMono NF",
        ]
    } else if primary_name.eq_ignore_ascii_case("SF Pro Text") {
        vec![
            "SF Pro Text",
            "SF Pro Display",
            "Segoe UI Variable Text",
            "Segoe UI",
            "JetBrainsMono NF",
        ]
    } else if primary_name.contains("JetBrainsMono") {
        vec![
            "JetBrainsMono NF",
            "JetBrainsMono NFP",
            "JetBrainsMono Nerd Font",
            "Segoe UI",
        ]
    } else {
        vec![primary_name, "Segoe UI Variable Text", "Segoe UI"]
    };

    for name in candidates {
        let name_u16: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let hfont = create_font(PCWSTR(name_u16.as_ptr()), height, weight);
        if !hfont.is_invalid() && !hfont.0.is_null() {
            return hfont;
        }
    }
    create_font(PCWSTR::null(), height, weight)
}

pub unsafe fn measure_toast_card_height(summary: &str, body: &str) -> i32 {
    ensure_fonts_loaded();
    let screen_dc = GetDC(HWND::default());
    let mem_dc = CreateCompatibleDC(screen_dc);
    let _ = ReleaseDC(HWND::default(), screen_dc);
    if mem_dc.is_invalid() {
        return TOAST_MIN_HEIGHT;
    }

    let font_sum = create_font_with_fallback("SF Pro Text", -15, 600);
    let font_body = create_font_with_fallback("SF Pro Text", -14, 400);

    let text_col_w = TOAST_WIDTH - 72 - 18; // 258px

    let clean_summary = summary.trim_start_matches(|c: char| {
        c == '\u{F001}' || c == '\u{F071}' || c == '\u{F0F3}' || c == ' '
    });

    let old_font = SelectObject(mem_dc, font_sum);
    let mut sum_rc = RECT {
        left: 0,
        top: 0,
        right: text_col_w,
        bottom: 0,
    };
    let mut sum_utf16: Vec<u16> = clean_summary.encode_utf16().collect();
    let sum_h = if !sum_utf16.is_empty() {
        DrawTextW(
            mem_dc,
            &mut sum_utf16,
            &mut sum_rc,
            DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
        );
        (sum_rc.bottom - sum_rc.top).max(18)
    } else {
        0
    };

    SelectObject(mem_dc, font_body);
    let mut body_rc = RECT {
        left: 0,
        top: 0,
        right: text_col_w,
        bottom: 0,
    };
    let mut body_utf16: Vec<u16> = body.encode_utf16().collect();
    let body_h = if !body_utf16.is_empty() {
        DrawTextW(
            mem_dc,
            &mut body_utf16,
            &mut body_rc,
            DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
        );
        (body_rc.bottom - body_rc.top).max(16)
    } else {
        0
    };

    SelectObject(mem_dc, old_font);
    let _ = DeleteObject(font_sum);
    let _ = DeleteObject(font_body);
    let _ = DeleteDC(mem_dc);

    (16 + sum_h + 4 + body_h + 16).max(TOAST_MIN_HEIGHT)
}

// ----------------------------------------------------------------------------
// Premultiplied ARGB Source-Over Alpha Blending
// ----------------------------------------------------------------------------

#[inline]
pub fn alpha_blend_premul_bgra(dst: u32, fg_rgb: COLORREF, alpha_u8: u8) -> u32 {
    if alpha_u8 == 0 {
        return dst;
    }
    let fg_r = (fg_rgb.0 & 0xFF) as u32;
    let fg_g = ((fg_rgb.0 >> 8) & 0xFF) as u32;
    let fg_b = ((fg_rgb.0 >> 16) & 0xFF) as u32;
    let src_a = alpha_u8 as u32;

    let src_r = (fg_r * src_a + 127) / 255;
    let src_g = (fg_g * src_a + 127) / 255;
    let src_b = (fg_b * src_a + 127) / 255;

    if dst == 0 {
        return (src_a << 24) | (src_r << 16) | (src_g << 8) | src_b;
    }

    let dst_a = (dst >> 24) & 0xFF;
    let dst_r = (dst >> 16) & 0xFF;
    let dst_g = (dst >> 8) & 0xFF;
    let dst_b = dst & 0xFF;

    let inv_a = 255 - src_a;
    let out_a = (src_a + (dst_a * inv_a + 127) / 255).min(255);
    let out_r = (src_r + (dst_r * inv_a + 127) / 255).min(out_a);
    let out_g = (src_g + (dst_g * inv_a + 127) / 255).min(out_a);
    let out_b = (src_b + (dst_b * inv_a + 127) / 255).min(out_a);

    (out_a << 24) | (out_r << 16) | (out_g << 8) | out_b
}

#[inline]
pub fn alpha_blend_bgra(dst: u32, fg_rgb: COLORREF, alpha_u8: u8) -> u32 {
    alpha_blend_premul_bgra(dst, fg_rgb, alpha_u8)
}

// ----------------------------------------------------------------------------
// Signed Distance Field (SDF) Sub-Pixel Anti-Aliasing Rasterizer
// ----------------------------------------------------------------------------

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
                pixels[idx] = alpha_blend_premul_bgra(pixels[idx], fill_rgb, solid_fill_a);
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

                let with_border = alpha_blend_premul_bgra(bg, border_rgb, border_a);
                alpha_blend_premul_bgra(with_border, fill_rgb, fill_a)
            } else {
                let a = (outer_cov * opacity * 255.0).clamp(0.0, 255.0) as u8;
                alpha_blend_premul_bgra(bg, fill_rgb, a)
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
// Offscreen Scratch & 2x Supersampled Font / Icon Rasterizers
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

unsafe fn get_text_scratch(min_w: i32, min_h: i32) -> &'static Mutex<TextScratchSurface> {
    TEXT_SCRATCH.get_or_init(|| {
        let w = min_w.max(1024);
        let h = min_h.max(1024);
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

    let face_len = lf
        .lfFaceName
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(lf.lfFaceName.len());
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

unsafe fn draw_text_aa(
    pixels: &mut [u32],
    stride: usize,
    max_w: i32,
    max_h: i32,
    text: &str,
    rect: &RECT,
    color: COLORREF,
    font: HFONT,
    format: DRAW_TEXT_FORMAT,
    opacity: f32,
) {
    if text.is_empty() || opacity <= 0.001 || rect.right <= rect.left || rect.bottom <= rect.top {
        return;
    }
    let rw = (rect.right - rect.left).max(1);
    let rh = (rect.bottom - rect.top).max(1);

    let scratch_mutex = get_text_scratch(rw * 2, rh * 2);
    let guard = match scratch_mutex.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let s_w = guard.w;
    let s_h = guard.h;
    let s_bits = guard.bits;
    let s_dc = guard.dc;
    if s_bits.is_null() {
        return;
    }

    let scratch_slice = std::slice::from_raw_parts_mut(s_bits, (s_w * s_h) as usize);

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
    let mut actual_format = format;
    if (format.0 & DT_WORDBREAK.0) != 0 {
        actual_format = DRAW_TEXT_FORMAT(actual_format.0 & !DT_SINGLELINE.0);
    }
    DrawTextW(s_dc, &mut utf16, &mut scratch_rc, actual_format);
    SelectObject(s_dc, old_font);

    // Isotropic 2x2 box-filter downsampling with linear coverage and premultiplied ARGB blend
    for iy in 0..render_h {
        let dy = rect.top + iy;
        if dy < 0 || dy >= max_h {
            continue;
        }
        let dst_row = (dy as usize) * stride;

        let sy0 = (iy * 2) as usize;
        let sy1 = sy0 + 1;
        let s_row0 = sy0 * (s_w as usize);
        let s_row1 = sy1 * (s_w as usize);

        for ix in 0..render_w {
            let dx = rect.left + ix;
            if dx < 0 || dx >= max_w {
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

            let cov_u8 = ((raw_cov * opacity * 255.0).round() as u32).min(255) as u8;

            if cov_u8 > 0 {
                let dst_idx = dst_row + (dx as usize);
                if dst_idx < pixels.len() {
                    pixels[dst_idx] = alpha_blend_premul_bgra(pixels[dst_idx], color, cov_u8);
                }
            }
        }
    }
}

#[derive(Clone)]
struct CachedGlyph {
    ink_w: i32,
    ink_h: i32,
    mask: Vec<u8>,
    opt_offset_x: f32,
}

static GLYPH_INK_CACHE: OnceLock<Mutex<HashMap<String, CachedGlyph>>> = OnceLock::new();

unsafe fn draw_icon_ink_centered(
    pixels: &mut [u32],
    stride: usize,
    max_w: i32,
    max_h: i32,
    glyph: &str,
    target_rect: (i32, i32, i32, i32),
    color: COLORREF,
    font: HFONT,
    opacity: f32,
) {
    let (rx, ry, rw, rh) = target_rect;
    if rw <= 0 || rh <= 0 || glyph.is_empty() || opacity <= 0.001 {
        return;
    }

    let mut lf = LOGFONTW::default();
    GetObjectW(
        font,
        std::mem::size_of::<LOGFONTW>() as i32,
        Some(&mut lf as *mut _ as *mut core::ffi::c_void),
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

            let eff_cov = ((cov as f32) * opacity).round().clamp(0.0, 255.0) as u8;
            if eff_cov > 0 {
                let idx = dst_row + (dx as usize);
                if idx < pixels.len() {
                    pixels[idx] = alpha_blend_premul_bgra(pixels[idx], color, eff_cov);
                }
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Hyprland-Win DComp Overlay Window Cutout Region
// ----------------------------------------------------------------------------

pub unsafe fn set_overlay_notify_cutout(
    screen_w: i32,
    screen_h: i32,
    notify_rect: Option<(i32, i32, i32, i32)>,
) {
    let classes = ["HyprlandWinNativeDCompOverlay\0", "HyprlandWinNativeDCompOverlay\0"];
    let mut ov_hwnd = HWND::default();
    for cls_str in classes {
        let cls: Vec<u16> = cls_str.encode_utf16().collect();
        if let Ok(h) = FindWindowW(PCWSTR(cls.as_ptr()), PCWSTR::null()) {
            if !h.0.is_null() {
                ov_hwnd = h;
                break;
            }
        }
    }
    if !ov_hwnd.0.is_null() {
            if let Some((x, y, w, h)) = notify_rect {
                let rgn_full = CreateRectRgn(0, 0, screen_w, screen_h);
                let rgn_cut = CreateRectRgn(x, y, x + w, y + h);
                let rgn_dest = CreateRectRgn(0, 0, 0, 0);
                CombineRgn(rgn_dest, rgn_full, rgn_cut, RGN_DIFF);
                let _ = DeleteObject(rgn_full);
                let _ = DeleteObject(rgn_cut);
                SetWindowRgn(ov_hwnd, rgn_dest, true);
            } else {
                SetWindowRgn(ov_hwnd, HRGN::default(), true);
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Double-Buffered DIBSection & SDF Toast Notifications Painting
// ----------------------------------------------------------------------------

pub unsafe fn render_notifications(hwnd: HWND, hdc: HDC, model: &NotifyRenderModel) {
    let width = model.width.max(TOAST_WIDTH);
    let height = model.height.max(TOAST_MIN_HEIGHT);

    if model.cards.is_empty() {
        return;
    }

    ensure_fonts_loaded();

    let mem_dc = CreateCompatibleDC(hdc);
    if mem_dc.is_invalid() {
        return;
    }

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // negative for top-down bitmap
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (width * height * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
    let Ok(dib_bmp) = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
        let _ = DeleteDC(mem_dc);
        return;
    };

    let pixels: &mut [u32] =
        std::slice::from_raw_parts_mut(bits as *mut u32, (width * height) as usize);

    // 1. Fill base buffer with 100% transparent pixels so the gap between stacked cards
    // and outer rounded corners are completely transparent!
    pixels.fill(0x0000_0000);

    let card_w = TOAST_WIDTH;

    let font_icon_large = create_font_with_fallback("JetBrainsMono NFP", -22, 400);
    let font_summary = create_font_with_fallback("SF Pro Text", -15, 600);
    let font_body = create_font_with_fallback("SF Pro Text", -14, 400);
    let font_close = create_font_with_fallback("JetBrainsMono NF", -12, 400);

    // 2. Render Cards, App Badges, Typography, and Top-Left Hover Close Button
    for card in &model.cards {
        let card_h = card.card_h.max(TOAST_MIN_HEIGHT);
        let card_x = card.offset_x.round() as i32;
        let card_y = card.offset_y.round() as i32;
        let card_alpha = card.opacity.clamp(0.0, 1.0);
        if card_alpha <= 0.005 || card_x >= width || card_y >= height || card_y + card_h <= 0 {
            continue;
        }

        // Toast Card Background: 100% opaque interior (1.0 * card_alpha), 22.0px corner radius
        // matching the inward monitor edge borders, with 1.0px border rgb(42, 44, 54)
        fill_aa_round_rect_f32(
            pixels,
            width as usize,
            width,
            height,
            card.offset_x,
            card.offset_y,
            card_w as f32,
            card_h as f32,
            22.0,
            COLOR_BG,
            COLOR_CARD_BORDER,
            1.0,
            1.0 * card_alpha,
        );

        // Determine Nerd Font Icon & Clean Title
        let icon_glyph = if card.summary.starts_with('\u{F001}')
            || card.summary.contains("Now Playing")
        {
            "\u{F001}"
        } else if card.urgency >= 2 {
            "\u{F071}"
        } else if card.app_name.to_lowercase().contains("mail") {
            "\u{F0E0}"
        } else if card.app_name.to_lowercase().contains("slack")
            || card.app_name.to_lowercase().contains("discord")
            || card.app_name.to_lowercase().contains("chat")
        {
            "\u{F075}"
        } else {
            "\u{F0F3}"
        };

        let clean_summary = card.summary.trim_start_matches(|c: char| {
            c == '\u{F001}' || c == '\u{F071}' || c == '\u{F0F3}' || c == ' '
        });

        // Left App Icon Badge: 40x40px rounded square at x = card_x + 16, y = card_y + (card.card_h - 40) / 2, radius 10.0px
        let badge_x = card.offset_x + 16.0;
        let badge_y = card.offset_y + (((card_h - 40) / 2).max(12) as f32);
        let (tile_bg, tile_border) = if card.urgency >= 2 {
            (rgb(215, 65, 85), rgb(243, 139, 168))
        } else if card.summary.contains("Now Playing") || card.summary.starts_with('\u{F001}') {
            (rgb(40, 160, 95), rgb(80, 200, 130))
        } else {
            (rgb(42, 118, 246), rgb(68, 140, 255))
        };

        fill_aa_round_rect_f32(
            pixels,
            width as usize,
            width,
            height,
            badge_x,
            badge_y,
            40.0,
            40.0,
            10.0,
            tile_bg,
            tile_border,
            1.0,
            card_alpha,
        );

        draw_icon_ink_centered(
            pixels,
            width as usize,
            width,
            height,
            icon_glyph,
            (badge_x.round() as i32, badge_y.round() as i32, 40, 40),
            COLOR_TEXT_WHITE,
            font_icon_large,
            card_alpha,
        );

        // Right Text Column (left = card_x + 68, right = card_x + card_w - 18)
        let text_col_w = card_w - 68 - 18;

        let old_dc_font = SelectObject(mem_dc, font_summary);
        let mut calc_sum_rc = RECT {
            left: 0,
            top: 0,
            right: text_col_w,
            bottom: 0,
        };
        let mut sum_utf16: Vec<u16> = clean_summary.encode_utf16().collect();
        let sum_h = if !sum_utf16.is_empty() {
            DrawTextW(
                mem_dc,
                &mut sum_utf16,
                &mut calc_sum_rc,
                DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
            );
            (calc_sum_rc.bottom - calc_sum_rc.top).max(18)
        } else {
            0
        };

        SelectObject(mem_dc, font_body);
        let mut calc_body_rc = RECT {
            left: 0,
            top: 0,
            right: text_col_w,
            bottom: 0,
        };
        let mut body_utf16: Vec<u16> = card.body.encode_utf16().collect();
        let body_h = if !body_utf16.is_empty() {
            DrawTextW(
                mem_dc,
                &mut body_utf16,
                &mut calc_body_rc,
                DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
            );
            (calc_body_rc.bottom - calc_body_rc.top).max(16)
        } else {
            0
        };
        SelectObject(mem_dc, old_dc_font);

        let total_text_h = if body_h > 0 { sum_h + 3 + body_h } else { sum_h };
        let text_top = card_y + (((card_h - total_text_h) / 2).max(12));

        let sum_rc = RECT {
            left: card_x + 68,
            top: text_top,
            right: (card_x + card_w - 18).min(width),
            bottom: text_top + sum_h + 4,
        };
        draw_text_aa(
            pixels,
            width as usize,
            width,
            height,
            clean_summary,
            &sum_rc,
            COLOR_TEXT_WHITE,
            font_summary,
            DT_LEFT | DT_WORDBREAK | DT_NOPREFIX,
            card_alpha,
        );

        if !card.body.is_empty() {
            let body_rc = RECT {
                left: card_x + 68,
                top: text_top + sum_h + 3,
                right: (card_x + card_w - 18).min(width),
                bottom: card_y + card_h - 10,
            };
            draw_text_aa(
                pixels,
                width as usize,
                width,
                height,
                &card.body,
                &body_rc,
                COLOR_TEXT_MUTED,
                font_body,
                DT_LEFT | DT_WORDBREAK | DT_NOPREFIX,
                card_alpha,
            );
        }

        // Top-Left Hover Close (×) Circle Button
        if card.hovered {
            let close_x = card.offset_x + 4.0;
            let close_y = card.offset_y + 4.0;
            fill_aa_round_rect_f32(
                pixels,
                width as usize,
                width,
                height,
                close_x,
                close_y,
                20.0,
                20.0,
                10.0,
                rgb(44, 46, 56),
                rgb(72, 74, 88),
                1.0,
                card_alpha,
            );
            draw_icon_ink_centered(
                pixels,
                width as usize,
                width,
                height,
                "\u{F00D}",
                (close_x.round() as i32, close_y.round() as i32, 20, 20),
                rgb(220, 222, 230),
                font_close,
                card_alpha,
            );
        }
    }

    // 3. Present directly to layered window using hardware premultiplied ARGB blending
    let old_bmp = SelectObject(mem_dc, dib_bmp);
    let pt_src = POINT { x: 0, y: 0 };
    let win_size = SIZE {
        cx: width,
        cy: height,
    };
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
        Some(&win_size),
        mem_dc,
        Some(&pt_src),
        COLORREF(0),
        Some(&blend),
        ULW_ALPHA,
    );

    // 4. Clean up GDI handles
    SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(font_icon_large);
    let _ = DeleteObject(font_summary);
    let _ = DeleteObject(font_body);
    let _ = DeleteObject(font_close);
    let _ = DeleteObject(dib_bmp);
    let _ = DeleteDC(mem_dc);
}
