//! Hot-reloading JSON + CSS Theme Engine for `hyprbar`.
//!
//! Features:
//! - Full CSS stylesheet parser supporting `:root` custom properties (`--bar-height`, `--ws-active-bg`),
//!   CSS selectors (`window#hyprbar`, `#workspaces`, `#window`, `#clock`, etc.), and `var(--...)` resolution.
//! - Full JSON theme config (`hyprbar.json`) for declarative module ordering and geometry.
//! - Priority search paths:
//!   1. `config/hyprbar.json` / `config/hyprbar.css`
//!   2. `crates/hyprbar/config/hyprbar.json` / `crates/hyprbar/config/hyprbar.css`
//!   3. `%USERPROFILE%/.config/hyprwin/hyprbar.json` / `hyprbar.css`
//!   4. `%USERPROFILE%/.config/chickenwm/roobar.css` (backward compatibility with `roobar`)
//! - Sub-millisecond hot-reloading: checks file modification times and reloads instantly when modified.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;
use windows::Win32::Foundation::COLORREF;

/// Win32 `COLORREF` is `0x00BBGGRR`.
pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

/// Parse hex `#RGB`, `#RRGGBB`, `#RRGGBBAA`, `rgb(r,g,b)`, or `rgba(r,g,b,a)` into a Win32 `COLORREF`.
pub fn parse_color(input: &str) -> Option<COLORREF> {
    let s = input.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("transparent") || s.eq_ignore_ascii_case("none") {
        return Some(COLORREF(0));
    }

    if let Some(hex) = s.strip_prefix('#') {
        let hex = hex.trim();
        if !hex.is_ascii() {
            return None;
        }
        match hex.len() {
            3 => {
                let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
                let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
                let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
                return Some(rgb(r, g, b));
            }
            6 | 8 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                return Some(rgb(r, g, b));
            }
            _ => return None,
        }
    }

    if let Some(inner) = s
        .strip_prefix("rgba(")
        .or_else(|| s.strip_prefix("rgb("))
        .and_then(|v| v.strip_suffix(')'))
    {
        let parts: Vec<&str> = inner.split(',').map(|p| p.trim()).collect();
        if parts.len() >= 3 {
            let r = parts[0].parse::<u8>().ok()?;
            let g = parts[1].parse::<u8>().ok()?;
            let b = parts[2].parse::<u8>().ok()?;
            return Some(rgb(r, g, b));
        }
    }

    // Common named colors fallback
    match s.to_ascii_lowercase().as_str() {
        "black" => Some(rgb(0, 0, 0)),
        "white" => Some(rgb(255, 255, 255)),
        "cyan" => Some(rgb(51, 204, 255)),
        "mint" | "green" => Some(rgb(0, 255, 153)),
        "red" => Some(rgb(243, 139, 168)),
        "peach" | "orange" => Some(rgb(250, 179, 135)),
        _ => None,
    }
}

/// Parse pixel numbers like `36px`, `8`, `2.5rem`, `14pt`.
pub fn parse_px(val: &str) -> Option<f32> {
    let v = val
        .trim()
        .trim_end_matches("px")
        .trim_end_matches("rem")
        .trim_end_matches("pt")
        .trim();
    v.parse::<f32>().ok()
}

