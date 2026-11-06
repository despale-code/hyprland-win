//! Visual layout, styling, Departure Mono typography, per-pixel SDF anti-aliased rounded rects,
//! application icon extraction, and DComp overlay cutout for `hyprlauncher` (Rofi Hyprland `.rasi` style).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use windows::core::{s, PCWSTR};
use windows::Win32::Foundation::{BOOL, COLORREF, HWND, LPARAM, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AddFontResourceExW, BitBlt, CombineRgn, CreateCompatibleDC, CreateDIBSection, CreateFontW,
    CreateRectRgn, DeleteDC, DeleteObject, DrawTextW, GetDC, GetDIBits, GetObjectW,
    GetTextExtentPoint32W, ReleaseDC, SelectObject, SetBkMode, SetTextColor, SetWindowRgn,
    AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, FR_PRIVATE,
    HBITMAP, HDC, HFONT, HRGN, LOGFONTW, RGN_DIFF, SRCCOPY, TRANSPARENT,
};
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{
    ExtractIconExW, SHGetFileInfoW, SHFILEINFOW, SHGFI_ICONLOCATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, DrawIconEx, FindWindowW, GetClassLongPtrW, GetIconInfo, GetWindowThreadProcessId,
    LoadIconW, SendMessageTimeoutW, UpdateLayeredWindow, DI_NORMAL, GCLP_HICON, GCLP_HICONSM,
    HICON, ICONINFO, ICON_BIG, ICON_SMALL, ICON_SMALL2, IDI_APPLICATION, SMTO_ABORTIFHUNG,
    ULW_ALPHA, WM_GETICON,
};

// ----------------------------------------------------------------------------
// Public Constants & Data Types Required by hyprlauncher
// ----------------------------------------------------------------------------

pub const LAUNCHER_WIDTH: i32 = 620;
pub const LAUNCHER_HEIGHT: i32 = 516;
pub const MAX_VISIBLE_ITEMS: usize = 8;
pub const LIST_START_Y: f32 = 14.0;
pub const ROW_HEIGHT: i32 = 52;
pub const ROW_STEP: f32 = 54.0;
pub const ROW_X_PAD: i32 = 36;

#[derive(Debug, Clone)]
pub struct LauncherDisplayRow {
    pub title: String,
    pub subtitle: String,
    pub badge: String,
    pub is_window: bool,
    pub icon_hwnd: u64,
    pub icon_path: String,
}

#[derive(Debug, Clone)]
pub struct LauncherRenderModel {
    pub width: i32,
    pub height: i32,
    pub query: String,
    pub query_selected: bool,
    pub rows: Vec<LauncherDisplayRow>,
    pub selected_index: usize,
    pub hovered_index: Option<usize>,
    pub total_matches: usize,
    pub open_progress: f32,
    pub closing: bool,
    pub anim_selected_y: f32,
    pub anim_height: f32,
    pub row_anim: [f32; MAX_VISIBLE_ITEMS],
    pub typing_pulse: f32,
}

impl Default for LauncherRenderModel {
    fn default() -> Self {
        Self {
            width: LAUNCHER_WIDTH,
            height: LAUNCHER_HEIGHT,
            query: String::new(),
            query_selected: false,
            rows: Vec::new(),
            selected_index: 0,
            hovered_index: None,
            total_matches: 0,
            open_progress: 0.0,
            closing: false,
            anim_selected_y: LIST_START_Y,
            anim_height: LAUNCHER_HEIGHT as f32,
            row_anim: [0.0; MAX_VISIBLE_ITEMS],
            typing_pulse: 0.0,
        }
    }
}

pub fn target_card_height(_row_count: usize) -> f32 {
    LAUNCHER_HEIGHT as f32
}

pub fn reset_open_animation(model: &mut LauncherRenderModel) {
    model.closing = false;
    model.open_progress = 0.0;
    model.anim_selected_y = LIST_START_Y + (model.selected_index as f32) * ROW_STEP;
    model.anim_height = LAUNCHER_HEIGHT as f32;
    model.typing_pulse = 1.0;
    for (idx, val) in model.row_anim.iter_mut().enumerate() {
        *val = -(idx as f32) * 0.11;
    }
}

pub fn trigger_list_cascade(model: &mut LauncherRenderModel) {
    model.typing_pulse = 1.0;
    for (idx, val) in model.row_anim.iter_mut().enumerate() {
        *val = 0.28 - (idx as f32) * 0.075;
    }
}

/// Steps all 144Hz animations (open/close fade+slide, gliding selection pill, staggered row cascade,
/// dynamic card height, and searchbar typing pulse).
/// Returns `(needs_repaint, hide_completed)`.
pub fn step_launcher_animations(model: &mut LauncherRenderModel, dt: f32) -> (bool, bool) {
    let dt = dt.clamp(0.001, 0.050);
    let mut dirty = false;
    let mut hide_completed = false;

    // 1. Appearing / Disappearing progress
    if model.closing {
        if model.open_progress > 0.0 {
            model.open_progress = (model.open_progress - dt * 9.5).max(0.0);
            dirty = true;
            if model.open_progress <= 0.005 {
                model.open_progress = 0.0;
                hide_completed = true;
            }
        } else {
            hide_completed = true;
        }
    } else if model.open_progress < 1.0 {
        model.open_progress = (model.open_progress + dt * 7.5).min(1.0);
        dirty = true;
    }

    // 2. Smooth gliding selection highlight pill (exponential spring)
    let visible = model.rows.len().min(MAX_VISIBLE_ITEMS);
    if visible > 0 {
        let sel_idx = model.selected_index.min(visible - 1);
        let target_y = LIST_START_Y + (sel_idx as f32) * ROW_STEP;
        let diff_y = target_y - model.anim_selected_y;
        if diff_y.abs() > 0.15 {
            let factor = 1.0 - (-26.0 * dt).exp();
            model.anim_selected_y += diff_y * factor;
            dirty = true;
        } else if (model.anim_selected_y - target_y).abs() > 0.001 {
            model.anim_selected_y = target_y;
            dirty = true;
        }
    }

    // 3. Keep card height fixed to LAUNCHER_HEIGHT so Acrylic blur geometry matches exactly
    model.anim_height = LAUNCHER_HEIGHT as f32;

    // 4. Staggered item list row cascade
    for idx in 0..MAX_VISIBLE_ITEMS {
        if model.row_anim[idx] < 1.0 {
            model.row_anim[idx] = (model.row_anim[idx] + dt * 7.8).min(1.0);
            dirty = true;
        }
    }

    // 5. Typing border pulse decay
    if model.typing_pulse > 0.0 {
        model.typing_pulse = (model.typing_pulse - dt * 4.5).max(0.0);
        dirty = true;
    }

    (dirty, hide_completed)
}

// ----------------------------------------------------------------------------
// Color Palette (Minimalist Matte Zinc & Charcoal Theme)
// ----------------------------------------------------------------------------

#[inline]
pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

pub const COLOR_BG0: COLORREF = rgb(24, 27, 34);             // #181B22 Raycast dark charcoal slate background
pub const COLOR_BG0_U32: u32 = 0xFF18_1B22;
pub const COLOR_BORDER: COLORREF = rgb(52, 56, 68);          // #343844 gentle 1px border
pub const COLOR_HEADER_DIVIDER: COLORREF = rgb(52, 56, 68);  // 0x0034_3844 header 1px line
pub const COLOR_FOOTER_DIVIDER: COLORREF = rgb(48, 52, 64);  // 0x0030_3440 footer 1px line
pub const COLOR_INDICATOR_FILL: COLORREF = rgb(40, 34, 30);  // 0x0028_221E
pub const COLOR_INDICATOR_BORDER: COLORREF = rgb(72, 62, 54);// 0x0048_3E36
pub const COLOR_INPUT_BG: COLORREF = rgb(39, 40, 46);        // #27282E
pub const COLOR_INPUT_BORDER: COLORREF = rgb(58, 60, 68);    // #3A3C44
pub const COLOR_ROW_SELECTED_BG: COLORREF = rgb(41, 58, 92); // 0x0029_3A5C Raycast selection pill
pub const COLOR_ROW_SELECTED_BORDER: COLORREF = rgb(70, 98, 152); // 0x0046_6298 selection border
pub const COLOR_ROW_HOVER_BG: COLORREF = rgb(36, 40, 50);    // gentle hover row
pub const COLOR_PROMPT: COLORREF = rgb(148, 150, 160);       // #9496A0
pub const COLOR_QUERY: COLORREF = rgb(238, 240, 246);        // 0x00EE_F0F6 crisp query text
pub const COLOR_CURSOR: COLORREF = rgb(180, 184, 196);       // cursor
pub const COLOR_PLACEHOLDER: COLORREF = rgb(116, 118, 128);  // muted placeholder text
pub const COLOR_TITLE_SELECTED: COLORREF = rgb(240, 242, 247); // 0x00F0_F2F7
pub const COLOR_TITLE_NORMAL: COLORREF = rgb(224, 226, 234);   // 0x00E0_E2EA
pub const COLOR_FG0: COLORREF = rgb(224, 226, 234);
pub const COLOR_FG_MUTED: COLORREF = rgb(138, 140, 150);
pub const COLOR_BADGE_BG: COLORREF = rgb(40, 44, 54);
pub const COLOR_BADGE_BORDER: COLORREF = rgb(66, 72, 88);
pub const COLOR_BADGE_TEXT: COLORREF = rgb(166, 170, 184);
pub const COLOR_KEYCAP_BG: COLORREF = rgb(40, 44, 54);         // 0x0028_2C36
pub const COLOR_KEYCAP_BORDER: COLORREF = rgb(66, 72, 88);     // 0x0042_4858
pub const COLOR_KEYCAP_TEXT: COLORREF = rgb(166, 170, 184);    // 0x00A6_AAB8
pub const COLOR_SELECT_KEYCAP_BG: COLORREF = rgb(46, 68, 112); // 0x002E_4470
pub const COLOR_SELECT_KEYCAP_BORDER: COLORREF = rgb(78, 110, 168); // 0x004E_6EA8
pub const COLOR_SELECT_KEYCAP_TEXT: COLORREF = rgb(255, 255, 255);
pub const COLOR_ICON_TILE_BG: COLORREF = rgb(46, 48, 56);
pub const COLOR_ICON_TILE_BORDER: COLORREF = rgb(64, 66, 76);
pub const COLOR_ACCENT_BLUE: COLORREF = rgb(94, 158, 248);
pub const COLOR_BG1: COLORREF = rgb(39, 40, 46);
pub const COLOR_BG2: COLORREF = rgb(66, 68, 76);
pub const COLOR_ACCENT: COLORREF = rgb(94, 158, 248);
pub const COLOR_URGENT: COLORREF = rgb(235, 106, 102);       // #EB6A66 macOS soft coral red

