//! Visual layout, double-buffered DIBSection rendering, Liquid Glass dimensions badge,
//! selection border, and crosshairs for `hyprshot`.

use std::sync::OnceLock;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    AddFontResourceExW, BitBlt, CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC,
    DeleteObject, DrawTextW, GetTextExtentPoint32W, SelectObject, SetBkMode, SetTextColor,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLEARTYPE_NATURAL_QUALITY, DIB_RGB_COLORS, DT_CENTER,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, FR_PRIVATE, HDC, HFONT, SRCCOPY, TRANSPARENT,
};

#[inline]
pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

pub const COLOR_BORDER: u32 = 0xFFE4_E4E7;      // #E4E4E7 1.5px / 2px crisp selection border
pub const COLOR_CROSSHAIR: u32 = 0x80A1_A1AA;    // #A1A1AA semi-transparent crosshair line
pub const COLOR_BADGE_BG: COLORREF = rgb(22, 23, 27); // #16171B Liquid Glass pill badge
pub const COLOR_BADGE_BORDER: COLORREF = rgb(82, 82, 91); // #52525B subtle glass border
pub const COLOR_BADGE_TEXT: COLORREF = rgb(244, 244, 245); // #F4F4F5 crisp text

// ----------------------------------------------------------------------------
// ShotOverlayState
// ----------------------------------------------------------------------------

pub struct ShotOverlayState {
    pub screen_w: i32,
    pub screen_h: i32,
    pub screen_pixels: Vec<u32>,
    pub dimmed_pixels: Vec<u32>,
    pub dragging: bool,
    pub finished: bool,
    pub cancelled: bool,
    pub start_pt: (i32, i32),
    pub cur_pt: (i32, i32),
    pub hover_pt: (i32, i32),
}

impl ShotOverlayState {
    pub fn new(screen_w: i32, screen_h: i32, screen_pixels: Vec<u32>) -> Self {
        let mut dimmed_pixels = Vec::with_capacity(screen_pixels.len());
        for &p in &screen_pixels {
            let b = (((p & 0xFF) as f32) * 0.45).round() as u32;
            let g = ((((p >> 8) & 0xFF) as f32) * 0.45).round() as u32;
            let r = ((((p >> 16) & 0xFF) as f32) * 0.45).round() as u32;
            dimmed_pixels.push(0xFF00_0000 | (r << 16) | (g << 8) | b);
        }

        Self {
            screen_w,
            screen_h,
            screen_pixels,
            dimmed_pixels,
            dragging: false,
            finished: false,
            cancelled: false,
            start_pt: (0, 0),
            cur_pt: (0, 0),
            hover_pt: (0, 0),
        }
    }

    pub fn selected_crop(&self) -> Option<(i32, i32, i32, i32)> {
        if self.cancelled {
            return None;
        }
        let x = self.start_pt.0.min(self.cur_pt.0);
        let y = self.start_pt.1.min(self.cur_pt.1);
        let w = (self.start_pt.0 - self.cur_pt.0).abs();
        let h = (self.start_pt.1 - self.cur_pt.1).abs();
        if w >= 4 && h >= 4 {
            Some((x, y, w, h))
        } else {
            None
        }
    }
}

// ----------------------------------------------------------------------------
// Private Font Loading
// ----------------------------------------------------------------------------

static FONTS_LOADED: OnceLock<()> = OnceLock::new();

