//! Parser for standard `<userfolder>/.config/hyprwin/hyprland.conf` configuration files.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::bezier::CubicBezier;

/// Returns the canonical `hyprland-win` configuration directory `<userfolder>/.config/hyprwin`,
/// creating it on disk if it does not exist yet.
pub fn hyprland_win_config_dir() -> PathBuf {
    let base = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join(".config").join("hyprwin");
    if !dir.exists() {
        let _ = std::fs::create_dir_all(&dir);
    }
    dir
}

/// Resolves `<userfolder>/.config/hyprwin/<filename>`, seeding it once from `config/<filename>`
/// only if the file does not exist yet in `<userfolder>/.config/hyprwin`.
pub fn hyprland_win_config_file(filename: &str) -> PathBuf {
    let target = hyprland_win_config_dir().join(filename);
    if !target.exists() {
        let mut seed_candidates = vec![PathBuf::from("config").join(filename)];
        if let Ok(exe) = std::env::current_exe() {
            if let Some(root) = exe.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
                seed_candidates.push(root.join("config").join(filename));
            }
        }
        for seed in seed_candidates {
            if seed.is_file() {
                let _ = std::fs::copy(&seed, &target);
                break;
            }
        }
    }
    target
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutMode {
    Dwindle,
    Master,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyprConfig {
    pub gaps_in: i32,
    pub gaps_out: i32,
    pub border_size: i32,
    pub active_border_rgba: [f32; 4],
    pub active_border_rgba_secondary: [f32; 4],
    pub border_angle_deg: f32,
    pub inactive_border_rgba: [f32; 4],
    pub layout: LayoutMode,
    pub direct_scanout: bool,
    pub follow_mouse: bool,
    pub rounding: f32,
    pub active_opacity: f32,
    pub inactive_opacity: f32,
    pub blur_enabled: bool,
    pub blur_size: u32,
    pub blur_passes: u32,
    pub animations_enabled: bool,
    pub master_mfact: f32,
    pub weather_location: String,
    pub wallpaper: Option<String>,
    pub beziers: HashMap<String, CubicBezier>,
    pub binds: Vec<Keybind>,
    pub window_rules: Vec<WindowRuleV2>,
    pub exec_once: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keybind {
    pub flags: String,
    pub modifiers: String,
    pub key: String,
    pub dispatcher: String,
    pub arg: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WindowRuleEffect {
    Float,
    Tile,
    Fullscreen,
    Immediate,
    Center,
    Workspace(u32),
    Size(f32, f32),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowRuleV2 {
    pub effect: WindowRuleEffect,
    pub class_pattern: Option<String>,
    pub title_pattern: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct WindowRuleDecision {
    pub float: Option<bool>,
    pub fullscreen: Option<bool>,
    pub center: bool,
    pub workspace: Option<u32>,
    pub size: Option<(f32, f32)>,
}

impl Default for HyprConfig {
    fn default() -> Self {
        let mut beziers = HashMap::new();
        beziers.insert("default".to_string(), CubicBezier::default());
        beziers.insert("myBezier".to_string(), CubicBezier::new(0.05, 0.9, 0.1, 1.05));
        beziers.insert("overshot".to_string(), CubicBezier::new(0.13, 0.99, 0.29, 1.1));

        Self {
            gaps_in: 6,
            gaps_out: 6,
            border_size: 2,
            active_border_rgba: [0.44, 0.44, 0.48, 0.80],
            active_border_rgba_secondary: [0.44, 0.44, 0.48, 0.80],
            border_angle_deg: 0.0,
            inactive_border_rgba: [0.15, 0.15, 0.16, 0.60],
            layout: LayoutMode::Dwindle,
            direct_scanout: true,
            follow_mouse: true,
            rounding: 22.0,
            active_opacity: 1.0,
            inactive_opacity: 1.0,
            blur_enabled: false,
            blur_size: 8,
            blur_passes: 3,
            animations_enabled: true,
            master_mfact: 0.55,
            weather_location: "Istanbul, Tuzla".to_string(),
            wallpaper: None,
            beziers,
            binds: Vec::new(),
            window_rules: Vec::new(),
            exec_once: Vec::new(),
        }
    }
}

fn expand_hypr_vars(input: &str, vars: &[(String, String)]) -> String {
    if !input.contains('$') || vars.is_empty() {
        return input.to_string();
    }
    let mut out = input.to_string();
    for (k, v) in vars {
        if out.contains(k.as_str()) {
            out = out.replace(k.as_str(), v.as_str());
        }
    }
    out
}

impl HyprConfig {
    pub fn load_from_file(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(e) => {
                tracing::warn!("Could not read {}: {e} — using default HyprConfig", path.display());
                Self::default()
            }
        }
    }

    pub fn parse(src: &str) -> Self {
        let mut cfg = Self::default();
        let mut section_stack: Vec<String> = Vec::new();
        let mut vars: Vec<(String, String)> = Vec::new();

        for raw_line in src.lines() {
            let line = raw_line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some(sec_part) = line.strip_suffix('{') {
                section_stack.push(sec_part.trim().to_ascii_lowercase());
                continue;
            }
            if line == "}" {
                section_stack.pop();
                continue;
            }

            let Some((raw_key, raw_val)) = line.split_once('=') else {
                continue;
            };
            let key_trim = raw_key.trim();
            let val_expanded = expand_hypr_vars(raw_val.trim(), &vars);

            if key_trim.starts_with('$') && section_stack.is_empty() {
                if let Some(existing) = vars.iter_mut().find(|(k, _)| k == key_trim) {
                    existing.1 = val_expanded;
                } else {
                    vars.push((key_trim.to_string(), val_expanded));
                    // Keep longest variable names first so `$mainModShift` expands before `$mainMod`
                    vars.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
                }
                continue;
            }

            let key_expanded = expand_hypr_vars(key_trim, &vars);
            let key = key_expanded.trim();
            let val = val_expanded.trim();
            let current_sec = section_stack.last().map(|s| s.as_str()).unwrap_or("");

            match (current_sec, key) {
                ("", "exec-once") => cfg.exec_once.push(val.to_string()),
                ("", "weather_location") | ("general", "weather_location") => {
                    let loc = val.trim_matches('"').trim_matches('\'').trim();
                    if !loc.is_empty() {
                        cfg.weather_location = loc.to_string();
                    }
                }
                ("", "wallpaper") | ("misc", "wallpaper") => {
                    let wp = val.split(',').last().unwrap_or(val).trim();
                    if !wp.is_empty() {
                        cfg.wallpaper = Some(wp.to_string());
                    }
                }
                ("", "windowrulev2") | ("", "windowrule") => {
                    if let Some(rule) = parse_window_rule_v2(val) {
                        cfg.window_rules.push(rule);
                    }
                }
                ("", k)
                    if matches!(
                        k,
                        "bind" | "binde" | "bindl" | "bindr" | "bindi" | "bindm" | "bindel" | "bindn"
                    ) =>
                {
                    let flags = k.strip_prefix("bind").unwrap_or("").to_string();
                    let parts: Vec<&str> = val.splitn(4, ',').map(|s| s.trim()).collect();
                    if parts.len() >= 3 {
                        cfg.binds.push(Keybind {
                            flags,
                            modifiers: parts[0].to_string(),
                            key: parts[1].to_string(),
                            dispatcher: parts[2].to_string(),
                            arg: parts.get(3).copied().unwrap_or("").to_string(),
                        });
                    }
                }
                ("", "unbind") => {
                    let parts: Vec<&str> = val.splitn(2, ',').map(|s| s.trim()).collect();
                    if parts.len() == 2 {
                        let m_target = parts[0].to_ascii_uppercase();
                        let k_target = parts[1].to_ascii_uppercase();
                        cfg.binds.retain(|b| {
                            !(b.modifiers.trim().to_ascii_uppercase() == m_target
                                && b.key.trim().to_ascii_uppercase() == k_target)
                        });
                    }
                }
                ("animations", "bezier") | ("", "bezier") => {
                    let parts: Vec<&str> = val.split(',').map(|s| s.trim()).collect();
                    if parts.len() == 5 {
                        if let (Ok(x1), Ok(y1), Ok(x2), Ok(y2)) = (
                            parts[1].parse::<f32>(),
                            parts[2].parse::<f32>(),
                            parts[3].parse::<f32>(),
                            parts[4].parse::<f32>(),
                        ) {
                            cfg.beziers
                                .insert(parts[0].to_string(), CubicBezier::new(x1, y1, x2, y2));
                        }
                    }
                }
                ("general", "gaps_in") => {
                    if let Ok(v) = val.parse() {
                        cfg.gaps_in = v;
                    }
                }
                ("general", "gaps_out") => {
                    if let Ok(v) = val.parse() {
                        cfg.gaps_out = v;
                    }
                }
                ("general", "border_size") => {
                    if let Ok(v) = val.parse() {
                        cfg.border_size = v;
                    }
                }
                ("general", "layout") => {
                    cfg.layout = if val.eq_ignore_ascii_case("master") {
                        LayoutMode::Master
                    } else {
                        LayoutMode::Dwindle
                    };
                }
                ("general", "direct_scanout") => {
                    cfg.direct_scanout = matches!(val, "true" | "1" | "yes");
                }
                ("general", "col.active_border") => {
                    let tokens: Vec<&str> = val.split_whitespace().collect();
                    if let Some(c1) = tokens.first().and_then(|t| parse_hypr_rgba(t)) {
                        cfg.active_border_rgba = c1;
                        cfg.active_border_rgba_secondary = c1;
                    }
                    if let Some(c2) = tokens.get(1).and_then(|t| parse_hypr_rgba(t)) {
                        cfg.active_border_rgba_secondary = c2;
                    }
                    for tok in &tokens {
                        if let Some(deg_str) = tok.strip_suffix("deg") {
                            if let Ok(deg) = deg_str.parse::<f32>() {
                                cfg.border_angle_deg = deg;
                            }
                        }
                    }
                }
                ("general", "col.inactive_border") => {
                    if let Some(c) = parse_hypr_rgba(val) {
                        cfg.inactive_border_rgba = c;
                    }
                }
                ("decoration", "rounding") => {
                    if let Ok(v) = val.parse() {
                        cfg.rounding = v;
                    }
                }
                ("decoration", "active_opacity") => {
                    if let Ok(v) = val.parse() {
                        cfg.active_opacity = v;
                    }
                }
                ("decoration", "inactive_opacity") => {
                    if let Ok(v) = val.parse() {
                        cfg.inactive_opacity = v;
                    }
                }
                ("blur", "enabled") => {
                    cfg.blur_enabled = matches!(val, "true" | "1" | "yes");
                }
                ("blur", "size") => {
                    if let Ok(v) = val.parse() {
                        cfg.blur_size = v;
                    }
                }
                ("blur", "passes") => {
                    if let Ok(v) = val.parse() {
                        cfg.blur_passes = v;
                    }
                }
                ("animations", "enabled") => {
                    cfg.animations_enabled = matches!(val, "true" | "1" | "yes");
                }
                ("master", "mfact") => {
                    if let Ok(v) = val.parse() {
                        cfg.master_mfact = v;
                    }
                }
                ("input", "follow_mouse") | ("general", "follow_mouse") => {
                    cfg.follow_mouse = !matches!(val, "0" | "false" | "no");
                }
                _ => {}
            }
        }
        cfg
    }

    pub fn apply_keyword(&mut self, key: &str, val: &str) -> bool {
        match key {
            "general:gaps_in" => val.parse().map(|v| self.gaps_in = v).is_ok(),
            "general:gaps_out" => val.parse().map(|v| self.gaps_out = v).is_ok(),
            "general:border_size" => val.parse().map(|v| self.border_size = v).is_ok(),
            "general:col.active_border" => {
                let tokens: Vec<&str> = val.split_whitespace().collect();
                if let Some(c1) = tokens.first().and_then(|t| parse_hypr_rgba(t)) {
                    self.active_border_rgba = c1;
                    self.active_border_rgba_secondary = c1;
                    if let Some(c2) = tokens.get(1).and_then(|t| parse_hypr_rgba(t)) {
                        self.active_border_rgba_secondary = c2;
                    }
                    for tok in &tokens {
                        if let Some(deg_str) = tok.strip_suffix("deg") {
                            if let Ok(deg) = deg_str.parse::<f32>() {
                                self.border_angle_deg = deg;
                            }
                        }
                    }
                    return true;
                }
                false
            }
            "general:col.inactive_border" => {
                if let Some(c) = parse_hypr_rgba(val) {
                    self.inactive_border_rgba = c;
                    return true;
                }
                false
            }
            "decoration:rounding" => val.parse().map(|v| self.rounding = v).is_ok(),
            "decoration:active_opacity" => val.parse().map(|v| self.active_opacity = v).is_ok(),
            "decoration:inactive_opacity" => val.parse().map(|v| self.inactive_opacity = v).is_ok(),
            "decoration:blur:enabled" => {
                self.blur_enabled = matches!(val, "true" | "1" | "yes");
                true
            }
            "decoration:blur:size" => val.parse().map(|v| self.blur_size = v).is_ok(),
            "decoration:blur:passes" => val.parse().map(|v| self.blur_passes = v).is_ok(),
            "animations:enabled" => {
                self.animations_enabled = matches!(val, "true" | "1" | "yes");
                true
            }
            "master:mfact" => val.parse().map(|v| self.master_mfact = v).is_ok(),
            "general:direct_scanout" => {
                self.direct_scanout = matches!(val, "true" | "1" | "yes");
                true
            }
            "general:layout" => {
                self.layout = if val.eq_ignore_ascii_case("master") {
                    LayoutMode::Master
                } else {
                    LayoutMode::Dwindle
                };
                true
            }
            "input:follow_mouse" | "general:follow_mouse" => {
                self.follow_mouse = !matches!(val, "0" | "false" | "no");
                true
            }
            "weather_location" => {
                self.weather_location = val.trim_matches('"').trim_matches('\'').trim().to_string();
                true
            }
            "wallpaper" => {
                let wp = val.split(',').last().unwrap_or(val).trim();
                self.wallpaper = if wp.is_empty() { None } else { Some(wp.to_string()) };
                true
            }
            _ => false,
        }
    }

    pub fn evaluate_window_rules(
        &self,
        exe_name: &str,
        class_name: &str,
        title: &str,
    ) -> WindowRuleDecision {
        let mut out = WindowRuleDecision::default();
        for rule in &self.window_rules {
            let class_ok = match &rule.class_pattern {
                Some(pat) => pattern_matches(pat, exe_name) || pattern_matches(pat, class_name),
                None => true,
            };
            let title_ok = match &rule.title_pattern {
                Some(pat) => pattern_matches(pat, title),
                None => true,
            };
            if class_ok && title_ok {
                match rule.effect {
                    WindowRuleEffect::Float => out.float = Some(true),
                    WindowRuleEffect::Tile => out.float = Some(false),
                    WindowRuleEffect::Fullscreen => out.fullscreen = Some(true),
                    WindowRuleEffect::Immediate => {}
                    WindowRuleEffect::Center => out.center = true,
                    WindowRuleEffect::Workspace(ws) => out.workspace = Some(ws),
                    WindowRuleEffect::Size(w, h) => out.size = Some((w, h)),
                }
            }
        }
        out
    }
}

fn parse_window_rule_v2(val: &str) -> Option<WindowRuleV2> {
    let parts: Vec<&str> = val.split(',').map(|s| s.trim()).collect();
    if parts.len() < 2 {
        return None;
    }
    let effect_str = parts[0].to_ascii_lowercase();
    let effect = if effect_str == "float" {
        WindowRuleEffect::Float
    } else if effect_str == "tile" {
        WindowRuleEffect::Tile
    } else if effect_str.starts_with("fullscreen") {
        WindowRuleEffect::Fullscreen
    } else if effect_str == "immediate" {
        WindowRuleEffect::Immediate
    } else if effect_str == "center" {
        WindowRuleEffect::Center
    } else if let Some(ws_arg) = effect_str.strip_prefix("workspace") {
        let ws_trim = ws_arg.trim();
        let ws = if ws_trim.starts_with("special") {
            99
        } else {
            ws_trim.parse::<u32>().ok()?
        };
        WindowRuleEffect::Workspace(ws)
    } else if let Some(sz_arg) = effect_str.strip_prefix("size") {
        let mut nums = sz_arg.split_whitespace();
        let w = nums.next()?.parse::<f32>().ok()?;
        let h = nums.next()?.parse::<f32>().ok()?;
        WindowRuleEffect::Size(w, h)
    } else {
        return None;
    };

    let mut class_pattern = None;
    let mut title_pattern = None;
    for cond in &parts[1..] {
        if let Some(c) = cond.strip_prefix("class:") {
            class_pattern = Some(c.trim().to_string());
        } else if let Some(t) = cond.strip_prefix("title:") {
            title_pattern = Some(t.trim().to_string());
        } else if class_pattern.is_none() {
            class_pattern = Some(cond.trim().to_string());
        }
    }

    Some(WindowRuleV2 {
        effect,
        class_pattern,
        title_pattern,
    })
}

fn pattern_matches(pattern: &str, text: &str) -> bool {
    let clean = pattern
        .trim_start_matches('^')
        .trim_end_matches('$')
        .trim_start_matches('(')
        .trim_end_matches(')')
        .replace(r"\.", ".");
    if clean.is_empty() {
        return true;
    }
    let text_lower = text.to_ascii_lowercase();
    for branch in clean.split('|') {
        let b = branch.trim().to_ascii_lowercase();
        if !b.is_empty() && text_lower.contains(&b) {
            return true;
        }
    }
    false
}

fn parse_hypr_rgba(token: &str) -> Option<[f32; 4]> {
    let hex = token
        .strip_prefix("rgba(")
        .and_then(|s| s.strip_suffix(')'))
        .or_else(|| token.strip_prefix("0x"))?;
    if hex.len() != 8 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()? as f32 / 255.0;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()? as f32 / 255.0;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()? as f32 / 255.0;
    let a = u8::from_str_radix(&hex[6..8], 16).ok()? as f32 / 255.0;
    Some([r, g, b, a])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sample_hyprland_conf() {
        let src = r#"
        general {
            gaps_in = 6
            gaps_out = 12
            border_size = 3
            col.active_border = rgba(33ccffee) rgba(00ff99ee) 45deg
            layout = master
        }
        decoration {
            rounding = 14
            blur {
                enabled = true
                size = 10
                passes = 4
            }
        }
        bind = SUPER, Q, exec, wt.exe
        "#;
        let cfg = HyprConfig::parse(src);
        assert_eq!(cfg.gaps_in, 6);
        assert_eq!(cfg.gaps_out, 12);
        assert_eq!(cfg.border_size, 3);
        assert_eq!(cfg.layout, LayoutMode::Master);
        assert_eq!(cfg.rounding, 14.0);
        assert_eq!(cfg.blur_size, 10);
        assert_eq!(cfg.blur_passes, 4);
        assert_eq!(cfg.binds.len(), 1);
        assert_eq!(cfg.binds[0].dispatcher, "exec");
    }
}