fn strip_css_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'*') {
            let _ = chars.next();
            while let Some(nc) = chars.next() {
                if nc == '*' && chars.peek() == Some(&'/') {
                    let _ = chars.next();
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[derive(Debug, Clone, Default)]
pub struct RawCssRule {
    pub properties: HashMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedCss {
    pub custom_properties: HashMap<String, String>,
    pub rules: HashMap<String, RawCssRule>,
}

impl ParsedCss {
    pub fn parse(source: &str) -> Self {
        let cleaned = strip_css_comments(source);
        let mut parsed = Self::default();

        let mut raw_blocks: Vec<(Vec<String>, Vec<(String, String)>)> = Vec::new();
        for chunk in cleaned.split('}') {
            let Some((sel_part, body_part)) = chunk.split_once('{') else {
                continue;
            };
            let selectors: Vec<String> = sel_part
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            if selectors.is_empty() {
                continue;
            }
            let mut decls = Vec::new();
            for decl in body_part.split(';') {
                let Some((prop, val)) = decl.split_once(':') else {
                    continue;
                };
                let prop = prop.trim().to_ascii_lowercase();
                let val = val.trim().to_string();
                if prop.starts_with("--") {
                    parsed.custom_properties.insert(prop.clone(), val.clone());
                }
                decls.push((prop, val));
            }
            raw_blocks.push((selectors, decls));
        }

        // Second pass: resolve `var(--name)`
        for (selectors, decls) in raw_blocks {
            for sel in selectors {
                let rule = parsed.rules.entry(sel).or_default();
                for (prop, raw_val) in decls.clone() {
                    let val = Self::resolve_var(&raw_val, &parsed.custom_properties);
                    rule.properties.insert(prop, val);
                }
            }
        }

        parsed
    }

    fn resolve_var(input: &str, vars: &HashMap<String, String>) -> String {
        let mut s = input.to_string();
        for _ in 0..4 {
            let Some(start) = s.find("var(") else {
                break;
            };
            let Some(end_rel) = s[start..].find(')') else {
                break;
            };
            let end = start + end_rel;
            let inner = &s[start + 4..end];
            let (var_name, fallback) = match inner.split_once(',') {
                Some((vn, fb)) => (vn.trim(), Some(fb.trim())),
                None => (inner.trim(), None),
            };
            let replacement = vars
                .get(var_name)
                .map(|v| v.as_str())
                .or(fallback)
                .unwrap_or("")
                .to_string();
            s.replace_range(start..=end, &replacement);
        }
        s
    }

    pub fn get_var(&self, name: &str) -> Option<&str> {
        self.custom_properties.get(name).map(|s| s.as_str())
    }

    pub fn get_rule_prop(&self, selector: &str, prop: &str) -> Option<&str> {
        self.rules
            .get(selector)
            .and_then(|r| r.properties.get(prop))
            .map(|s| s.as_str())
    }
}

// ----------------------------------------------------------------------------
// JSON Deserialization Structure
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HyprBarThemeJson {
    pub bar_height: Option<i32>,
    pub pill_height: Option<i32>,
    pub pill_radius: Option<i32>,
    pub pill_gap: Option<f32>,

    pub modules_left: Option<Vec<String>>,
    pub modules_center: Option<Vec<String>>,
    pub modules_right: Option<Vec<String>>,

    pub color_bar_bg: Option<String>,
    pub color_accent_line: Option<String>,
    pub color_text_white: Option<String>,
    pub color_text_muted: Option<String>,
    pub color_cyan: Option<String>,
    pub color_mint: Option<String>,
    pub color_peach: Option<String>,
    pub color_coral: Option<String>,

    pub ws_active_width: Option<f32>,
    pub ws_occupied_width: Option<f32>,
    pub ws_empty_dot_width: Option<f32>,
    pub ws_anim_speed: Option<f32>,
    pub color_ws_active_highlight: Option<String>,
    pub color_ws_active_text: Option<String>,
    pub color_ws_active_border: Option<String>,
    pub color_ws_occupied_bg: Option<String>,
    pub color_ws_occupied_border: Option<String>,
    pub color_ws_occupied_hover: Option<String>,
    pub color_ws_occupied_text: Option<String>,
    pub color_ws_empty_bg: Option<String>,
    pub color_ws_empty_border: Option<String>,
    pub color_ws_empty_dot: Option<String>,

    pub window_max_width: Option<i32>,
    pub color_window_class: Option<String>,
    pub color_window_title: Option<String>,
    pub active_title_fallback: Option<String>,

    pub clock_width: Option<i32>,
    pub clock_format: Option<String>,
    pub color_clock_bg: Option<String>,
    pub color_clock_text: Option<String>,

    pub font_family_ui: Option<String>,
    pub font_family_mono: Option<String>,
    pub font_size: Option<i32>,
    pub font_weight: Option<i32>,
}

// ----------------------------------------------------------------------------
// HyprBar Theme Runtime Model
// ----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct HyprBarTheme {
    // Geometry & Layout
    pub bar_height: i32,
    pub pill_height: i32,
    pub pill_radius: i32,
    pub pill_gap: f32,

    // Module Placement: modules to show in order
    // Minimalist default: modules_left = ["workspaces"], modules_center = ["window"], modules_right = ["clock"]
    pub modules_left: Vec<String>,
    pub modules_center: Vec<String>,
    pub modules_right: Vec<String>,

    // General Palette
    pub color_bar_bg: COLORREF,
    pub color_accent_line: COLORREF,
    pub color_text_white: COLORREF,
    pub color_text_muted: COLORREF,
    pub color_cyan: COLORREF,
    pub color_mint: COLORREF,
    pub color_peach: COLORREF,
    pub color_coral: COLORREF,

    // Workspaces Module
    pub ws_active_width: f32,
    pub ws_occupied_width: f32,
    pub ws_empty_dot_width: f32,
    pub ws_anim_speed: f32,
    pub color_ws_active_highlight: COLORREF,
    pub color_ws_active_text: COLORREF,
    pub color_ws_active_border: COLORREF,
    pub color_ws_occupied_bg: COLORREF,
    pub color_ws_occupied_border: COLORREF,
    pub color_ws_occupied_hover: COLORREF,
    pub color_ws_occupied_text: COLORREF,
    pub color_ws_empty_bg: COLORREF,
    pub color_ws_empty_border: COLORREF,
    pub color_ws_empty_dot: COLORREF,

    // Window Title Module
    pub window_max_width: i32,
    pub color_window_class: COLORREF,
    pub color_window_title: COLORREF,
    pub active_title_fallback: String,

    // Clock Module
    pub clock_width: i32,
    pub clock_format: String,
    pub color_clock_bg: COLORREF,
    pub color_clock_text: COLORREF,

    // Typography
    pub font_family_ui: String,
    pub font_family_mono: String,
    pub font_size: i32,
    pub font_weight: i32,

    // File paths and modification timestamps for hot reload
    pub loaded_json_path: Option<PathBuf>,
    pub loaded_css_path: Option<PathBuf>,
    pub last_json_modified: Option<SystemTime>,
    pub last_css_modified: Option<SystemTime>,
}

impl Default for HyprBarTheme {
    fn default() -> Self {
        Self {
            bar_height: 40,
            pill_height: 30,
            pill_radius: 15,
            pill_gap: 6.0,

            modules_left: vec!["workspaces".to_string()],
            modules_center: vec!["window".to_string()],
            modules_right: vec!["clock".to_string()],

            color_bar_bg: rgb(30, 30, 35),        // #1E1E23 macOS Dark charcoal-graphite background
            color_accent_line: rgb(64, 65, 74),   // #40414A gentle low-contrast 1px border
            color_text_white: rgb(228, 230, 235), // #E4E6EB soft Apple off-white
            color_text_muted: rgb(152, 154, 164), // #989AA4 soft secondary gray
            color_cyan: rgb(94, 158, 248),        // #5E9EF8 macOS system blue
            color_mint: rgb(82, 196, 126),        // #52C47E macOS soft green
            color_peach: rgb(238, 180, 84),       // #EEB454 macOS soft amber
            color_coral: rgb(235, 106, 102),      // #EB6A66 macOS soft coral red

            ws_active_width: 36.0,
            ws_occupied_width: 26.0,
            ws_empty_dot_width: 18.0,
            ws_anim_speed: 36.0,
            color_ws_active_highlight: rgb(74, 76, 86), // #4A4C56 active pill elevated surface
            color_ws_active_text: rgb(236, 237, 242),   // #ECEDF2 crisp off-white text
            color_ws_active_border: rgb(94, 96, 108),   // #5E606C subtle top highlight
            color_ws_occupied_bg: rgb(40, 41, 47),      // #28292F workspace track capsule
            color_ws_occupied_border: rgb(56, 58, 66),  // #383A42 soft border
            color_ws_occupied_hover: rgb(48, 50, 58),   // #30323A gentle hover
            color_ws_occupied_text: rgb(186, 188, 198), // #BABCC6 occupied inactive text
            color_ws_empty_bg: rgb(40, 41, 47),         // #28292F
            color_ws_empty_border: rgb(56, 58, 66),     // #383A42
            color_ws_empty_dot: rgb(146, 148, 158),     // #92949E inactive numerals/dots

            window_max_width: 750,
            color_window_class: rgb(228, 230, 235),
            color_window_title: rgb(228, 230, 235),
            active_title_fallback: "desktop · workspace {}".to_string(),

            clock_width: 68,
            clock_format: "%H:%M".to_string(),
            color_clock_bg: rgb(40, 41, 47),      // #28292F subtle clock capsule
            color_clock_text: rgb(228, 230, 235), // #E4E6EB soft Apple off-white

            font_family_ui: "SF Pro Text".to_string(),
            font_family_mono: "JetBrainsMono NF".to_string(),
            font_size: 16,
            font_weight: 700,

            loaded_json_path: None,
            loaded_css_path: None,
            last_json_modified: None,
            last_css_modified: None,
        }
    }
}

impl HyprBarTheme {
    /// Discovers and loads the theme from config files, applying CSS overrides.
    /// Prioritizes `C:\Users\mbura\.config` and picks whichever config file was modified most recently.
    pub fn load() -> Self {
        let mut theme = Self::default();

        // 1. Locate potential JSON config paths (pick most recently modified)
        let json_candidates = Self::json_candidates();
        if let Some((best_p, best_mtime)) = Self::newest_existing_file(&json_candidates) {
            if let Ok(content) = fs::read_to_string(&best_p) {
                let clean = content.trim_start_matches('\u{feff}');
                if let Ok(deserialized) = serde_json::from_str::<HyprBarThemeJson>(clean) {
                    theme.apply_json(&deserialized);
                    theme.loaded_json_path = Some(best_p);
                    theme.last_json_modified = Some(best_mtime);
                }
            }
        }

        // 2. Locate potential CSS files (pick most recently modified)
        let css_candidates = Self::css_candidates();
        if let Some((best_p, best_mtime)) = Self::newest_existing_file(&css_candidates) {
            if let Ok(content) = fs::read_to_string(&best_p) {
                let clean = content.trim_start_matches('\u{feff}');
                theme.apply_css(clean);
                theme.loaded_css_path = Some(best_p);
                theme.last_css_modified = Some(best_mtime);
            }
        }

        theme
    }

    fn json_candidates() -> Vec<PathBuf> {
        vec![hyprland_win_core::hyprland_win_config_file("hyprbar.json")]
    }

    fn css_candidates() -> Vec<PathBuf> {
        vec![hyprland_win_core::hyprland_win_config_file("hyprbar.css")]
    }

    fn newest_existing_file(candidates: &[PathBuf]) -> Option<(PathBuf, SystemTime)> {
        let mut best: Option<(PathBuf, SystemTime)> = None;
        for p in candidates {
            if p.is_file() {
                if let Ok(mtime) = fs::metadata(p).and_then(|m| m.modified()) {
                    if best.as_ref().map(|(_, bm)| mtime > *bm).unwrap_or(true) {
                        best = Some((p.clone(), mtime));
                    }
                }
            }
        }
        best
    }

    /// Check if `hyprbar.json` or `hyprbar.css` in `<userfolder>/.config/hyprwin` has changed on disk;
    /// if so, reloads immediately and returns `true`.
    pub fn check_and_reload(&mut self) -> bool {
        let mut needs_reload = false;

        if let Some((path, mtime)) = Self::newest_existing_file(&Self::json_candidates()) {
            let is_newer = self
                .last_json_modified
                .map(|prev| mtime != prev)
                .unwrap_or(true)
                || self.loaded_json_path.as_ref() != Some(&path);
            if is_newer {
                needs_reload = true;
            }
        }

        if let Some((path, mtime)) = Self::newest_existing_file(&Self::css_candidates()) {
            let is_newer = self
                .last_css_modified
                .map(|prev| mtime != prev)
                .unwrap_or(true)
                || self.loaded_css_path.as_ref() != Some(&path);
            if is_newer {
                needs_reload = true;
            }
        }

        if needs_reload {
            *self = Self::load();
            return true;
        }

        false
    }

    /// Apply deserialized JSON fields onto this theme.
    pub fn apply_json(&mut self, json: &HyprBarThemeJson) {
        if let Some(v) = json.bar_height { self.bar_height = v; }
        if let Some(v) = json.pill_height { self.pill_height = v; }
        if let Some(v) = json.pill_radius { self.pill_radius = v; }
        if let Some(v) = json.pill_gap { self.pill_gap = v; }

        if let Some(ref v) = json.modules_left { self.modules_left = v.clone(); }
        if let Some(ref v) = json.modules_center { self.modules_center = v.clone(); }
        if let Some(ref v) = json.modules_right { self.modules_right = v.clone(); }

        if let Some(c) = json.color_bar_bg.as_deref().and_then(parse_color) { self.color_bar_bg = c; }
        if let Some(c) = json.color_accent_line.as_deref().and_then(parse_color) { self.color_accent_line = c; }
        if let Some(c) = json.color_text_white.as_deref().and_then(parse_color) { self.color_text_white = c; }
        if let Some(c) = json.color_text_muted.as_deref().and_then(parse_color) { self.color_text_muted = c; }
        if let Some(c) = json.color_cyan.as_deref().and_then(parse_color) { self.color_cyan = c; }
        if let Some(c) = json.color_mint.as_deref().and_then(parse_color) { self.color_mint = c; }
        if let Some(c) = json.color_peach.as_deref().and_then(parse_color) { self.color_peach = c; }
        if let Some(c) = json.color_coral.as_deref().and_then(parse_color) { self.color_coral = c; }

        if let Some(v) = json.ws_active_width { self.ws_active_width = v; }
        if let Some(v) = json.ws_occupied_width { self.ws_occupied_width = v; }
        if let Some(v) = json.ws_empty_dot_width { self.ws_empty_dot_width = v; }
        if let Some(v) = json.ws_anim_speed { self.ws_anim_speed = v; }
        if let Some(c) = json.color_ws_active_highlight.as_deref().and_then(parse_color) { self.color_ws_active_highlight = c; }
        if let Some(c) = json.color_ws_active_text.as_deref().and_then(parse_color) { self.color_ws_active_text = c; }
        if let Some(c) = json.color_ws_active_border.as_deref().and_then(parse_color) { self.color_ws_active_border = c; }
        if let Some(c) = json.color_ws_occupied_bg.as_deref().and_then(parse_color) { self.color_ws_occupied_bg = c; }
        if let Some(c) = json.color_ws_occupied_border.as_deref().and_then(parse_color) { self.color_ws_occupied_border = c; }
        if let Some(c) = json.color_ws_occupied_hover.as_deref().and_then(parse_color) { self.color_ws_occupied_hover = c; }
        if let Some(c) = json.color_ws_occupied_text.as_deref().and_then(parse_color) { self.color_ws_occupied_text = c; }
        if let Some(c) = json.color_ws_empty_bg.as_deref().and_then(parse_color) { self.color_ws_empty_bg = c; }
        if let Some(c) = json.color_ws_empty_border.as_deref().and_then(parse_color) { self.color_ws_empty_border = c; }
        if let Some(c) = json.color_ws_empty_dot.as_deref().and_then(parse_color) { self.color_ws_empty_dot = c; }

        if let Some(v) = json.window_max_width { self.window_max_width = v; }
        if let Some(c) = json.color_window_class.as_deref().and_then(parse_color) { self.color_window_class = c; }
        if let Some(c) = json.color_window_title.as_deref().and_then(parse_color) { self.color_window_title = c; }
        if let Some(ref v) = json.active_title_fallback { self.active_title_fallback = v.clone(); }

        if let Some(v) = json.clock_width { self.clock_width = v; }
        if let Some(ref v) = json.clock_format { self.clock_format = v.clone(); }
        if let Some(c) = json.color_clock_bg.as_deref().and_then(parse_color) { self.color_clock_bg = c; }
        if let Some(c) = json.color_clock_text.as_deref().and_then(parse_color) { self.color_clock_text = c; }

        if let Some(ref v) = json.font_family_ui {
            self.font_family_ui = if v.eq_ignore_ascii_case("applefont")
                || v.eq_ignore_ascii_case("apple font")
                || v.eq_ignore_ascii_case("SF Pro")
                || v.eq_ignore_ascii_case("San Francisco")
            {
                "SF Pro Text".to_string()
            } else {
                v.clone()
            };
        }
        if let Some(ref v) = json.font_family_mono { self.font_family_mono = v.clone(); }
        if let Some(v) = json.font_size { self.font_size = v; }
        if let Some(v) = json.font_weight { self.font_weight = v; }
    }

    /// Apply CSS properties (`:root` variables and selectors) onto this theme.
    pub fn apply_css(&mut self, css_source: &str) {
        let css = ParsedCss::parse(css_source);

        // 1. Root Custom Variables
        if let Some(v) = css.get_var("--bar-height").and_then(parse_px) {
            self.bar_height = v as i32;
        }
        if let Some(v) = css.get_var("--pill-height").and_then(parse_px) {
            self.pill_height = v as i32;
        }
        if let Some(v) = css.get_var("--pill-radius").and_then(parse_px) {
            self.pill_radius = v as i32;
        }
        if let Some(v) = css.get_var("--pill-gap").and_then(parse_px) {
            self.pill_gap = v;
        }

        // Colors
        if let Some(c) = css.get_var("--bar-bg").and_then(parse_color) {
            self.color_bar_bg = c;
        }
        if let Some(c) = css.get_var("--border-color").and_then(parse_color) {
            self.color_accent_line = c;
        }
        if let Some(c) = css.get_var("--fg-color").and_then(parse_color) {
            self.color_text_white = c;
            self.color_window_title = c;
        }
        if let Some(c) = css.get_var("--subtext-color").and_then(parse_color) {
            self.color_text_muted = c;
        }
        if let Some(c) = css.get_var("--accent-color").and_then(parse_color) {
            self.color_cyan = c;
            self.color_ws_active_highlight = c;
            self.color_clock_text = c;
        }

        // Workspaces custom properties
        if let Some(c) = css.get_var("--ws-active-bg").and_then(parse_color) {
            self.color_ws_active_highlight = c;
        }
        if let Some(c) = css.get_var("--ws-active-fg").and_then(parse_color) {
            self.color_ws_active_text = c;
        }
        if let Some(c) = css.get_var("--ws-active-border").and_then(parse_color) {
            self.color_ws_active_border = c;
        }
        if let Some(c) = css.get_var("--clock-bg").and_then(parse_color) {
            self.color_clock_bg = c;
        }
        if let Some(c) = css.get_var("--ws-occupied-bg").and_then(parse_color) {
            self.color_ws_occupied_bg = c;
        }
        if let Some(c) = css.get_var("--ws-occupied-fg").and_then(parse_color) {
            self.color_ws_occupied_text = c;
        }
        if let Some(c) = css.get_var("--ws-empty-bg").and_then(parse_color) {
            self.color_ws_empty_bg = c;
        }
        if let Some(c) = css.get_var("--ws-empty-fg").and_then(parse_color) {
            self.color_ws_empty_dot = c;
        }
        if let Some(w) = css.get_var("--ws-active-width").and_then(parse_px) {
            self.ws_active_width = w;
        }
        if let Some(w) = css.get_var("--ws-occupied-width").and_then(parse_px) {
            self.ws_occupied_width = w;
        }
        if let Some(w) = css.get_var("--ws-empty-dot-width").and_then(parse_px) {
            self.ws_empty_dot_width = w;
        }
        if let Some(spd) = css.get_var("--ws-anim-speed").and_then(parse_px) {
            self.ws_anim_speed = spd;
        }

        // Modules custom properties
        if let Some(mods) = css.get_var("--modules-right") {
            let list: Vec<String> = mods
                .split_whitespace()
                .map(|s| s.trim_matches(';').to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            if !list.is_empty() {
                self.modules_right = list;
            }
        }
        if let Some(mods) = css.get_var("--modules-left") {
            let list: Vec<String> = mods
                .split_whitespace()
                .map(|s| s.trim_matches(';').to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            if !list.is_empty() {
                self.modules_left = list;
            }
        }
        if let Some(mods) = css.get_var("--modules-center") {
            let list: Vec<String> = mods
                .split_whitespace()
                .map(|s| s.trim_matches(';').to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            if !list.is_empty() {
                self.modules_center = list;
            }
        }

        if let Some(fmt) = css.get_var("--clock-format") {
            self.clock_format = fmt.trim_matches('"').trim_matches('\'').trim_matches(';').to_string();
        }

        if let Some(font) = css.get_var("--font-family") {
            let first = font.split(',').next().unwrap_or(font).trim_matches('"').trim_matches('\'').trim();
            if !first.is_empty() {
                let resolved = if first.eq_ignore_ascii_case("applefont")
                    || first.eq_ignore_ascii_case("apple font")
                    || first.eq_ignore_ascii_case("SF Pro")
                    || first.eq_ignore_ascii_case("San Francisco")
                {
                    "SF Pro Text".to_string()
                } else {
                    first.to_string()
                };
                self.font_family_ui = resolved.clone();
                self.font_family_mono = resolved;
            }
        }
        if let Some(sz) = css.get_var("--font-size").and_then(parse_px) {
            self.font_size = sz as i32;
        }
        if let Some(w) = css.get_var("--font-weight").and_then(parse_px) {
            self.font_weight = w as i32;
        }

        // 2. Selectors overrides
        if let Some(c) = css.get_rule_prop("#clock", "color").and_then(parse_color) {
            self.color_clock_text = c;
        }
        if let Some(c) = css.get_rule_prop("#clock", "background-color").and_then(parse_color) {
            self.color_clock_bg = c;
        }
        if let Some(c) = css.get_rule_prop("#window", "color").and_then(parse_color) {
            self.color_window_title = c;
        }
    }
}

fn user_config_dir() -> PathBuf {
    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        PathBuf::from(userprofile).join(".config")
    } else {
        PathBuf::from(".config")
    }
}