pub unsafe fn ensure_jetbrains_font_loaded() {
    FONTS_LOADED.get_or_init(|| {
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
            "JetBrainsMonoNerdFont-Regular.ttf",
            "JetBrainsMonoNerdFont-Medium.ttf",
            "JetBrainsMonoNerdFont-SemiBold.ttf",
            "JetBrainsMonoNerdFont-Bold.ttf",
            "JetBrainsMonoNerdFontPropo-Regular.ttf",
            "JetBrainsMonoNerdFontPropo-Bold.ttf",
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
    let normalized = if primary_name.eq_ignore_ascii_case("JetBrainsMono Nerd Font")
        || primary_name.eq_ignore_ascii_case("JetBrains Mono")
        || primary_name.is_empty()
    {
        "JetBrainsMono NF"
    } else {
        primary_name
    };
    let candidates = [
        normalized,
        "JetBrainsMono NF",
        "JetBrainsMono NFP",
        "JetBrainsMono NFM",
        "Segoe UI Variable Display",
        "Segoe UI",
        "Consolas",
    ];
    for name in candidates {
        if name.is_empty() {
            continue;
        }
        let name_u16: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let hfont = create_font(PCWSTR(name_u16.as_ptr()), height, weight);
        if !hfont.is_invalid() && !hfont.0.is_null() {
            return hfont;
        }
    }
    create_font(PCWSTR::null(), height, weight)
}

// ----------------------------------------------------------------------------
// SDF Rounded Rect with Liquid Glass Shading
// ----------------------------------------------------------------------------

#[inline]
fn alpha_blend_bgra(bg: u32, fg_rgb: COLORREF, alpha_u8: u8) -> u32 {
    if alpha_u8 == 0 {
        return bg;
    }
    let fg_r = (fg_rgb.0 & 0xFF) as u32;
    let fg_g = ((fg_rgb.0 >> 8) & 0xFF) as u32;
    let fg_b = ((fg_rgb.0 >> 16) & 0xFF) as u32;

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

fn fill_aa_round_rect(
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
    if w <= 0 || h <= 0 || opacity <= 0.001 {
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
            let dist = qx.max(qy).min(0.0) + (qx_clamped * qx_clamped + qy_clamped * qy_clamped).sqrt() - r;

            let outer_cov = (0.5 - dist).clamp(0.0, 1.0);
            if outer_cov <= 0.001 {
                continue;
            }

            let idx = row_offset + (px as usize);
            if idx >= pixels.len() {
                continue;
            }

            let bg = pixels[idx];

            let final_pixel = if border_thickness > 0.0 {
                let inner_dist = dist + border_thickness;
                let inner_cov = (0.5 - inner_dist).clamp(0.0, 1.0);
                let border_cov = (outer_cov - inner_cov).max(0.0);

                let fill_a = (inner_cov * opacity * 230.0).clamp(0.0, 255.0) as u8;
                let border_a = (border_cov * opacity * 255.0).clamp(0.0, 255.0) as u8;

                let with_border = alpha_blend_bgra(bg, border_rgb, border_a);
                alpha_blend_bgra(with_border, fill_rgb, fill_a)
            } else {
                let a = (outer_cov * opacity * 230.0).clamp(0.0, 255.0) as u8;
                alpha_blend_bgra(bg, fill_rgb, a)
            };

            pixels[idx] = final_pixel;
        }
    }
}

// ----------------------------------------------------------------------------
// Double-Buffered Shot Overlay Rendering
// ----------------------------------------------------------------------------

pub unsafe fn render_shot_overlay(hdc: HDC, state: &ShotOverlayState) {
    let screen_w = state.screen_w.max(1);
    let screen_h = state.screen_h.max(1);

    ensure_jetbrains_font_loaded();

    let mem_dc = CreateCompatibleDC(hdc);
    if mem_dc.is_invalid() {
        return;
    }

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: screen_w,
            biHeight: -screen_h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (screen_w * screen_h * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
    let Ok(dib_bmp) = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
        let _ = DeleteDC(mem_dc);
        return;
    };

    let pixels: &mut [u32] = std::slice::from_raw_parts_mut(bits as *mut u32, (screen_w * screen_h) as usize);

    // 1. Copy precomputed dimmed desktop pixels (45% brightness)
    if state.dimmed_pixels.len() == pixels.len() {
        pixels.copy_from_slice(&state.dimmed_pixels);
    } else {
        pixels.fill(0xFF10_1012);
    }

    let crop_opt = state.selected_crop();
    let is_active_selection = (state.dragging || state.finished) && crop_opt.is_some();

    // 2. Un-dim the selected crop rectangle (100% bright/clear)
    if let Some((rx, ry, rw, rh)) = crop_opt {
        if is_active_selection && state.screen_pixels.len() == pixels.len() {
            let x0 = rx.clamp(0, screen_w - 1);
            let y0 = ry.clamp(0, screen_h - 1);
            let x1 = (rx + rw).clamp(x0, screen_w);
            let y1 = (ry + rh).clamp(y0, screen_h);

            for y in y0..y1 {
                let row_start = (y as usize) * (screen_w as usize);
                let start_idx = row_start + (x0 as usize);
                let end_idx = row_start + (x1 as usize);
                pixels[start_idx..end_idx].copy_from_slice(&state.screen_pixels[start_idx..end_idx]);
            }

            // 3. Draw 1.5px / 2px crisp border in #E4E4E7 around the selection
            let border_color = COLOR_BORDER;
            let b_thick = 2; // 2px crisp stroke

            // Top line
            for by in y0..(y0 + b_thick).min(y1) {
                let row_start = (by as usize) * (screen_w as usize);
                for bx in x0..x1 {
                    pixels[row_start + (bx as usize)] = border_color;
                }
            }
            // Bottom line
            for by in (y1 - b_thick).max(y0)..y1 {
                let row_start = (by as usize) * (screen_w as usize);
                for bx in x0..x1 {
                    pixels[row_start + (bx as usize)] = border_color;
                }
            }
            // Left line
            for by in y0..y1 {
                let row_start = (by as usize) * (screen_w as usize);
                for bx in x0..(x0 + b_thick).min(x1) {
                    pixels[row_start + (bx as usize)] = border_color;
                }
            }
            // Right line
            for by in y0..y1 {
                let row_start = (by as usize) * (screen_w as usize);
                for bx in (x1 - b_thick).max(x0)..x1 {
                    pixels[row_start + (bx as usize)] = border_color;
                }
            }

            // 4. Liquid Glass dimensions badge pill ("{rw} × {rh}")
            let badge_w = 110;
            let badge_h = 26;
            let badge_gap = 8;
            let mut badge_x = rx + (rw - badge_w) / 2;
            let mut badge_y = ry + rh + badge_gap;

            // Flip badge above selection if it would go off-screen
            if badge_y + badge_h > screen_h - 10 {
                badge_y = ry - badge_h - badge_gap;
            }
            if badge_y < 10 {
                badge_y = ry + 10;
            }
            badge_x = badge_x.clamp(10, screen_w - badge_w - 10);

            fill_aa_round_rect(
                pixels,
                screen_w as usize,
                screen_w,
                screen_h,
                badge_x,
                badge_y,
                badge_w,
                badge_h,
                13.0, // true pill radius
                COLOR_BADGE_BG,
                COLOR_BADGE_BORDER,
                1.0,
                0.95,
            );
        }
    }

    // 5. Draw subtle crosshair lines across the screen intersecting at hover_pt
    if !state.finished {
        let (hx, hy) = state.hover_pt;
        if hx >= 0 && hx < screen_w && hy >= 0 && hy < screen_h {
            // Horizontal line
            let h_row = (hy as usize) * (screen_w as usize);
            for x in 0..screen_w {
                let idx = h_row + (x as usize);
                let bg = pixels[idx];
                pixels[idx] = alpha_blend_bgra(bg, rgb(161, 161, 170), 100);
            }
            // Vertical line
            for y in 0..screen_h {
                let idx = (y as usize) * (screen_w as usize) + (hx as usize);
                let bg = pixels[idx];
                pixels[idx] = alpha_blend_bgra(bg, rgb(161, 161, 170), 100);
            }
        }
    }

    // 6. Draw dimension badge text via GDI
    let old_bmp = SelectObject(mem_dc, dib_bmp);
    SetBkMode(mem_dc, TRANSPARENT);

    if let Some((rx, ry, rw, rh)) = crop_opt {
        if is_active_selection {
            let font = create_font_with_fallback("JetBrainsMono Nerd Font", -12, 600);
            let old_font = SelectObject(mem_dc, font);
            SetTextColor(mem_dc, COLOR_BADGE_TEXT);

            let badge_w = 110;
            let badge_h = 26;
            let badge_gap = 8;
            let mut badge_x = rx + (rw - badge_w) / 2;
            let mut badge_y = ry + rh + badge_gap;

            if badge_y + badge_h > screen_h - 10 {
                badge_y = ry - badge_h - badge_gap;
            }
            if badge_y < 10 {
                badge_y = ry + 10;
            }
            badge_x = badge_x.clamp(10, screen_w - badge_w - 10);

            let mut badge_rc = RECT {
                left: badge_x,
                top: badge_y,
                right: badge_x + badge_w,
                bottom: badge_y + badge_h,
            };
            let dim_str = format!("{rw} × {rh}");
            let mut dim_u16: Vec<u16> = dim_str.encode_utf16().chain(Some(0)).collect();

            DrawTextW(
                mem_dc,
                &mut dim_u16,
                &mut badge_rc,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );

            SelectObject(mem_dc, old_font);
            let _ = DeleteObject(font);
        }
    }

    // 7. Blit finished frame to window HDC
    let _ = BitBlt(hdc, 0, 0, screen_w, screen_h, mem_dc, 0, 0, SRCCOPY);

    // 8. Clean up GDI handles
    SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(dib_bmp);
    let _ = DeleteDC(mem_dc);
}