// ----------------------------------------------------------------------------
// Typography Helpers (Departure Mono)
// ----------------------------------------------------------------------------

static APPLE_FONTS_LOADED: OnceLock<()> = OnceLock::new();

pub unsafe fn ensure_apple_fonts_loaded() {
    APPLE_FONTS_LOADED.get_or_init(|| {
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

#[inline]
pub unsafe fn ensure_departure_font_loaded() {
    ensure_apple_fonts_loaded();
}

#[inline]
pub fn is_nerd_font_glyph(c: char) -> bool {
    let u = c as u32;
    (0xE000..=0xF8FF).contains(&u) || (0xF0000..=0xFFFFD).contains(&u)
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
        7, // OUT_TT_ONLY_PRECIS (force TrueType/OpenType outline vector rendering)
        0, // CLIP_DEFAULT_PRECIS
        ANTIALIASED_QUALITY.0 as u32,
        0, // DEFAULT_PITCH | FF_DONTCARE
        name,
    )
}

pub unsafe fn create_icon_font(height: i32, weight: i32) -> HFONT {
    let icon_candidates = [
        "JetBrainsMono NF",
        "JetBrainsMono NFP",
        "JetBrainsMono NFM",
        "Segoe Fluent Icons",
        "Segoe MDL2 Assets",
    ];
    for name in icon_candidates {
        let name_u16: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let hfont = create_font(PCWSTR(name_u16.as_ptr()), height, weight);
        if !hfont.is_invalid() && !hfont.0.is_null() {
            return hfont;
        }
    }
    create_font(PCWSTR::null(), height, weight)
}

pub unsafe fn create_apple_font(height: i32, weight: i32) -> HFONT {
    let primary = if height.abs() >= 17 { "Segoe UI Variable Display" } else { "Segoe UI Variable Text" };
    let candidates = [
        primary,
        "Segoe UI Variable Text",
        "Segoe UI Variable Display",
        "Segoe UI",
        "SF Pro Text",
        "SF Pro Display",
        "Arial",
    ];
    for name in candidates {
        let name_u16: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let hfont = create_font(PCWSTR(name_u16.as_ptr()), height, weight);
        if !hfont.is_invalid() && !hfont.0.is_null() {
            return hfont;
        }
    }
    create_font(PCWSTR::null(), height, weight)
}

static LAUNCHER_FONT_2X_CACHE: OnceLock<Mutex<HashMap<(i32, i32, String), usize>>> = OnceLock::new();

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

    let face_len = lf.lfFaceName.iter().position(|&c| c == 0).unwrap_or(lf.lfFaceName.len());
    let face_name = String::from_utf16_lossy(&lf.lfFaceName[..face_len]);
    let key = (lf.lfHeight, lf.lfWeight, face_name.clone());

    let cache = LAUNCHER_FONT_2X_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
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

unsafe fn create_font_with_fallback(primary_name: &str, height: i32, weight: i32) -> HFONT {
    if primary_name.eq_ignore_ascii_case("JetBrainsMono NF")
        || primary_name.eq_ignore_ascii_case("JetBrainsMono Nerd Font")
        || primary_name.eq_ignore_ascii_case("JetBrains Mono")
    {
        create_icon_font(height, weight)
    } else {
        create_apple_font(height, weight)
    }
}

#[derive(Copy, Clone)]
struct CachedLauncherFonts {
    query: usize,
    title: usize,
    keycap: usize,
    footer: usize,
    badge: usize,
    sub: usize,
}

static CACHED_FONTS: OnceLock<CachedLauncherFonts> = OnceLock::new();

unsafe fn get_cached_fonts() -> (HFONT, HFONT, HFONT, HFONT, HFONT, HFONT) {
    let f = CACHED_FONTS.get_or_init(|| CachedLauncherFonts {
        query: create_apple_font(-20, 500).0 as usize,
        title: create_apple_font(-18, 500).0 as usize,
        keycap: create_apple_font(-15, 600).0 as usize,
        footer: create_apple_font(-15, 500).0 as usize,
        badge: create_apple_font(-19, 400).0 as usize,
        sub: create_apple_font(-16, 400).0 as usize,
    });
    (
        HFONT(f.query as *mut core::ffi::c_void),
        HFONT(f.title as *mut core::ffi::c_void),
        HFONT(f.keycap as *mut core::ffi::c_void),
        HFONT(f.footer as *mut core::ffi::c_void),
        HFONT(f.badge as *mut core::ffi::c_void),
        HFONT(f.sub as *mut core::ffi::c_void),
    )
}

fn split_icon_chunks(text: &str) -> Vec<(bool, String)> {
    let mut chunks: Vec<(bool, String)> = Vec::new();
    let mut cur_icon = false;
    let mut cur_str = String::new();

    for c in text.chars() {
        let is_icon = is_nerd_font_glyph(c);
        if cur_str.is_empty() {
            cur_icon = is_icon;
            cur_str.push(c);
        } else if is_icon == cur_icon {
            cur_str.push(c);
        } else {
            chunks.push((cur_icon, cur_str));
            cur_icon = is_icon;
            cur_str = c.to_string();
        }
    }
    if !cur_str.is_empty() {
        chunks.push((cur_icon, cur_str));
    }
    chunks
}

struct ScratchTextRenderer {
    dc: HDC,
    bmp: HBITMAP,
    width: i32,
    height: i32,
    bits: *mut u32,
}

unsafe impl Send for ScratchTextRenderer {}

impl ScratchTextRenderer {
    unsafe fn new(width: i32, height: i32) -> Option<Self> {
        let screen_dc = GetDC(HWND::default());
        if screen_dc.is_invalid() {
            return None;
        }
        let dc = CreateCompatibleDC(screen_dc);
        if dc.is_invalid() {
            let _ = ReleaseDC(HWND::default(), screen_dc);
            return None;
        }

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: (width * height * 4) as u32,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
        let Ok(bmp) = CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(dc);
            let _ = ReleaseDC(HWND::default(), screen_dc);
            return None;
        };

        let _ = ReleaseDC(HWND::default(), screen_dc);
        SelectObject(dc, bmp);
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, rgb(255, 255, 255));

        Some(Self {
            dc,
            bmp,
            width,
            height,
            bits: bits as *mut u32,
        })
    }
}

static TEXT_SCRATCH: OnceLock<Mutex<ScratchTextRenderer>> = OnceLock::new();

#[inline]
pub fn blend_premul_pixel(dst: u32, src_rgb: COLORREF, src_alpha: f32) -> u32 {
    let sa = src_alpha.clamp(0.0, 1.0);
    if sa <= 0.001 {
        return dst;
    }

    let sr = ((src_rgb.0 & 0xFF) as f32 / 255.0) * sa;
    let sg = (((src_rgb.0 >> 8) & 0xFF) as f32 / 255.0) * sa;
    let sb = (((src_rgb.0 >> 16) & 0xFF) as f32 / 255.0) * sa;

    let da = ((dst >> 24) & 0xFF) as f32 / 255.0;
    let dr = ((dst >> 16) & 0xFF) as f32 / 255.0;
    let dg = ((dst >> 8) & 0xFF) as f32 / 255.0;
    let db = (dst & 0xFF) as f32 / 255.0;

    let inv_sa = 1.0 - sa;
    let out_a = sa + da * inv_sa;
    let out_r = sr + dr * inv_sa;
    let out_g = sg + dg * inv_sa;
    let out_b = sb + db * inv_sa;

    (((out_a * 255.0).round() as u32) << 24)
        | (((out_r * 255.0).round() as u32) << 16)
        | (((out_g * 255.0).round() as u32) << 8)
        | ((out_b * 255.0).round() as u32)
}

