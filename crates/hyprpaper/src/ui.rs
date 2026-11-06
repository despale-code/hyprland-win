//! Minimalist Wallpaper & Desktop Backdrop Renderer for `hyprpaper`.
//!
//! Generates a sleek Hyprland dark-slate gradient backdrop with subtle
//! geometric grid lines and an ambient center glow, cached by resolution
//! for sub-millisecond blitting when `explorer.exe` is hollowed.

use std::path::Path;
use std::sync::{Mutex, OnceLock};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetObjectW,
    ReleaseDC, SelectObject, SetBrushOrgEx, SetStretchBltMode, StretchBlt, BITMAP, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HALFTONE, HBITMAP, HDC, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    LoadImageW, IMAGE_BITMAP, LR_LOADFROMFILE,
};

#[derive(Debug, Clone)]
pub struct PaperRenderModel {
    pub width: i32,
    pub height: i32,
    pub prev_wallpaper_path: Option<String>,
    pub wallpaper_path: Option<String>,
    pub fade_progress: f32,
    pub fade_start: std::time::Instant,
    pub fade_duration_secs: f32,
}

impl PaperRenderModel {
    pub fn new(width: i32, height: i32, wallpaper_path: Option<String>) -> Self {
        Self {
            width,
            height,
            prev_wallpaper_path: None,
            wallpaper_path,
            fade_progress: 1.0,
            fade_start: std::time::Instant::now(),
            fade_duration_secs: 15.0,
        }
    }

    pub fn transition_to(&mut self, new_path: Option<String>, duration_secs: f32) -> bool {
        if self.wallpaper_path != new_path {
            self.prev_wallpaper_path = self.wallpaper_path.clone();
            self.wallpaper_path = new_path;
            self.fade_progress = 0.0;
            self.fade_start = std::time::Instant::now();
            self.fade_duration_secs = duration_secs.clamp(0.10, 120.0);
            true
        } else {
            false
        }
    }
}

#[inline]
pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

struct CachedBackdropEntry {
    width: i32,
    height: i32,
    path: Option<String>,
    pixels: Vec<u32>,
}

static BACKDROP_CACHE: OnceLock<Mutex<Vec<CachedBackdropEntry>>> = OnceLock::new();

fn get_or_load_cached_pixels(
    hdc: HDC,
    cache: &mut Vec<CachedBackdropEntry>,
    width: i32,
    height: i32,
    path: Option<&str>,
) -> usize {
    let path_string = path.map(|s| s.to_string());
    if let Some(pos) = cache
        .iter()
        .position(|e| e.width == width && e.height == height && e.path == path_string)
    {
        return pos;
    }

    let buf = path
        .and_then(|p| unsafe { rasterize_scaled_wallpaper(hdc, p, width, height) })
        .unwrap_or_else(|| generate_procedural_backdrop(width, height));

    if cache.len() >= 4 {
        cache.remove(0);
    }
    cache.push(CachedBackdropEntry {
        width,
        height,
        path: path_string,
        pixels: buf,
    });
    cache.len() - 1
}