#[inline]
pub fn blend_premul_argb(dst: u32, src_premul: u32, alpha_mul: f32) -> u32 {
    let mul = alpha_mul.clamp(0.0, 1.0);
    let src_a = ((src_premul >> 24) & 0xFF) as f32 / 255.0 * mul;
    if src_a <= 0.001 {
        return dst;
    }

    let sr = (((src_premul >> 16) & 0xFF) as f32 / 255.0) * mul;
    let sg = (((src_premul >> 8) & 0xFF) as f32 / 255.0) * mul;
    let sb = ((src_premul & 0xFF) as f32 / 255.0) * mul;

    let da = ((dst >> 24) & 0xFF) as f32 / 255.0;
    let dr = ((dst >> 16) & 0xFF) as f32 / 255.0;
    let dg = ((dst >> 8) & 0xFF) as f32 / 255.0;
    let db = (dst & 0xFF) as f32 / 255.0;

    let inv_sa = 1.0 - src_a;
    let out_a = src_a + da * inv_sa;
    let out_r = sr + dr * inv_sa;
    let out_g = sg + dg * inv_sa;
    let out_b = sb + db * inv_sa;

    (((out_a * 255.0).round() as u32) << 24)
        | (((out_r * 255.0).round() as u32) << 16)
        | (((out_g * 255.0).round() as u32) << 8)
        | ((out_b * 255.0).round() as u32)
}

unsafe fn draw_smooth_text(
    pixels: &mut [u32],
    stride: usize,
    max_w: i32,
    max_h: i32,
    text: &str,
    rect: &RECT,
    color: COLORREF,
    font: HFONT,
    flags: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
) {
    if text.is_empty()
        || rect.left >= max_w
        || rect.top >= max_h
        || rect.right <= 0
        || rect.bottom <= 0
    {
        return;
    }

    let scratch_mutex = TEXT_SCRATCH.get_or_init(|| {
        Mutex::new(
            ScratchTextRenderer::new(2048, 256).expect("Failed to initialize text scratch DIB"),
        )
    });

    let Ok(guard) = scratch_mutex.lock() else {
        return;
    };
    let s = &*guard;
    if s.bits.is_null() {
        return;
    }

    let rw = (rect.right - rect.left).clamp(1, (s.width / 2).max(1));
    let rh = (rect.bottom - rect.top).clamp(1, (s.height / 2).max(1));

    let scratch_slice = std::slice::from_raw_parts_mut(s.bits, (s.width * s.height) as usize);

    let hfont_2x = get_2x_font(font);
    let sub_w = rw * 2;
    let sub_h = rh * 2;

    for y in 0..sub_h {
        let row_start = (y * s.width) as usize;
        scratch_slice[row_start..row_start + sub_w as usize].fill(0);
    }

    let old_font = SelectObject(s.dc, hfont_2x);
    SetTextColor(s.dc, rgb(255, 255, 255));
    SetBkMode(s.dc, TRANSPARENT);

    let mut local_rc = RECT {
        left: 0,
        top: 0,
        right: sub_w,
        bottom: sub_h,
    };
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    DrawTextW(s.dc, &mut utf16, &mut local_rc, flags);
    SelectObject(s.dc, old_font);

    // Isotropic 2x2 box-filter downsampling with gamma 0.75
    for y in 0..rh {
        let py = rect.top + y;
        if py < 0 || py >= max_h {
            continue;
        }
        let dst_row = (py as usize) * stride;

        let sy0 = (y * 2) as usize;
        let sy1 = sy0 + 1;
        let src_row0 = sy0 * (s.width as usize);
        let src_row1 = sy1 * (s.width as usize);

        for x in 0..rw {
            let px = rect.left + x;
            if px < 0 || px >= max_w {
                continue;
            }

            let sx0 = (x * 2) as usize;
            let sx1 = sx0 + 1;

            let p00 = scratch_slice[src_row0 + sx0];
            let p01 = scratch_slice[src_row0 + sx1];
            let p10 = scratch_slice[src_row1 + sx0];
            let p11 = scratch_slice[src_row1 + sx1];

            if (p00 | p01 | p10 | p11) == 0 {
                continue;
            }

            let c00 = (p00 & 0xFF).max((p00 >> 8) & 0xFF).max((p00 >> 16) & 0xFF);
            let c01 = (p01 & 0xFF).max((p01 >> 8) & 0xFF).max((p01 >> 16) & 0xFF);
            let c10 = (p10 & 0xFF).max((p10 >> 8) & 0xFF).max((p10 >> 16) & 0xFF);
            let c11 = (p11 & 0xFF).max((p11 >> 8) & 0xFF).max((p11 >> 16) & 0xFF);

            let sum = (c00 + c01 + c10 + c11) as f32;
            let raw_cov = sum / (4.0 * 255.0);

            let alpha = raw_cov;
            let idx = dst_row + px as usize;
            pixels[idx] = blend_premul_pixel(pixels[idx], color, alpha);
        }
    }
}

unsafe fn draw_smooth_mixed_text(
    pixels: &mut [u32],
    stride: usize,
    max_w: i32,
    max_h: i32,
    text: &str,
    rect: &RECT,
    color: COLORREF,
    font: HFONT,
) {
    if !text.chars().any(is_nerd_font_glyph) {
        draw_smooth_text(
            pixels,
            stride,
            max_w,
            max_h,
            text,
            rect,
            color,
            font,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        return;
    }

    let mut lf = LOGFONTW::default();
    GetObjectW(
        font,
        std::mem::size_of::<LOGFONTW>() as i32,
        Some(&mut lf as *mut LOGFONTW as *mut core::ffi::c_void),
    );
    let icon_font = create_icon_font(lf.lfHeight, 400);

    let chunks = split_icon_chunks(text);
    let mut cur_x = rect.left;

    let scratch_mutex = TEXT_SCRATCH.get_or_init(|| {
        Mutex::new(
            ScratchTextRenderer::new(2048, 256).expect("Failed to initialize text scratch DIB"),
        )
    });

    for (is_icon, chunk) in chunks {
        let f = if is_icon { icon_font } else { font };
        let mut utf16: Vec<u16> = chunk.encode_utf16().collect();
        let mut sz = SIZE::default();

        if let Ok(guard) = scratch_mutex.lock() {
            let old_f = SelectObject(guard.dc, f);
            let _ = GetTextExtentPoint32W(guard.dc, &utf16, &mut sz);
            SelectObject(guard.dc, old_f);
        }

        let chunk_rc = RECT {
            left: cur_x,
            top: rect.top,
            right: (cur_x + sz.cx).min(rect.right),
            bottom: rect.bottom,
        };

        draw_smooth_text(
            pixels,
            stride,
            max_w,
            max_h,
            &chunk,
            &chunk_rc,
            color,
            f,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );

        cur_x += sz.cx;
        if cur_x >= rect.right {
            break;
        }
    }

    let _ = DeleteObject(icon_font);
}

unsafe fn draw_mixed_text_left(
    hdc: HDC,
    text: &str,
    rect: &mut RECT,
    font: HFONT,
) {
    if !text.chars().any(is_nerd_font_glyph) {
        let old_f = SelectObject(hdc, font);
        let mut u16_buf: Vec<u16> = text.encode_utf16().collect();
        DrawTextW(hdc, &mut u16_buf, rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX);
        SelectObject(hdc, old_f);
        return;
    }

    let mut lf = LOGFONTW::default();
    GetObjectW(
        font,
        std::mem::size_of::<LOGFONTW>() as i32,
        Some(&mut lf as *mut LOGFONTW as *mut core::ffi::c_void),
    );
    let icon_font = create_icon_font(lf.lfHeight, 400);

    let chunks = split_icon_chunks(text);
    let mut cur_x = rect.left;

    for (is_icon, chunk) in chunks {
        let f = if is_icon { icon_font } else { font };
        let old_f = SelectObject(hdc, f);
        let mut utf16: Vec<u16> = chunk.encode_utf16().collect();
        let mut sz = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &utf16, &mut sz);
        let mut chunk_rc = RECT {
            left: cur_x,
            top: rect.top,
            right: (cur_x + sz.cx).min(rect.right),
            bottom: rect.bottom,
        };
        DrawTextW(hdc, &mut utf16, &mut chunk_rc, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
        SelectObject(hdc, old_f);
        cur_x += sz.cx;
        if cur_x >= rect.right {
            break;
        }
    }

    let _ = DeleteObject(icon_font);
}

// ----------------------------------------------------------------------------
// Icon Extraction & Caching
// ----------------------------------------------------------------------------

static HWND_ICON_CACHE: OnceLock<Mutex<HashMap<u64, Option<usize>>>> = OnceLock::new();
static PATH_ICON_CACHE: OnceLock<Mutex<HashMap<String, Option<usize>>>> = OnceLock::new();

pub unsafe fn get_path_hicon(path: &str) -> Option<HICON> {
    if path.is_empty() {
        return None;
    }
    let cache = PATH_ICON_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(&cached) = guard.get(path) {
            return cached.map(|ptr| HICON(ptr as *mut core::ffi::c_void));
        }
    }

    let path_u16: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    let mut large_icon = HICON::default();
    let mut small_icon = HICON::default();
    let mut icon_result: Option<HICON> = None;

    // Step 1: Never call SHGetFileInfoW with SHGFI_ICON on .lnk (composites shortcut arrow).
    // Instead, directly call ExtractIconExW on path_str to extract raw un-composited PE/ICO icon.
    let count = ExtractIconExW(
        PCWSTR(path_u16.as_ptr()),
        0,
        Some(&mut large_icon),
        Some(&mut small_icon),
        1,
    );

    if count > 0 && (!large_icon.is_invalid() || !small_icon.is_invalid()) {
        if !large_icon.is_invalid() && !large_icon.0.is_null() {
            if !small_icon.is_invalid() && !small_icon.0.is_null() {
                let _ = DestroyIcon(small_icon);
            }
            icon_result = Some(large_icon);
        } else if !small_icon.is_invalid() && !small_icon.0.is_null() {
            icon_result = Some(small_icon);
        }
    }

    // Step 2: If ExtractIconExW returns 0, query icon location without drawing shortcut arrow
    if icon_result.is_none() {
        let mut shfi = SHFILEINFOW::default();
        let res = SHGetFileInfoW(
            PCWSTR(path_u16.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut shfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICONLOCATION,
        );
        if res != 0 {
            let mut loc_large = HICON::default();
            let mut loc_small = HICON::default();
            let loc_count = ExtractIconExW(
                PCWSTR(shfi.szDisplayName.as_ptr()),
                shfi.iIcon,
                Some(&mut loc_large),
                Some(&mut loc_small),
                1,
            );
            if loc_count > 0 && (!loc_large.is_invalid() || !loc_small.is_invalid()) {
                if !loc_large.is_invalid() && !loc_large.0.is_null() {
                    if !loc_small.is_invalid() && !loc_small.0.is_null() {
                        let _ = DestroyIcon(loc_small);
                    }
                    icon_result = Some(loc_large);
                } else if !loc_small.is_invalid() && !loc_small.0.is_null() {
                    icon_result = Some(loc_small);
                }
            }
        }
    }

    // Step 3: Fallback to standard application icon
    if icon_result.is_none() {
        if let Ok(app_icon) = LoadIconW(None, IDI_APPLICATION) {
            if !app_icon.is_invalid() && !app_icon.0.is_null() {
                icon_result = Some(app_icon);
            }
        }
    }

    let ptr_val = icon_result.map(|h| h.0 as usize);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(path.to_string(), ptr_val);
    }

    icon_result
}

pub unsafe fn get_window_hicon(hwnd_val: u64) -> Option<HICON> {
    if hwnd_val == 0 {
        return None;
    }
    let cache = HWND_ICON_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(&cached) = guard.get(&hwnd_val) {
            return cached.map(|ptr| HICON(ptr as *mut core::ffi::c_void));
        }
    }

    let h = HWND(hwnd_val as *mut core::ffi::c_void);
    let mut icon_result: usize = 0;

    // 1. WM_GETICON (ICON_SMALL2)
    let _ = SendMessageTimeoutW(
        h,
        WM_GETICON,
        WPARAM(ICON_SMALL2 as usize),
        LPARAM(0),
        SMTO_ABORTIFHUNG,
        25,
        Some(&mut icon_result),
    );

    // 2. WM_GETICON (ICON_SMALL)
    if icon_result == 0 {
        let _ = SendMessageTimeoutW(
            h,
            WM_GETICON,
            WPARAM(ICON_SMALL as usize),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            25,
            Some(&mut icon_result),
        );
    }

    // 3. WM_GETICON (ICON_BIG)
    if icon_result == 0 {
        let _ = SendMessageTimeoutW(
            h,
            WM_GETICON,
            WPARAM(ICON_BIG as usize),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            25,
            Some(&mut icon_result),
        );
    }

    // 4. Class Small Icon (GCLP_HICONSM)
    if icon_result == 0 {
        icon_result = GetClassLongPtrW(h, GCLP_HICONSM);
    }

    // 5. Class Big Icon (GCLP_HICON)
    if icon_result == 0 {
        icon_result = GetClassLongPtrW(h, GCLP_HICON);
    }

    // 6. Executable Path Fallback via Process Query
    if icon_result == 0 {
        let mut pid: u32 = 0;
        let _ = GetWindowThreadProcessId(h, Some(&mut pid));
        if pid != 0 {
            if let Ok(proc_handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                let mut exe_buf = [0u16; 1024];
                let mut exe_len = exe_buf.len() as u32;
                if QueryFullProcessImageNameW(
                    proc_handle,
                    PROCESS_NAME_WIN32,
                    windows::core::PWSTR(exe_buf.as_mut_ptr()),
                    &mut exe_len,
                )
                .is_ok()
                {
                    let mut large = HICON::default();
                    let mut small = HICON::default();
                    let count = ExtractIconExW(
                        PCWSTR(exe_buf.as_ptr()),
                        0,
                        Some(&mut large),
                        Some(&mut small),
                        1,
                    );
                    if count > 0 && !large.is_invalid() && !large.0.is_null() {
                        if !small.is_invalid() && !small.0.is_null() {
                            let _ = DestroyIcon(small);
                        }
                        icon_result = large.0 as usize;
                    } else if count > 0 && !small.is_invalid() && !small.0.is_null() {
                        icon_result = small.0 as usize;
                    }
                }
                let _ = windows::Win32::Foundation::CloseHandle(proc_handle);
            }
        }
    }

    // 7. Fallback to standard application icon
    if icon_result == 0 {
        if let Ok(def_icon) = LoadIconW(None, IDI_APPLICATION) {
            icon_result = def_icon.0 as usize;
        }
    }

    let result_opt = if icon_result != 0 {
        Some(icon_result)
    } else {
        None
    };

    if result_opt.is_some() {
        if let Ok(mut guard) = cache.lock() {
            guard.insert(hwnd_val, result_opt);
        }
    }

    result_opt.map(|ptr| HICON(ptr as *mut core::ffi::c_void))
}

// ----------------------------------------------------------------------------
// Signed Distance Field (SDF) Sub-Pixel Anti-Aliasing Rasterizer
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