pub unsafe fn preload_wallpaper_cache(hwnd: HWND, width: i32, height: i32, path: Option<&str>) {
    let hdc = GetDC(hwnd);
    if !hdc.is_invalid() {
        let cache_mutex = BACKDROP_CACHE.get_or_init(|| Mutex::new(Vec::new()));
        if let Ok(mut guard) = cache_mutex.lock() {
            let _ = get_or_load_cached_pixels(hdc, &mut guard, width, height, path);
        }
        let _ = ReleaseDC(hwnd, hdc);
    }
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

unsafe fn load_wallpaper_hbitmap(path: &str) -> Option<HBITMAP> {
    // 1. Try GDI+ first (supports .jpg, .jpeg, .png, .bmp, .webp/tiff)
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

    // 2. Fallback to Win32 LoadImageW (for uncompressed .bmp files or sibling wallpaper.bmp)
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

    // 3. Check if a sibling `wallpaper.bmp` exists in the same directory
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

unsafe fn rasterize_scaled_wallpaper(
    hdc: HDC,
    path: &str,
    dst_w: i32,
    dst_h: i32,
) -> Option<Vec<u32>> {
    let src_hbmp = load_wallpaper_hbitmap(path)?;

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

    // High-quality HALFTONE cover-fit scaling (preserves aspect ratio without stretching)
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

/// Generates the minimalist Hyprland dark-slate procedural backdrop:
/// - Smooth dark slate gradient: #0D0E13 top-left to #131621 bottom-right.
/// - Subtle #1B2030 geometric grid lines every 64px.
/// - Soft radial cyan/mint ambient glow in the center (~6% alpha).
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
        let is_grid_y = y % 64 == 0;

        for x in 0..width {
            let x_ratio = x as f32 / width as f32;
            let is_grid_x = x % 64 == 0;

            // 1. Base linear gradient (#0D0E13 -> #131621)
            let t = (x_ratio + y_ratio) * 0.5;
            let mut r = 13.0 + t * (19.0 - 13.0);
            let mut g = 14.0 + t * (22.0 - 14.0);
            let mut b = 19.0 + t * (33.0 - 19.0);

            // 2. Subtle geometric grid lines (#1B2030: R27, G32, B48)
            if is_grid_x || is_grid_y {
                r = r * 0.72 + 27.0 * 0.28;
                g = g * 0.72 + 32.0 * 0.28;
                b = b * 0.72 + 48.0 * 0.28;
            }

            // 3. Ambient center radial glow (#33CCFF / #00FF99 blend, ~6% intensity)
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let radial_t = (1.0 - (dist / (max_r * 0.70))).clamp(0.0, 1.0);
            let glow = radial_t * radial_t * 0.06;

            // Blend cyan (51, 204, 255) and mint (0, 255, 153)
            let glow_r = 25.0;
            let glow_g = 225.0;
            let glow_b = 200.0;

            r = (r + glow_r * glow).min(255.0);
            g = (g + glow_g * glow).min(255.0);
            b = (b + glow_b * glow).min(255.0);

            pixels[y_idx + (x as usize)] =
                0xFF00_0000 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
        }
    }

    pixels
}

pub unsafe fn render_wallpaper(hdc: HDC, model: &PaperRenderModel) {
    let width = model.width.max(640);
    let height = model.height.max(480);

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

    let cache_mutex = BACKDROP_CACHE.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut guard) = cache_mutex.lock() {
        let is_fading = model.fade_progress < 0.999
            && model.prev_wallpaper_path.is_some()
            && model.prev_wallpaper_path != model.wallpaper_path;

        if is_fading {
            let prev_idx = get_or_load_cached_pixels(
                hdc,
                &mut guard,
                width,
                height,
                model.prev_wallpaper_path.as_deref(),
            );
            let curr_idx = get_or_load_cached_pixels(
                hdc,
                &mut guard,
                width,
                height,
                model.wallpaper_path.as_deref(),
            );

            let t = model.fade_progress.clamp(0.0, 1.0);
            let smooth_t = t * t * (3.0 - 2.0 * t);
            let alpha = (smooth_t * 256.0).round().clamp(0.0, 256.0) as u32;
            let inv_alpha = 256u32.saturating_sub(alpha);

            let prev_slice = &guard[prev_idx].pixels;
            let curr_slice = &guard[curr_idx].pixels;
            let total = pixels.len().min(prev_slice.len()).min(curr_slice.len());

            for i in 0..total {
                let p0 = prev_slice[i];
                let p1 = curr_slice[i];

                let r0 = (p0 >> 16) & 0xFF;
                let g0 = (p0 >> 8) & 0xFF;
                let b0 = p0 & 0xFF;

                let r1 = (p1 >> 16) & 0xFF;
                let g1 = (p1 >> 8) & 0xFF;
                let b1 = p1 & 0xFF;

                let r = (r0 * inv_alpha + r1 * alpha) >> 8;
                let g = (g0 * inv_alpha + g1 * alpha) >> 8;
                let b = (b0 * inv_alpha + b1 * alpha) >> 8;

                pixels[i] = 0xFF00_0000 | (r << 16) | (g << 8) | b;
            }
        } else {
            let curr_idx = get_or_load_cached_pixels(
                hdc,
                &mut guard,
                width,
                height,
                model.wallpaper_path.as_deref(),
            );
            let curr_slice = &guard[curr_idx].pixels;
            let len = pixels.len().min(curr_slice.len());
            pixels[..len].copy_from_slice(&curr_slice[..len]);
        }
    }

    let old_bmp = SelectObject(mem_dc, dib_bmp);
    let _ = BitBlt(hdc, 0, 0, width, height, mem_dc, 0, 0, SRCCOPY);

    SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(dib_bmp);
    let _ = DeleteDC(mem_dc);
}