#[allow(clippy::too_many_arguments)]
pub fn draw_aa_rounded_rect(
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
    fill_opacity: f32,
    border_rgb: COLORREF,
    border_thickness: f32,
    border_opacity: f32,
) {
    if w <= 0.0 || h <= 0.0 || (fill_opacity <= 0.0001 && border_opacity <= 0.0001) {
        return;
    }

    let half_w = w * 0.5;
    let half_h = h * 0.5;
    let center_x = x + half_w;
    let center_y = y + half_h;
    let r = radius.min(half_w).min(half_h).max(0.0);
    let bx = half_w - r;
    let by = half_h - r;

    let min_x = (x.floor() as i32).max(0);
    let max_x = ((x + w).ceil() as i32 + 1).min(max_w);
    let min_y = (y.floor() as i32).max(0);
    let max_y = ((y + h).ceil() as i32 + 1).min(max_h);

    let fr = (fill_rgb.0 & 0xFF) as f32 / 255.0;
    let fg = ((fill_rgb.0 >> 8) & 0xFF) as f32 / 255.0;
    let fb = ((fill_rgb.0 >> 16) & 0xFF) as f32 / 255.0;

    let br = (border_rgb.0 & 0xFF) as f32 / 255.0;
    let bg_val = ((border_rgb.0 >> 8) & 0xFF) as f32 / 255.0;
    let bb = ((border_rgb.0 >> 16) & 0xFF) as f32 / 255.0;

    let fill_op = fill_opacity.clamp(0.0, 1.0);
    let border_op = border_opacity.clamp(0.0, 1.0);
    let has_border = border_thickness > 0.0 && border_op > 0.001;

    for py in min_y..max_y {
        let row_offset = (py as usize) * stride;
        let py_f = py as f32 + 0.5;
        let qy = (py_f - center_y).abs() - by;

        for px in min_x..max_x {
            let px_f = px as f32 + 0.5;
            let qx = (px_f - center_x).abs() - bx;

            let idx = row_offset + (px as usize);
            if idx >= pixels.len() {
                continue;
            }

            // Exact Euclidean Signed Distance d to outer boundary
            let qx_clamped = qx.max(0.0);
            let qy_clamped = qy.max(0.0);
            let d = qx.max(qy).min(0.0) + (qx_clamped * qx_clamped + qy_clamped * qy_clamped).sqrt() - r;

            let cov_outer = (-d + 0.5).clamp(0.0, 1.0);
            if cov_outer <= 0.0001 {
                continue;
            }

            let (cov_inner, cov_border) = if has_border {
                let d_inner = d + border_thickness;
                let c_in = (-d_inner + 0.5).clamp(0.0, 1.0);
                let c_bor = (cov_outer - c_in).max(0.0);
                (c_in, c_bor)
            } else {
                (cov_outer, 0.0)
            };

            let fill_a = cov_inner * fill_op;
            let border_a = cov_border * border_op;
            let src_a = (fill_a + border_a).clamp(0.0, 1.0);
            if src_a <= 0.0001 {
                continue;
            }

            let sa_r = fr * fill_a + br * border_a;
            let sa_g = fg * fill_a + bg_val * border_a;
            let sa_b = fb * fill_a + bb * border_a;

            let dst = pixels[idx];
            let da = ((dst >> 24) & 0xFF) as f32 / 255.0;
            let dr = ((dst >> 16) & 0xFF) as f32 / 255.0;
            let dg = ((dst >> 8) & 0xFF) as f32 / 255.0;
            let db = (dst & 0xFF) as f32 / 255.0;

            let inv_sa = 1.0 - src_a;
            let out_a = src_a + da * inv_sa;
            let out_r = sa_r + dr * inv_sa;
            let out_g = sa_g + dg * inv_sa;
            let out_b = sa_b + db * inv_sa;

            pixels[idx] = (((out_a * 255.0).round() as u32) << 24)
                | (((out_r * 255.0).round() as u32) << 16)
                | (((out_g * 255.0).round() as u32) << 8)
                | ((out_b * 255.0).round() as u32);
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
    draw_aa_rounded_rect(
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
        opacity,
        border_rgb,
        border_thickness,
        opacity,
    );
}

// ----------------------------------------------------------------------------
// Hit Testing
// ----------------------------------------------------------------------------

pub fn hit_test_row(model: &LauncherRenderModel, x: i32, y: i32) -> Option<usize> {
    let row_x = ROW_X_PAD;
    let row_w = LAUNCHER_WIDTH - ROW_X_PAD * 2;
    if x < row_x || x > row_x + row_w {
        return None;
    }
    let count = model.rows.len().min(MAX_VISIBLE_ITEMS);
    let start_y = LIST_START_Y as i32;
    let row_h = ROW_HEIGHT;
    let row_step = ROW_STEP as i32;
    for i in 0..count {
        let ry = start_y + (i as i32) * row_step;
        if y >= ry && y < ry + row_h {
            return Some(i);
        }
    }
    None
}

// ----------------------------------------------------------------------------
// Hyprland-Win DComp Overlay Window Cutout Region
// ----------------------------------------------------------------------------

pub unsafe fn set_overlay_launcher_cutout(
    screen_w: i32,
    screen_h: i32,
    launcher_rect: Option<(i32, i32, i32, i32)>,
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
            if let Some((x, y, w, h)) = launcher_rect {
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

// ----------------------------------------------------------------------------
// Native Windows DWM Acrylic Backdrop & Layered Alpha Composition
// ----------------------------------------------------------------------------

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

/// Enable native DWM hardware-accelerated Acrylic blur behind `hwnd` with zero CPU shader cost.
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

/// Disable Acrylic blur behind `hwnd`.
pub unsafe fn disable_window_acrylic(hwnd: HWND) {
    if let Ok(user32) = GetModuleHandleW(PCWSTR(
        "user32.dll\0".encode_utf16().collect::<Vec<u16>>().as_ptr(),
    )) {
        if let Some(proc) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) {
            let set_wca: SetWindowCompositionAttributeFn = std::mem::transmute(proc);
            let mut policy = AccentPolicy {
                accent_state: 0, // ACCENT_DISABLED
                accent_flags: 0,
                gradient_color: 0,
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

#[allow(clippy::too_many_arguments)]
pub fn fill_aa_round_rect_layered(
    pixels: &mut [u32],
    _alpha_map: &mut [u8],
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
    draw_aa_rounded_rect(
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
        fill_alpha_u8 as f32 / 255.0,
        border_rgb,
        border_thickness,
        border_alpha_u8 as f32 / 255.0,
    );
}

// ----------------------------------------------------------------------------
// Anti-Aliased Bilinear / Area-Sampled Icon Resampling & Blending
// ----------------------------------------------------------------------------

struct RawIconData {
    width: i32,
    height: i32,
    pixels: Vec<u32>,
}

unsafe fn extract_icon_argb(hicon: HICON) -> Option<RawIconData> {
    let mut info = ICONINFO::default();
    if GetIconInfo(hicon, &mut info).is_ok() {
        let hbm_color = info.hbmColor;
        let hbm_mask = info.hbmMask;

        if !hbm_color.is_invalid() && !hbm_color.0.is_null() {
            let mut bm = BITMAP::default();
            GetObjectW(
                hbm_color,
                std::mem::size_of::<BITMAP>() as i32,
                Some(&mut bm as *mut _ as *mut core::ffi::c_void),
            );
            let w = bm.bmWidth;
            let h = bm.bmHeight;

            if w > 0 && h > 0 {
                let screen_dc = GetDC(HWND::default());
                let mut bmi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: w,
                        biHeight: -h, // top-down
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        biSizeImage: (w * h * 4) as u32,
                        ..Default::default()
                    },
                    ..Default::default()
                };

                let mut buffer = vec![0u32; (w * h) as usize];
                GetDIBits(
                    screen_dc,
                    hbm_color,
                    0,
                    h as u32,
                    Some(buffer.as_mut_ptr() as *mut _),
                    &mut bmi,
                    DIB_RGB_COLORS,
                );

                let has_alpha = buffer.iter().any(|&p| (p >> 24) > 0);

                if has_alpha {
                    // Pre-multiply alpha for proper compositing
                    for p in buffer.iter_mut() {
                        let a = (*p >> 24) & 0xFF;
                        if a == 0 {
                            *p = 0;
                        } else if a < 255 {
                            let r = (((*p >> 16) & 0xFF) * a + 127) / 255;
                            let g = (((*p >> 8) & 0xFF) * a + 127) / 255;
                            let b = ((*p & 0xFF) * a + 127) / 255;
                            *p = (a << 24) | (r << 16) | (g << 8) | b;
                        }
                    }
                } else if !hbm_mask.is_invalid() && !hbm_mask.0.is_null() {
                    let mut mask_buf = vec![0u32; (w * h) as usize];
                    GetDIBits(
                        screen_dc,
                        hbm_mask,
                        0,
                        h as u32,
                        Some(mask_buf.as_mut_ptr() as *mut _),
                        &mut bmi,
                        DIB_RGB_COLORS,
                    );
                    for i in 0..buffer.len() {
                        if (mask_buf[i] & 0x00FF_FFFF) != 0 {
                            buffer[i] = 0x0000_0000;
                        } else {
                            buffer[i] |= 0xFF00_0000;
                        }
                    }
                } else {
                    for p in buffer.iter_mut() {
                        *p |= 0xFF00_0000;
                    }
                }

                let _ = ReleaseDC(HWND::default(), screen_dc);
                let _ = DeleteObject(hbm_color);
                if !hbm_mask.is_invalid() && !hbm_mask.0.is_null() {
                    let _ = DeleteObject(hbm_mask);
                }

                return Some(RawIconData {
                    width: w,
                    height: h,
                    pixels: buffer,
                });
            }

            let _ = DeleteObject(hbm_color);
        }

        if !hbm_mask.is_invalid() && !hbm_mask.0.is_null() {
            let _ = DeleteObject(hbm_mask);
        }
    }

    // Fallback: render icon to a 48x48 scratch DIB
    let sz = 48;
    let screen_dc = GetDC(HWND::default());
    if screen_dc.is_invalid() {
        return None;
    }
    let mem_dc = CreateCompatibleDC(screen_dc);
    if mem_dc.is_invalid() {
        let _ = ReleaseDC(HWND::default(), screen_dc);
        return None;
    }

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: sz,
            biHeight: -sz,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (sz * sz * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
    let Ok(dib) = CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(HWND::default(), screen_dc);
        return None;
    };

    let old_bmp = SelectObject(mem_dc, dib);
    let scratch_slice = std::slice::from_raw_parts_mut(bits as *mut u32, (sz * sz) as usize);
    scratch_slice.fill(0x0000_0000);

    let _ = DrawIconEx(mem_dc, 0, 0, hicon, sz, sz, 0, None, DI_NORMAL);

    let mut out = scratch_slice.to_vec();
    let has_alpha = out.iter().any(|&p| (p >> 24) > 0);
    if !has_alpha {
        for p in out.iter_mut() {
            if (*p & 0x00FF_FFFF) != 0 {
                *p |= 0xFF00_0000;
            }
        }
    }

    SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(dib);
    let _ = DeleteDC(mem_dc);
    let _ = ReleaseDC(HWND::default(), screen_dc);

    Some(RawIconData {
        width: sz,
        height: sz,
        pixels: out,
    })
}

fn resample_icon_premul(
    src: &[u32],
    src_w: i32,
    src_h: i32,
    dst_w: i32,
    dst_h: i32,
) -> Vec<u32> {
    let mut out = vec![0u32; (dst_w * dst_h) as usize];
    if src_w <= 0 || src_h <= 0 || dst_w <= 0 || dst_h <= 0 || src.is_empty() {
        return out;
    }

    if src_w == dst_w && src_h == dst_h {
        let copy_len = out.len().min(src.len());
        out[..copy_len].copy_from_slice(&src[..copy_len]);
        return out;
    }

    let x_scale = src_w as f32 / dst_w as f32;
    let y_scale = src_h as f32 / dst_h as f32;

    // When downsampling significantly, use area averaging for anti-aliasing
    if x_scale > 1.4 || y_scale > 1.4 {
        for dy in 0..dst_h {
            let sy_start = (dy as f32 * y_scale).floor() as i32;
            let sy_end = (((dy + 1) as f32 * y_scale).ceil() as i32).min(src_h);
            let row_dst = (dy * dst_w) as usize;

            for dx in 0..dst_w {
                let sx_start = (dx as f32 * x_scale).floor() as i32;
                let sx_end = (((dx + 1) as f32 * x_scale).ceil() as i32).min(src_w);

                let mut sum_a = 0.0f32;
                let mut sum_r = 0.0f32;
                let mut sum_g = 0.0f32;
                let mut sum_b = 0.0f32;
                let mut total_weight = 0.0f32;

                for sy in sy_start..sy_end {
                    let y_box_start = dy as f32 * y_scale;
                    let y_box_end = (dy + 1) as f32 * y_scale;
                    let y0 = (sy as f32).max(y_box_start);
                    let y1 = ((sy + 1) as f32).min(y_box_end);
                    let h_weight = (y1 - y0).max(0.0);
                    if h_weight <= 0.0 {
                        continue;
                    }

                    let row_src = (sy * src_w) as usize;

                    for sx in sx_start..sx_end {
                        let x_box_start = dx as f32 * x_scale;
                        let x_box_end = (dx + 1) as f32 * x_scale;
                        let x0 = (sx as f32).max(x_box_start);
                        let x1 = ((sx + 1) as f32).min(x_box_end);
                        let w_weight = (x1 - x0).max(0.0);
                        let weight = h_weight * w_weight;
                        if weight <= 0.0 {
                            continue;
                        }

                        let idx = row_src + sx as usize;
                        if idx < src.len() {
                            let p = src[idx];
                            let a = ((p >> 24) & 0xFF) as f32;
                            let r = ((p >> 16) & 0xFF) as f32;
                            let g = ((p >> 8) & 0xFF) as f32;
                            let b = (p & 0xFF) as f32;

                            sum_a += a * weight;
                            sum_r += r * weight;
                            sum_g += g * weight;
                            sum_b += b * weight;
                            total_weight += weight;
                        }
                    }
                }

                if total_weight > 0.0 {
                    let inv = 1.0 / total_weight;
                    let a = (sum_a * inv).round().clamp(0.0, 255.0) as u32;
                    let r = (sum_r * inv).round().clamp(0.0, 255.0) as u32;
                    let g = (sum_g * inv).round().clamp(0.0, 255.0) as u32;
                    let b = (sum_b * inv).round().clamp(0.0, 255.0) as u32;
                    out[row_dst + dx as usize] = (a << 24) | (r << 16) | (g << 8) | b;
                }
            }
        }
    } else {
        // Bilinear interpolation for near 1:1 scaling
        for dy in 0..dst_h {
            let sy_f = (dy as f32 + 0.5) * y_scale - 0.5;
            let sy0 = (sy_f.floor() as i32).clamp(0, src_h - 1);
            let sy1 = (sy0 + 1).min(src_h - 1);
            let fy = (sy_f - sy0 as f32).clamp(0.0, 1.0);
            let inv_fy = 1.0 - fy;
            let row_dst = (dy * dst_w) as usize;

            for dx in 0..dst_w {
                let sx_f = (dx as f32 + 0.5) * x_scale - 0.5;
                let sx0 = (sx_f.floor() as i32).clamp(0, src_w - 1);
                let sx1 = (sx0 + 1).min(src_w - 1);
                let fx = (sx_f - sx0 as f32).clamp(0.0, 1.0);
                let inv_fx = 1.0 - fx;

                let w00 = inv_fx * inv_fy;
                let w10 = fx * inv_fy;
                let w01 = inv_fx * fy;
                let w11 = fx * fy;

                let p00 = src[(sy0 * src_w + sx0) as usize];
                let p10 = src[(sy0 * src_w + sx1) as usize];
                let p01 = src[(sy1 * src_w + sx0) as usize];
                let p11 = src[(sy1 * src_w + sx1) as usize];

                let a = (((p00 >> 24) & 0xFF) as f32 * w00
                    + ((p10 >> 24) & 0xFF) as f32 * w10
                    + ((p01 >> 24) & 0xFF) as f32 * w01
                    + ((p11 >> 24) & 0xFF) as f32 * w11)
                    .round()
                    .clamp(0.0, 255.0) as u32;

                let r = (((p00 >> 16) & 0xFF) as f32 * w00
                    + ((p10 >> 16) & 0xFF) as f32 * w10
                    + ((p01 >> 16) & 0xFF) as f32 * w01
                    + ((p11 >> 16) & 0xFF) as f32 * w11)
                    .round()
                    .clamp(0.0, 255.0) as u32;

                let g = (((p00 >> 8) & 0xFF) as f32 * w00
                    + ((p10 >> 8) & 0xFF) as f32 * w10
                    + ((p01 >> 8) & 0xFF) as f32 * w01
                    + ((p11 >> 8) & 0xFF) as f32 * w11)
                    .round()
                    .clamp(0.0, 255.0) as u32;

                let b = ((p00 & 0xFF) as f32 * w00
                    + (p10 & 0xFF) as f32 * w10
                    + (p01 & 0xFF) as f32 * w01
                    + (p11 & 0xFF) as f32 * w11)
                    .round()
                    .clamp(0.0, 255.0) as u32;

                out[row_dst + dx as usize] = (a << 24) | (r << 16) | (g << 8) | b;
            }
        }
    }

    out
}

static RESAMPLED_ICON_CACHE: OnceLock<Mutex<HashMap<usize, Vec<u32>>>> = OnceLock::new();

fn composite_icon_premul(
    pixels: &mut [u32],
    stride: usize,
    max_w: i32,
    max_h: i32,
    dst_x: i32,
    dst_y: i32,
    icon: &[u32],
    icon_w: i32,
    icon_h: i32,
    alpha_mul: f32,
) {
    let mul = alpha_mul.clamp(0.0, 1.0);
    if mul <= 0.001 {
        return;
    }

    for iy in 0..icon_h {
        let py = dst_y + iy;
        if py < 0 || py >= max_h {
            continue;
        }
        let row_dst = (py as usize) * stride;
        let row_src = (iy * icon_w) as usize;

        for ix in 0..icon_w {
            let px = dst_x + ix;
            if px < 0 || px >= max_w {
                continue;
            }

            let src_p = icon[row_src + ix as usize];
            let src_a_u8 = (src_p >> 24) & 0xFF;
            if src_a_u8 == 0 {
                continue;
            }

            let sa = ((src_a_u8 as f32 / 255.0) * mul).clamp(0.0, 1.0);
            let sr = (((src_p >> 16) & 0xFF) as f32 / 255.0) * mul;
            let sg = (((src_p >> 8) & 0xFF) as f32 / 255.0) * mul;
            let sb = ((src_p & 0xFF) as f32 / 255.0) * mul;

            let idx = row_dst + px as usize;
            let dst_p = pixels[idx];
            let da = ((dst_p >> 24) & 0xFF) as f32 / 255.0;
            let dr = ((dst_p >> 16) & 0xFF) as f32 / 255.0;
            let dg = ((dst_p >> 8) & 0xFF) as f32 / 255.0;
            let db = (dst_p & 0xFF) as f32 / 255.0;

            let inv_sa = 1.0 - sa;
            let out_a = sa + da * inv_sa;
            let out_r = sr + dr * inv_sa;
            let out_g = sg + dg * inv_sa;
            let out_b = sb + db * inv_sa;

            pixels[idx] = (((out_a * 255.0).round() as u32) << 24)
                | (((out_r * 255.0).round() as u32) << 16)
                | (((out_g * 255.0).round() as u32) << 8)
                | ((out_b * 255.0).round() as u32);
        }
    }
}

#[inline]
fn compute_bottom_notch_sdf(
    px_f: f32,
    py_f: f32,
    x0: f32,
    x1: f32,
    y_top: f32,
    y_bot: f32,
    r_corner: f32,
    r_ear: f32,
) -> f32 {
    if px_f < x0 {
        if py_f > y_bot - r_ear {
            let dx = px_f - (x0 - r_ear);
            let dy = py_f - (y_bot - r_ear);
            if dx < 0.0 {
                (dx * dx + (y_bot - py_f).powi(2)).sqrt()
            } else {
                r_ear - (dx * dx + dy * dy).sqrt()
            }
        } else if py_f < y_top + r_corner {
            let dx = x0 + r_corner - px_f;
            let dy = y_top + r_corner - py_f;
            (dx * dx + dy * dy).sqrt() - r_corner
        } else {
            x0 - px_f
        }
    } else if px_f > x1 {
        if py_f > y_bot - r_ear {
            let dx = (x1 + r_ear) - px_f;
            let dy = py_f - (y_bot - r_ear);
            if dx < 0.0 {
                (dx * dx + (y_bot - py_f).powi(2)).sqrt()
            } else {
                r_ear - (dx * dx + dy * dy).sqrt()
            }
        } else if py_f < y_top + r_corner {
            let dx = px_f - (x1 - r_corner);
            let dy = y_top + r_corner - py_f;
            (dx * dx + dy * dy).sqrt() - r_corner
        } else {
            px_f - x1
        }
    } else if py_f < y_top {
        if px_f < x0 + r_corner {
            let dx = x0 + r_corner - px_f;
            let dy = y_top + r_corner - py_f;
            (dx * dx + dy * dy).sqrt() - r_corner
        } else if px_f > x1 - r_corner {
            let dx = px_f - (x1 - r_corner);
            let dy = y_top + r_corner - py_f;
            (dx * dx + dy * dy).sqrt() - r_corner
        } else {
            y_top - py_f
        }
    } else {
        // Point is inside the bounding rectangle [x0, x1] x [y_top, y_bot]
        if px_f < x0 + r_corner && py_f < y_top + r_corner {
            let dx = x0 + r_corner - px_f;
            let dy = y_top + r_corner - py_f;
            (dx * dx + dy * dy).sqrt() - r_corner
        } else if px_f > x1 - r_corner && py_f < y_top + r_corner {
            let dx = px_f - (x1 - r_corner);
            let dy = y_top + r_corner - py_f;
            (dx * dx + dy * dy).sqrt() - r_corner
        } else {
            let mut dist = y_top - py_f;
            if py_f > y_bot - r_ear {
                let dx_l = px_f - (x0 - r_ear);
                let dy_l = py_f - (y_bot - r_ear);
                let d_left = r_ear - (dx_l * dx_l + dy_l * dy_l).sqrt();
                dist = dist.max(d_left);

                let dx_r = (x1 + r_ear) - px_f;
                let dy_r = py_f - (y_bot - r_ear);
                let d_right = r_ear - (dx_r * dx_r + dy_r * dy_r).sqrt();
                dist = dist.max(d_right);
            }
            dist
        }
    }
}

pub static LAUNCHER_BACKDROP: OnceLock<Mutex<Vec<u32>>> = OnceLock::new();

/// Captures the screen area behind the launcher and prepares a 70% Acrylic blurred backdrop.
pub unsafe fn capture_launcher_backdrop(win_x: i32, win_y: i32, width: i32, height: i32) {
    if width <= 0 || height <= 0 {
        return;
    }
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
            biHeight: -height, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (width * height * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
    if let Ok(dib) = CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
        let old_bmp = SelectObject(mem_dc, dib);
        let _ = BitBlt(mem_dc, 0, 0, width, height, screen_dc, win_x, win_y, SRCCOPY);

        let total_px = (width * height) as usize;
        let mut raw_pixels = vec![0u32; total_px];
        std::ptr::copy_nonoverlapping(bits as *const u32, raw_pixels.as_mut_ptr(), total_px);

        SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(dib);

        // Fast 3-pass separable box blur (radius = 18)
        let w = width as usize;
        let h = height as usize;
        let radius = 18_usize;
        let mut temp = vec![0u32; total_px];

        for _ in 0..3 {
            // Horizontal pass
            for y in 0..h {
                let row = y * w;
                let mut sum_r = 0u32;
                let mut sum_g = 0u32;
                let mut sum_b = 0u32;
                let mut count = 0u32;

                for x in 0..radius.min(w) {
                    let px = raw_pixels[row + x];
                    sum_r += (px >> 16) & 0xFF;
                    sum_g += (px >> 8) & 0xFF;
                    sum_b += px & 0xFF;
                    count += 1;
                }

                for x in 0..w {
                    if x + radius < w {
                        let px = raw_pixels[row + x + radius];
                        sum_r += (px >> 16) & 0xFF;
                        sum_g += (px >> 8) & 0xFF;
                        sum_b += px & 0xFF;
                        count += 1;
                    }
                    if x > radius {
                        let px = raw_pixels[row + x - radius - 1];
                        sum_r -= (px >> 16) & 0xFF;
                        sum_g -= (px >> 8) & 0xFF;
                        sum_b -= px & 0xFF;
                        count -= 1;
                    }
                    let r = sum_r / count.max(1);
                    let g = sum_g / count.max(1);
                    let b = sum_b / count.max(1);
                    temp[row + x] = (r << 16) | (g << 8) | b;
                }
            }

            // Vertical pass
            for x in 0..w {
                let mut sum_r = 0u32;
                let mut sum_g = 0u32;
                let mut sum_b = 0u32;
                let mut count = 0u32;

                for y in 0..radius.min(h) {
                    let px = temp[y * w + x];
                    sum_r += (px >> 16) & 0xFF;
                    sum_g += (px >> 8) & 0xFF;
                    sum_b += px & 0xFF;
                    count += 1;
                }

                for y in 0..h {
                    if y + radius < h {
                        let px = temp[(y + radius) * w + x];
                        sum_r += (px >> 16) & 0xFF;
                        sum_g += (px >> 8) & 0xFF;
                        sum_b += px & 0xFF;
                        count += 1;
                    }
                    if y > radius {
                        let px = temp[(y - radius - 1) * w + x];
                        sum_r -= (px >> 16) & 0xFF;
                        sum_g -= (px >> 8) & 0xFF;
                        sum_b += px & 0xFF;
                        count += 1;
                    }
                    let r = sum_r / count.max(1);
                    let g = sum_g / count.max(1);
                    let b = sum_b / count.max(1);
                    raw_pixels[y * w + x] = (r << 16) | (g << 8) | b;
                }
            }
        }

        // Tint every pixel with rgb(18, 19, 24) at 70% opacity (0.70)
        let tr = 18u32;
        let tg = 19u32;
        let tb = 24u32;
        for p in raw_pixels.iter_mut() {
            let br = (*p >> 16) & 0xFF;
            let bg = (*p >> 8) & 0xFF;
            let bb = *p & 0xFF;
            let fr = ((br * 30 + tr * 70) / 100).min(255);
            let fg = ((bg * 30 + tg * 70) / 100).min(255);
            let fb = ((bb * 30 + tb * 70) / 100).min(255);
            *p = (0xFF << 24) | (fr << 16) | (fg << 8) | fb;
        }

        let cache = LAUNCHER_BACKDROP.get_or_init(|| Mutex::new(Vec::new()));
        if let Ok(mut guard) = cache.lock() {
            *guard = raw_pixels;
        }
    }

    let _ = DeleteDC(mem_dc);
    let _ = ReleaseDC(HWND::default(), screen_dc);
}

pub fn draw_aa_bottom_notch(
    pixels: &mut [u32],
    stride: usize,
    max_w: i32,
    max_h: i32,
    x0: f32,
    x1: f32,
    y_top: f32,
    y_bot: f32,
    r_corner: f32,
    r_ear: f32,
    fill_rgb: COLORREF,
    _border_rgb: COLORREF,
    fill_opacity: f32,
    _border_opacity: f32,
) {
    let min_x = ((x0 - r_ear - 1.0).floor() as i32).max(0);
    let max_x = ((x1 + r_ear + 2.0).ceil() as i32).min(max_w);
    let min_y = ((y_top - 1.0).floor() as i32).max(0);
    let max_y = (y_bot.ceil() as i32).min(max_h);

    let f_r = (fill_rgb.0 & 0xFF) as f32;
    let f_g = ((fill_rgb.0 >> 8) & 0xFF) as f32;
    let f_b = ((fill_rgb.0 >> 16) & 0xFF) as f32;

    for py in min_y..max_y {
        let row_offset = (py as usize) * stride;
        let py_f = py as f32 + 0.5;

        for px in min_x..max_x {
            let px_f = px as f32 + 0.5;
            let idx = row_offset + (px as usize);
            if idx >= pixels.len() {
                continue;
            }

            let dist = compute_bottom_notch_sdf(
                px_f, py_f, x0, x1, y_top, y_bot, r_corner, r_ear,
            );

            let outer_cov = (-dist + 0.5).clamp(0.0, 1.0);
            if outer_cov <= 0.001 {
                continue;
            }

            let src_a = (outer_cov * fill_opacity).clamp(0.0, 1.0);
            let sa_r = (f_r / 255.0) * src_a;
            let sa_g = (f_g / 255.0) * src_a;
            let sa_b = (f_b / 255.0) * src_a;

            let a_u8 = (src_a * 255.0).round().clamp(0.0, 255.0) as u32;
            let r_u8 = (sa_r * 255.0).round().clamp(0.0, 255.0) as u32;
            let g_u8 = (sa_g * 255.0).round().clamp(0.0, 255.0) as u32;
            let b_u8 = (sa_b * 255.0).round().clamp(0.0, 255.0) as u32;

            pixels[idx] = (a_u8 << 24) | (r_u8 << 16) | (g_u8 << 8) | b_u8;
        }
    }
}

// ----------------------------------------------------------------------------
// Double-Buffered DIBSection & UpdateLayeredWindow Rendering
// ----------------------------------------------------------------------------

pub unsafe fn render_launcher(hwnd: HWND, model: &LauncherRenderModel) {
    let width = model.width.max(LAUNCHER_WIDTH);
    let height = model.height.max(LAUNCHER_HEIGHT);
    let _card_h = LAUNCHER_HEIGHT;

    ensure_departure_font_loaded();

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
    let Ok(dib_bmp) = CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(HWND::default(), screen_dc);
        return;
    };

    let total_px = (width * height) as usize;
    let pixels: &mut [u32] = std::slice::from_raw_parts_mut(bits as *mut u32, total_px);

    // 1. Start with 100% transparent black buffer (0x0000_0000) for pure premultiplied ARGB compositing
    pixels.fill(0x0000_0000);

    let ease = 1.0 - (1.0 - model.open_progress.clamp(0.0, 1.0)).powi(3);
    let cur_h = ((height as f32) * ease).clamp(0.0, height as f32);
    let y_top = (height as f32) - cur_h;
    let y_offset = y_top.round() as i32;
    let min_clip_y = (y_top.ceil() as i32).clamp(0, height);

    // Draw Bottom-Edge Notch SDF merging into 3px bottom screen border
    draw_aa_bottom_notch(
        pixels,
        width as usize,
        width,
        height,
        24.0,
        (width - 24) as f32,
        y_top,
        height as f32,
        22.0,
        22.0,
        rgb(18, 19, 24),
        rgb(18, 19, 24),
        1.0,
        0.0,
    );

    let (font_query, font_title, _font_keycap, _font_footer, font_badge, font_sub) =
        get_cached_fonts();

    // 1. App List Area (y = 14 + y_offset .. height - 58 + y_offset)
    let start_y = (LIST_START_Y as i32) + y_offset;
    let row_h = ROW_HEIGHT;
    let row_step = ROW_STEP as i32;
    let row_x = ROW_X_PAD;
    let row_w = width - ROW_X_PAD * 2;
    let visible_count = model.rows.len().min(MAX_VISIBLE_ITEMS);

    // Selected Row Pill
    if visible_count > 0 {
        let pill_y = (model.anim_selected_y.round() as i32 + y_offset).clamp(start_y, start_y + (visible_count.saturating_sub(1) as i32) * row_step);
        draw_aa_rounded_rect(
            pixels,
            width as usize,
            width,
            height,
            row_x as f32,
            pill_y as f32,
            row_w as f32,
            row_h as f32,
            18.0,
            rgb(45, 47, 58),
            0.95,
            rgb(66, 69, 84),
            1.0,
            0.95,
        );
    }

    if visible_count == 0 {
        let empty_rc = RECT {
            left: 36,
            top: start_y + 40,
            right: width - 36,
            bottom: start_y + 80,
        };
        draw_smooth_text(
            pixels,
            width as usize,
            width,
            height,
            "No matching apps or commands",
            &empty_rc,
            COLOR_PLACEHOLDER,
            font_title,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
    } else {
        for idx in 0..visible_count {
            let row_y = start_y + (idx as i32) * row_step;
            if row_y + row_h > height - 52 + y_offset {
                continue;
            }
            let is_selected = idx == model.selected_index;
            let is_hovered = Some(idx) == model.hovered_index;
            let row = &model.rows[idx];

            if !is_selected && is_hovered {
                draw_aa_rounded_rect(
                    pixels,
                    width as usize,
                    width,
                    height,
                    row_x as f32,
                    row_y as f32,
                    row_w as f32,
                    row_h as f32,
                    18.0,
                    COLOR_ROW_HOVER_BG,
                    (140.0 * ease) / 255.0,
                    COLOR_ROW_HOVER_BG,
                    0.0,
                    0.0,
                );
            }

            // High-Res 36x36 App Icon
            let icon_opt = if row.icon_hwnd != 0 {
                get_window_hicon(row.icon_hwnd)
            } else if !row.icon_path.is_empty() {
                get_path_hicon(&row.icon_path)
            } else {
                LoadIconW(None, IDI_APPLICATION).ok()
            };

            let icon_sz = 36;
            let icon_x = row_x + 14;
            let icon_y = row_y + (row_h - icon_sz) / 2;
            let mut drew_extracted_icon = false;

            if let Some(hicon) = icon_opt {
                let icon_key = hicon.0 as usize;
                let icon_cache = RESAMPLED_ICON_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
                let mut cached_entry = None;
                if let Ok(guard) = icon_cache.lock() {
                    if let Some(resampled) = guard.get(&icon_key) {
                        cached_entry = Some(resampled.clone());
                    }
                }

                let resampled = if let Some(data) = cached_entry {
                    data
                } else if let Some(raw) = extract_icon_argb(hicon) {
                    let res = resample_icon_premul(&raw.pixels, raw.width, raw.height, icon_sz, icon_sz);
                    if let Ok(mut guard) = icon_cache.lock() {
                        guard.insert(icon_key, res.clone());
                    }
                    res
                } else {
                    Vec::new()
                };

                if !resampled.is_empty() {
                    composite_icon_premul(
                        pixels,
                        width as usize,
                        width,
                        height,
                        icon_x,
                        icon_y,
                        &resampled,
                        icon_sz,
                        icon_sz,
                        ease,
                    );
                    drew_extracted_icon = true;
                }
            }

            if !drew_extracted_icon {
                draw_aa_rounded_rect(
                    pixels,
                    width as usize,
                    width,
                    height,
                    icon_x as f32,
                    icon_y as f32,
                    icon_sz as f32,
                    icon_sz as f32,
                    9.0,
                    COLOR_ICON_TILE_BG,
                    (210.0 * ease) / 255.0,
                    COLOR_ICON_TILE_BORDER,
                    1.0,
                    (230.0 * ease) / 255.0,
                );
            }

            // Two-Line Text (text_x = row_x + 62, text_right = row_x + row_w - 16)
            let title_rc = RECT {
                left: row_x + 62,
                top: row_y + 6,
                right: row_x + row_w - 16,
                bottom: row_y + 28,
            };
            draw_smooth_mixed_text(
                pixels,
                width as usize,
                width,
                height,
                &row.title,
                &title_rc,
                rgb(238, 240, 248),
                font_title,
            );

            let sub_str = if row.subtitle.is_empty() {
                "Application"
            } else {
                &row.subtitle
            };
            let sub_rc = RECT {
                left: row_x + 62,
                top: row_y + 28,
                right: row_x + row_w - 16,
                bottom: row_y + 48,
            };
            draw_smooth_text(
                pixels,
                width as usize,
                width,
                height,
                sub_str,
                &sub_rc,
                rgb(148, 152, 168),
                font_sub,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        }
    }

    // 2. Bottom Search Input Bar (y = height - 50 + y_offset .. height - 12 + y_offset, h = 38px)
    let input_x = 36;
    let input_w = width - 72;
    let input_y = height - 50 + y_offset;
    let input_h = 38;

    draw_aa_rounded_rect(
        pixels,
        width as usize,
        width,
        height,
        input_x as f32,
        input_y as f32,
        input_w as f32,
        input_h as f32,
        16.0,
        rgb(28, 30, 38),
        1.0,
        rgb(46, 48, 60),
        1.0,
        1.0,
    );

    // Left search icon (\u{F002})
    let srch_rc = RECT {
        left: input_x + 12,
        top: input_y,
        right: input_x + 36,
        bottom: input_y + input_h,
    };
    draw_smooth_text(
        pixels,
        width as usize,
        width,
        height,
        "\u{F002}",
        &srch_rc,
        rgb(156, 160, 178),
        font_badge,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    );

    // Query text & vertical caret bar
    let query_x = input_x + 42;
    let query_rc = RECT {
        left: query_x,
        top: input_y,
        right: input_x + input_w - 14,
        bottom: input_y + input_h,
    };

    let scratch_mutex = TEXT_SCRATCH.get_or_init(|| {
        Mutex::new(
            ScratchTextRenderer::new(2048, 256).expect("Failed to initialize text scratch DIB"),
        )
    });

    let mut q_sz = SIZE::default();
    if !model.query.is_empty() {
        let q_u16: Vec<u16> = model.query.encode_utf16().collect();
        if let Ok(guard) = scratch_mutex.lock() {
            let old_f = SelectObject(guard.dc, font_query);
            let _ = GetTextExtentPoint32W(guard.dc, &q_u16, &mut q_sz);
            SelectObject(guard.dc, old_f);
        }
    }

    if model.query.is_empty() {
        // Vertical caret bar
        draw_aa_rounded_rect(
            pixels,
            width as usize,
            width,
            height,
            query_x as f32,
            (input_y + 10) as f32,
            1.5,
            18.0,
            0.75,
            rgb(220, 224, 238),
            0.95,
            rgb(220, 224, 238),
            0.0,
            0.0,
        );

        let ph_rc = RECT {
            left: query_x + 6,
            top: input_y,
            right: input_x + input_w - 14,
            bottom: input_y + input_h,
        };
        draw_smooth_text(
            pixels,
            width as usize,
            width,
            height,
            "Type \">\" for commands",
            &ph_rc,
            rgb(138, 142, 160),
            font_query,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
    } else {
        draw_smooth_text(
            pixels,
            width as usize,
            width,
            height,
            &model.query,
            &query_rc,
            rgb(238, 240, 248),
            font_query,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );

        let cursor_x = (query_x + q_sz.cx + 2).min(input_x + input_w - 16) as f32;
        draw_aa_rounded_rect(
            pixels,
            width as usize,
            width,
            height,
            cursor_x,
            (input_y + 10) as f32,
            1.5,
            18.0,
            0.75,
            rgb(220, 224, 238),
            0.95,
            rgb(220, 224, 238),
            0.0,
            0.0,
        );
    }

    // Zero out any pixel in rows 0..min_clip_y so the launcher smoothly rises out of the bottom border
    for y in 0..min_clip_y {
        let row_start = (y as usize) * (width as usize);
        let row_end = row_start + (width as usize);
        if row_end <= pixels.len() {
            pixels[row_start..row_end].fill(0x0000_0000);
        }
    }

    // 6. Direct Premultiplied BGRA Alpha Composition with UpdateLayeredWindow
    let old_bmp = SelectObject(mem_dc, dib_bmp);

    let pt_src = POINT { x: 0, y: 0 };
    let size = SIZE {
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
        screen_dc,
        None,
        Some(&size),
        mem_dc,
        Some(&pt_src),
        COLORREF(0),
        Some(&blend),
        ULW_ALPHA,
    );

    // 7. Clean up GDI handles (cached HFONTs and scratch buffers are preserved)
    SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(dib_bmp);
    let _ = DeleteDC(mem_dc);
    let _ = ReleaseDC(HWND::default(), screen_dc);
}
