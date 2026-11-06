//! Hyprland Dwindle BSP & Master-Stack Tiling Layout Engine + Workspace State Machine,
//! Directional Focus/Swap, and Interactive `SUPER` Mouse Drag/Scale Support.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::bezier::{CubicBezier, RectF};
use crate::config::{HyprConfig, LayoutMode};
use dwm_hollow::BridgedWin32Window;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedClient {
    pub id: u64, // HWND or Wayland surface ID
    pub pid: u32,
    pub title: String,
    pub class_name: String,
    pub exe_name: String,
    pub workspace_id: u32,
    pub floating: bool,
    pub fullscreen: bool,
    pub focused: bool,
    pub shared_surface_handle: u64,
    pub win32k_update_id: u64,
    pub start_rect: RectF,
    pub current_rect: RectF,
    pub target_rect: RectF,
    pub anim_progress: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: u32,
    pub name: String,
    pub windows: usize,
    pub has_fullscreen: bool,
}

/// Which corner quadrant of a window was grabbed during `SUPER + Right-Click` resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeCorner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Special scratchpad workspace ID (`special:scratchpad`, toggled with `SUPER + S`).
pub const SPECIAL_WORKSPACE_ID: u32 = 99;

#[derive(Debug, Clone)]
pub struct WorkspaceScene {
    pub active_workspace: u32,
    pub special_workspace_open: bool,
    pub monitor_width: i32,
    pub monitor_height: i32,
    /// Top/bottom/left/right margins reserved by `zwlr_layer_shell_v1` surfaces (e.g. `waybar`).
    pub reserved_margins: [i32; 4], // [top, bottom, left, right]
    pub clients: HashMap<u64, ManagedClient>,
    pub workspace_order: HashMap<u32, Vec<u64>>,
    /// Per-workspace tabbed window group (`togglegroup` / `SUPER + G`) and currently active tab HWND.
    pub workspace_groups: HashMap<u32, Vec<u64>>,
    pub group_active: HashMap<u32, u64>,
    /// Per-split custom ratio override (0.15..=0.85) adjusted by `SUPER + Right-Click` drag on tiled windows.
    pub split_ratios: HashMap<(u32, usize), f32>,
    /// Window currently being dragged/scaled by mouse (`SUPER + Left/Right Click`), bypassing auto-sync rect overwrite.
    pub active_drag_hwnd: Option<u64>,
}

impl WorkspaceScene {
    pub fn new(monitor_width: i32, monitor_height: i32) -> Self {
        let mut workspace_order = HashMap::new();
        for ws in 1..=10 {
            workspace_order.insert(ws, Vec::new());
        }
        workspace_order.insert(SPECIAL_WORKSPACE_ID, Vec::new());
        Self {
            active_workspace: 1,
            special_workspace_open: false,
            monitor_width,
            monitor_height,
            reserved_margins: [40, 9, 9, 9], // [top: 40px hyprbar, bottom: 9px border, left: 9px border, right: 9px border]
            clients: HashMap::new(),
            workspace_order,
            workspace_groups: HashMap::new(),
            group_active: HashMap::new(),
            split_ratios: HashMap::new(),
            active_drag_hwnd: None,
        }
    }

    /// Synchronize the compositor scene with the latest bridged Win32 / Wayland windows.
    pub fn sync_bridged_windows(&mut self, bridged: &[BridgedWin32Window], cfg: &HyprConfig) {
        let mut seen = std::collections::HashSet::with_capacity(bridged.len());

        // Keep windows that belong to inactive workspaces (or hidden scratchpad) so
        // EnumWindows skips or off-screen coordinates never erase or overwrite their rects!
        for (&id, client) in &self.clients {
            if client.workspace_id != self.active_workspace
                && !(client.workspace_id == SPECIAL_WORKSPACE_ID && self.special_workspace_open)
            {
                seen.insert(id);
            }
        }

        let any_bridged_focused = bridged.iter().any(|b| b.is_focused);

        for w in bridged {
            seen.insert(w.hwnd);
            if let Some(existing) = self.clients.get_mut(&w.hwnd) {
                existing.title = w.title.clone();
                if (!cfg.follow_mouse && any_bridged_focused)
                    && (existing.workspace_id == self.active_workspace
                        || (existing.workspace_id == SPECIAL_WORKSPACE_ID
                            && self.special_workspace_open))
                {
                    existing.focused = w.is_focused;
                }
                existing.shared_surface_handle = w.shared_surface_handle;
                existing.win32k_update_id = w.win32k_update_id;
            } else {
                let safe_x = if w.x >= 10_000 || w.x < -10_000 { 40 } else { w.x };
                let safe_y = if w.y >= 10_000 || w.y < -10_000 { 40 } else { w.y };
                let initial_rect = RectF::new(
                    safe_x as f32,
                    safe_y as f32,
                    w.width as f32,
                    w.height as f32,
                );
                let exe_lower = w.exe_name.to_ascii_lowercase();
                let is_openwith_or_dialog = exe_lower == "openwith.exe"
                    || exe_lower == "rundll32.exe"
                    || (exe_lower == "dllhost.exe" && w.width < 900 && w.height < 900)
                    || w.class_name.starts_with("Open With")
                    || w.class_name.contains("OpenWith")
                    || w.class_name == "#32770"
                    || w.class_name == "Shell_Flyout"
                    || w.class_name == "PopupWindowSiteBridge";
                let is_default_float = is_openwith_or_dialog
                    || matches!(
                        exe_lower.as_str(),
                        "taskmgr.exe" | "keypirinha-x64.exe" | "calc.exe" | "calculatorapp.exe"
                    );
                let rule_dec = cfg.evaluate_window_rules(&w.exe_name, &w.class_name, &w.title);
                let is_floating = rule_dec.float.unwrap_or(is_default_float);
                let is_fullscreen = rule_dec.fullscreen.unwrap_or(w.is_fullscreen_candidate);
                let target_ws = rule_dec.workspace.unwrap_or(self.active_workspace);

                let mut target_rect = initial_rect;
                if let Some((rw, rh)) = rule_dec.size {
                    target_rect.w = rw;
                    target_rect.h = rh;
                }
                if rule_dec.center || is_openwith_or_dialog || target_ws == SPECIAL_WORKSPACE_ID {
                    let sw = if target_ws == SPECIAL_WORKSPACE_ID {
                        (self.monitor_width as f32 * 0.70).round()
                    } else {
                        target_rect.w
                    };
                    let sh = if target_ws == SPECIAL_WORKSPACE_ID {
                        (self.monitor_height as f32 * 0.70).round()
                    } else {
                        target_rect.h
                    };
                    target_rect = RectF::new(
                        ((self.monitor_width as f32 - sw) * 0.5).round(),
                        ((self.monitor_height as f32 - sh) * 0.5).round(),
                        sw,
                        sh,
                    );
                }

                let client = ManagedClient {
                    id: w.hwnd,
                    pid: w.pid,
                    title: w.title.clone(),
                    class_name: w.class_name.clone(),
                    exe_name: w.exe_name.clone(),
                    workspace_id: target_ws,
                    floating: is_floating || target_ws == SPECIAL_WORKSPACE_ID,
                    fullscreen: is_fullscreen,
                    focused: w.is_focused,
                    shared_surface_handle: w.shared_surface_handle,
                    win32k_update_id: w.win32k_update_id,
                    start_rect: initial_rect,
                    current_rect: initial_rect,
                    target_rect,
                    anim_progress: 0.0,
                };
                self.clients.insert(w.hwnd, client);
                self.workspace_order
                    .entry(target_ws)
                    .or_default()
                    .push(w.hwnd);
            }
        }

        // Before removing unseen windows on the active workspace, check if any unseen window
        // was simply minimized (`IsIconic`) via its native `—` button; if so, restore it immediately
        // (`SW_SHOWNOACTIVATE`) and keep it alive in the workspace!
        for &id in self.clients.keys() {
            if !seen.contains(&id) && dwm_hollow::restore_if_minimized_or_maximized(id) {
                seen.insert(id);
            }
        }

        // Remove closed windows on the active workspace
        self.clients.retain(|id, _| seen.contains(id));
        for order in self.workspace_order.values_mut() {
            order.retain(|id| seen.contains(id));
        }
        for grp in self.workspace_groups.values_mut() {
            grp.retain(|id| seen.contains(id));
        }

        self.recalculate_layout(self.active_workspace, cfg);
    }

    /// Toggle whether `hwnd` belongs to the active workspace's tabbed group (`togglegroup` / `SUPER + G`).
    pub fn toggle_group(&mut self, hwnd: u64, cfg: &HyprConfig) -> bool {
        let ws = self.active_workspace;
        let grp = self.workspace_groups.entry(ws).or_default();
        if let Some(pos) = grp.iter().position(|&id| id == hwnd) {
            grp.remove(pos);
            if grp.len() <= 1 {
                grp.clear();
                self.group_active.remove(&ws);
            } else if self.group_active.get(&ws) == Some(&hwnd) {
                if let Some(&first) = grp.first() {
                    self.group_active.insert(ws, first);
                }
            }
            self.recalculate_layout(ws, cfg);
            false
        } else {
            if grp.is_empty() {
                grp.push(hwnd);
                // Also auto-group the adjacent tiled window on the workspace so `SUPER + G` immediately creates a 2-tab group
                if let Some(order) = self.workspace_order.get(&ws) {
                    for &other_id in order {
                        if other_id != hwnd {
                            if let Some(c) = self.clients.get(&other_id) {
                                if !c.floating && !c.fullscreen {
                                    grp.push(other_id);
                                    break;
                                }
                            }
                        }
                    }
                }
            } else {
                grp.push(hwnd);
            }
            self.group_active.insert(ws, hwnd);
            self.recalculate_layout(ws, cfg);
            true
        }
    }

    /// Cycle the active window inside the current workspace's tabbed group (`changegroupactive` / `ALT + TAB`).
    pub fn change_group_active(&mut self, forward: bool, cfg: &HyprConfig) -> Option<u64> {
        let ws = self.active_workspace;
        let next_id = {
            let grp = self.workspace_groups.get(&ws)?;
            if grp.len() < 2 {
                return None;
            }
            let cur_active = self.group_active.get(&ws).copied().unwrap_or(grp[0]);
            let cur_idx = grp.iter().position(|&id| id == cur_active).unwrap_or(0);
            let next_idx = if forward {
                (cur_idx + 1) % grp.len()
            } else if cur_idx == 0 {
                grp.len() - 1
            } else {
                cur_idx - 1
            };
            grp[next_idx]
        };
        self.group_active.insert(ws, next_id);
        self.recalculate_layout(ws, cfg);
        Some(next_id)
    }

    /// Toggle the `special:scratchpad` overlay workspace (`SUPER + S`).
    pub fn toggle_special_workspace(&mut self) -> bool {
        self.special_workspace_open = !self.special_workspace_open;
        let open = self.special_workspace_open;
        let mon_w = self.monitor_width as f32;
        let mon_h = self.monitor_height as f32;
        let sw = (mon_w * 0.72).round();
        let sh = (mon_h * 0.72).round();
        let cx = ((mon_w - sw) * 0.5).round();
        let cy = ((mon_h - sh) * 0.5).round();

        for client in self.clients.values_mut() {
            if client.workspace_id == SPECIAL_WORKSPACE_ID {
                if open {
                    client.start_rect = RectF::new(cx, -sh, sw, sh);
                    client.current_rect = client.start_rect;
                    client.target_rect = RectF::new(cx, cy, sw, sh);
                    client.anim_progress = 0.0;
                } else {
                    client.start_rect = client.current_rect;
                    client.target_rect = RectF::new(cx, -sh - 40.0, sw, sh);
                    client.anim_progress = 0.0;
                }
            }
        }
        open
    }

    /// Move a window into or out of `special:scratchpad` (`SUPER + SHIFT + S`).
    pub fn move_to_special_workspace(&mut self, hwnd_id: u64, cfg: &HyprConfig) {
        let mon_w = self.monitor_width as f32;
        let mon_h = self.monitor_height as f32;
        let active_ws = self.active_workspace;

        let Some(current_ws) = self.clients.get(&hwnd_id).map(|c| c.workspace_id) else {
            return;
        };

        if current_ws == SPECIAL_WORKSPACE_ID {
            // Restore from scratchpad back to active workspace
            if let Some(order) = self.workspace_order.get_mut(&SPECIAL_WORKSPACE_ID) {
                order.retain(|&id| id != hwnd_id);
            }
            if let Some(c) = self.clients.get_mut(&hwnd_id) {
                c.workspace_id = active_ws;
                c.floating = false;
                c.start_rect = c.current_rect;
                c.anim_progress = 0.0;
            }
            let order = self.workspace_order.entry(active_ws).or_default();
            if !order.contains(&hwnd_id) {
                order.push(hwnd_id);
            }
            self.recalculate_layout(active_ws, cfg);
        } else {
            // Send to scratchpad and open scratchpad
            if let Some(order) = self.workspace_order.get_mut(&current_ws) {
                order.retain(|&id| id != hwnd_id);
            }
            let sw = (mon_w * 0.72).round();
            let sh = (mon_h * 0.72).round();
            let cx = ((mon_w - sw) * 0.5).round();
            let cy = ((mon_h - sh) * 0.5).round();

            if let Some(c) = self.clients.get_mut(&hwnd_id) {
                c.workspace_id = SPECIAL_WORKSPACE_ID;
                c.floating = true;
                c.start_rect = c.current_rect;
                c.target_rect = RectF::new(cx, cy, sw, sh);
                c.anim_progress = 0.0;
            }
            let order = self.workspace_order.entry(SPECIAL_WORKSPACE_ID).or_default();
            if !order.contains(&hwnd_id) {
                order.push(hwnd_id);
            }
            self.special_workspace_open = true;
            self.recalculate_layout(current_ws, cfg);
        }
    }

    /// Compute Dwindle BSP or Master-Stack tile rectangles for `workspace_id`.
    pub fn recalculate_layout(&mut self, workspace_id: u32, cfg: &HyprConfig) {
        let Some(order) = self.workspace_order.get(&workspace_id).cloned() else {
            return;
        };

        let grp_list = self
            .workspace_groups
            .get(&workspace_id)
            .cloned()
            .unwrap_or_default();
        let active_grp_id = if grp_list.len() >= 2 {
            let preferred = self
                .group_active
                .get(&workspace_id)
                .copied()
                .unwrap_or(grp_list[0]);
            if grp_list.contains(&preferred) {
                Some(preferred)
            } else {
                grp_list.first().copied()
            }
        } else {
            None
        };

        let mut fullscreen_ids = Vec::new();
        let mut tiled_ids: Vec<u64> = Vec::with_capacity(order.len());
        let mut inactive_group_ids: Vec<u64> = Vec::new();

        for &id in &order {
            if let Some(c) = self.clients.get(&id) {
                if c.fullscreen {
                    fullscreen_ids.push(id);
                } else if !c.floating {
                    if let Some(act_id) = active_grp_id {
                        if grp_list.contains(&id) && id != act_id {
                            inactive_group_ids.push(id);
                            continue;
                        }
                    }
                    tiled_ids.push(id);
                }
            }
        }

        let full_area = RectF::new(
            0.0,
            0.0,
            self.monitor_width as f32,
            self.monitor_height as f32,
        );
        for id in fullscreen_ids {
            self.set_client_target_rect(id, full_area, cfg.animations_enabled);
        }

        let work_area = self.work_area(cfg);

        let targets = match cfg.layout {
            LayoutMode::Dwindle => compute_dwindle_rects_with_ratios(
                &tiled_ids,
                work_area,
                cfg.gaps_in as f32,
                workspace_id,
                &self.split_ratios,
            ),
            LayoutMode::Master => {
                let ratio = self
                    .split_ratios
                    .get(&(workspace_id, 0))
                    .copied()
                    .unwrap_or(cfg.master_mfact);
                compute_master_rects(&tiled_ids, work_area, cfg.gaps_in as f32, ratio)
            }
        };

        let mut group_slot_rect = work_area;
        for &(id, rect) in &targets {
            if Some(id) == active_grp_id {
                group_slot_rect = rect;
            }
        }

        for (id, rect) in targets {
            if Some(id) == self.active_drag_hwnd {
                continue;
            }
            self.set_client_target_rect(id, rect, cfg.animations_enabled);
        }

        // Park inactive tabs of the window group off-screen with matching size so only the active tab is visible
        for id in inactive_group_ids {
            let parked = RectF::new(25000.0, 25000.0, group_slot_rect.w, group_slot_rect.h);
            self.set_client_target_rect(id, parked, false);
        }
    }

    pub fn work_area(&self, cfg: &HyprConfig) -> RectF {
        RectF::new(
            (self.reserved_margins[2] + cfg.gaps_out) as f32,
            (self.reserved_margins[0] + cfg.gaps_out) as f32,
            (self.monitor_width
                - self.reserved_margins[2]
                - self.reserved_margins[3]
                - cfg.gaps_out * 2)
                .max(100) as f32,
            (self.monitor_height
                - self.reserved_margins[0]
                - self.reserved_margins[1]
                - cfg.gaps_out * 2)
                .max(100) as f32,
        )
    }

    fn set_client_target_rect(&mut self, id: u64, rect: RectF, animate: bool) {
        if let Some(client) = self.clients.get_mut(&id) {
            let target_changed = (client.target_rect.x - rect.x).abs() > 0.5
                || (client.target_rect.y - rect.y).abs() > 0.5
                || (client.target_rect.w - rect.w).abs() > 0.5
                || (client.target_rect.h - rect.h).abs() > 0.5;
            let current_displaced = client.anim_progress >= 1.0
                && ((client.current_rect.x - rect.x).abs() > 0.5
                    || (client.current_rect.y - rect.y).abs() > 0.5
                    || (client.current_rect.w - rect.w).abs() > 0.5
                    || (client.current_rect.h - rect.h).abs() > 0.5);
            if target_changed || current_displaced {
                client.start_rect = client.current_rect;
                client.target_rect = rect;
                client.anim_progress = if animate { 0.0 } else { 1.0 };
                if !animate {
                    client.current_rect = rect;
                }
            }
        }
    }

    /// Toggle floating mode on `hwnd`. When switching from tiled -> floating, centers and scales to 65% of screen.
    pub fn toggle_floating(&mut self, hwnd: u64, cfg: &HyprConfig) -> Option<bool> {
        let work = self.work_area(cfg);
        let is_now_floating = {
            let client = self.clients.get_mut(&hwnd)?;
            client.floating = !client.floating;
            client.fullscreen = false;
            client.floating
        };

        if is_now_floating {
            let fw = (work.w * 0.62).round();
            let fh = (work.h * 0.65).round();
            let fx = (work.x + (work.w - fw) * 0.5).round();
            let fy = (work.y + (work.h - fh) * 0.5).round();
            self.set_client_target_rect(hwnd, RectF::new(fx, fy, fw, fh), cfg.animations_enabled);
        }

        self.recalculate_layout(self.active_workspace, cfg);
        Some(is_now_floating)
    }

    /// Toggle fullscreen state on `hwnd`.
    pub fn toggle_fullscreen(&mut self, hwnd: u64, cfg: &HyprConfig) -> Option<bool> {
        let (ws, is_now_fs) = {
            let client = self.clients.get_mut(&hwnd)?;
            client.fullscreen = !client.fullscreen;
            (client.workspace_id, client.fullscreen)
        };
        if is_now_fs {
            for c in self.clients.values_mut() {
                if c.workspace_id == ws && c.id != hwnd {
                    c.fullscreen = false;
                }
                if c.workspace_id == ws {
                    c.focused = c.id == hwnd;
                }
            }
        }
        self.recalculate_layout(ws, cfg);
        Some(is_now_fs)
    }

    /// Swap two windows inside the active workspace's tiling order (used by `SUPER + Left-Click`
    /// tile drop and `SUPER + SHIFT + H/J/K/L`).
    pub fn swap_windows_in_workspace(&mut self, id_a: u64, id_b: u64, cfg: &HyprConfig) -> bool {
        if id_a == id_b {
            return false;
        }
        let ws = self.active_workspace;
        if let Some(order) = self.workspace_order.get_mut(&ws) {
            let pos_a = order.iter().position(|&x| x == id_a);
            let pos_b = order.iter().position(|&x| x == id_b);
            if let (Some(ia), Some(ib)) = (pos_a, pos_b) {
                order.swap(ia, ib);
                self.recalculate_layout(ws, cfg);
                return true;
            }
        }
        false
    }

    /// Find the nearest neighbor window in `direction` (`"l"`, `"r"`, `"u"`, `"d"`) relative to `from_hwnd`.
    pub fn find_neighbor_in_direction(&self, from_hwnd: u64, direction: &str) -> Option<u64> {
        let from = self.clients.get(&from_hwnd)?;
        let cx = from.current_rect.x + from.current_rect.w * 0.5;
        let cy = from.current_rect.y + from.current_rect.h * 0.5;

        let mut best_id = None;
        let mut best_score = f32::MAX;

        for (&id, cand) in &self.clients {
            if id == from_hwnd || cand.workspace_id != self.active_workspace {
                continue;
            }
            let tx = cand.current_rect.x + cand.current_rect.w * 0.5;
            let ty = cand.current_rect.y + cand.current_rect.h * 0.5;
            let dx = tx - cx;
            let dy = ty - cy;

            let valid = match direction {
                "l" | "left" => dx < -10.0 && dx.abs() >= dy.abs() * 0.5,
                "r" | "right" => dx > 10.0 && dx.abs() >= dy.abs() * 0.5,
                "u" | "up" => dy < -10.0 && dy.abs() >= dx.abs() * 0.5,
                "d" | "down" => dy > 10.0 && dy.abs() >= dx.abs() * 0.5,
                _ => false,
            };
            if valid {
                let dist = dx * dx + dy * dy;
                if dist < best_score {
                    best_score = dist;
                    best_id = Some(id);
                }
            }
        }
        best_id
    }

    /// Move `hwnd` from its current workspace to `target_ws` (`1..=10`).
    pub fn move_window_to_workspace(&mut self, hwnd: u64, target_ws: u32, cfg: &HyprConfig) -> bool {
        let target_ws = target_ws.clamp(1, 10);
        let Some(old_ws) = self.clients.get(&hwnd).map(|c| c.workspace_id) else {
            return false;
        };
        if old_ws == target_ws {
            return false;
        }
        if let Some(client) = self.clients.get_mut(&hwnd) {
            client.workspace_id = target_ws;
        }
        if let Some(old_list) = self.workspace_order.get_mut(&old_ws) {
            old_list.retain(|&id| id != hwnd);
        }
        self.workspace_order.entry(target_ws).or_default().push(hwnd);
        self.recalculate_layout(old_ws, cfg);
        self.recalculate_layout(target_ws, cfg);
        true
    }

    /// Find which tiled window on the active workspace contains screen point `(px, py)`
    /// (excluding `ignore_hwnd`).
    pub fn tiled_window_at_point(&self, px: f32, py: f32, ignore_hwnd: Option<u64>) -> Option<u64> {
        let ws = self.active_workspace;
        let order = self.workspace_order.get(&ws)?;
        for &id in order.iter().rev() {
            if Some(id) == ignore_hwnd {
                continue;
            }
            if let Some(c) = self.clients.get(&id) {
                if !c.floating
                    && !c.fullscreen
                    && px >= c.target_rect.x
                    && px <= c.target_rect.x + c.target_rect.w
                    && py >= c.target_rect.y
                    && py <= c.target_rect.y + c.target_rect.h
                {
                    return Some(id);
                }
            }
        }
        None
    }

    /// Find which window on the active workspace contains screen point `(px, py)` for `follow_mouse = 1`
    /// (checking open scratchpad windows first, then floating windows, then tiled `target_rect`s,
    /// with a nearest-tile fallback when the cursor sits inside `gaps_in` / `gaps_out`).
    pub fn window_at_point(&self, px: f32, py: f32) -> Option<u64> {
        if self.special_workspace_open {
            if let Some(sp_order) = self.workspace_order.get(&SPECIAL_WORKSPACE_ID) {
                for &id in sp_order.iter().rev() {
                    if let Some(c) = self.clients.get(&id) {
                        let r = c.target_rect;
                        if px >= r.x && px <= r.x + r.w && py >= r.y && py <= r.y + r.h {
                            return Some(id);
                        }
                    }
                }
            }
        }

        let ws = self.active_workspace;
        let order = self.workspace_order.get(&ws)?;

        // Fullscreen tiles always have the highest Z-order on their workspace
        for &id in order.iter().rev() {
            if let Some(c) = self.clients.get(&id) {
                if c.fullscreen {
                    return Some(id);
                }
            }
        }

        for check_floating in [true, false] {
            for &id in order.iter().rev() {
                if let Some(c) = self.clients.get(&id) {
                    if c.floating == check_floating {
                        let r = if c.floating { c.current_rect } else { c.target_rect };
                        if px >= r.x && px <= r.x + r.w && py >= r.y && py <= r.y + r.h {
                            return Some(id);
                        }
                    }
                }
            }
        }

        // Fallback if cursor is resting inside a 5px gap between tiles on the active workspace
        let mut best_id = None;
        let mut best_dist_sq = f32::MAX;
        for &id in order {
            if let Some(c) = self.clients.get(&id) {
                let r = if c.floating { c.current_rect } else { c.target_rect };
                let cx = px.clamp(r.x, r.x + r.w);
                let cy = py.clamp(r.y, r.y + r.h);
                let d2 = (px - cx) * (px - cx) + (py - cy) * (py - cy);
                if d2 < best_dist_sq {
                    best_dist_sq = d2;
                    best_id = Some(id);
                }
            }
        }
        best_id
    }

    /// Adjust both horizontal and vertical split ratios for ANY tiled window (`0..N`)
    /// when `SUPER + Right-Click` dragging on a tiled workspace.
    pub fn adjust_tiled_split_ratio(
        &mut self,
        hwnd: u64,
        corner: ResizeCorner,
        delta_px_x: f32,
        delta_px_y: f32,
        cfg: &HyprConfig,
    ) {
        let ws = self.active_workspace;
        let Some(order) = self.workspace_order.get(&ws) else {
            return;
        };
        let tiled_ids: Vec<u64> = order
            .iter()
            .copied()
            .filter(|id| {
                self.clients
                    .get(id)
                    .map(|c| !c.floating && !c.fullscreen)
                    .unwrap_or(false)
            })
            .collect();

        let n = tiled_ids.len();
        if n < 2 {
            return;
        }

        let idx = tiled_ids.iter().position(|&id| id == hwnd).unwrap_or(0);
        let work = self.work_area(cfg);

        if cfg.layout == LayoutMode::Master {
            if delta_px_x.abs() > 0.1 {
                let current = self
                    .split_ratios
                    .get(&(ws, 0))
                    .copied()
                    .unwrap_or(cfg.master_mfact);
                let delta_ratio = delta_px_x / work.w.max(600.0);
                self.split_ratios
                    .insert((ws, 0), (current + delta_ratio).clamp(0.18, 0.82));
            }
            self.recalculate_layout(ws, cfg);
            return;
        }

        // Dwindle BSP: even split indices (0, 2, 4, ...) are Horizontal splits;
        // odd split indices (1, 3, 5, ...) are Vertical splits.
        if delta_px_x.abs() > 0.1 {
            let prefer_right_edge =
                matches!(corner, ResizeCorner::TopRight | ResizeCorner::BottomRight);
            let h_split = if idx % 2 == 0 && idx + 1 < n && (prefer_right_edge || idx == 0) {
                Some(idx)
            } else {
                (0..idx).rev().find(|&s| s % 2 == 0)
            };

            if let Some(s) = h_split {
                // Estimate the container width at split depth `s` so mouse motion maps 1:1 to pixels
                let depth_h = (s / 2) as i32;
                let container_w = (work.w * 0.5_f32.powi(depth_h)).max(400.0);
                let cur = self.split_ratios.get(&(ws, s)).copied().unwrap_or(0.5);
                let next = (cur + delta_px_x / container_w).clamp(0.18, 0.82);
                self.split_ratios.insert((ws, s), next);
            }
        }

        if delta_px_y.abs() > 0.1 && n >= 3 {
            let prefer_bottom_edge =
                matches!(corner, ResizeCorner::BottomLeft | ResizeCorner::BottomRight);
            let v_split = if idx % 2 == 1 && idx + 1 < n && (prefer_bottom_edge || idx == 1) {
                Some(idx)
            } else {
                (0..idx).rev().find(|&s| s % 2 == 1)
            };

            if let Some(s) = v_split {
                let depth_v = (s / 2) as i32;
                let container_h = (work.h * 0.5_f32.powi(depth_v)).max(300.0);
                let cur = self.split_ratios.get(&(ws, s)).copied().unwrap_or(0.5);
                let next = (cur + delta_px_y / container_h).clamp(0.18, 0.82);
                self.split_ratios.insert((ws, s), next);
            }
        }

        self.recalculate_layout(ws, cfg);
    }

    /// Step all active window bezier animations forward by `dt_sec` (e.g. `1/144s` or `1/240s`).
    /// Returns `true` if any window is still animating (damage active).
    pub fn tick_animations(&mut self, dt_sec: f32, cfg: &HyprConfig) -> bool {
        let curve = cfg
            .beziers
            .get("myBezier")
            .copied()
            .unwrap_or_else(CubicBezier::default);
        let speed = 5.5_f32; // ~180ms snappy Hyprland transition
        let mut any_animating = false;

        for client in self.clients.values_mut() {
            if Some(client.id) == self.active_drag_hwnd {
                continue;
            }
            if client.anim_progress < 1.0 {
                client.anim_progress = (client.anim_progress + dt_sec * speed).min(1.0);
                let factor = curve.evaluate(client.anim_progress);
                client.current_rect = RectF::lerp(client.start_rect, client.target_rect, factor);
                any_animating = true;
            }
        }
        any_animating
    }

    pub fn workspace_summaries(&self) -> Vec<WorkspaceInfo> {
        let mut out = Vec::new();
        for ws in 1..=10 {
            let ids = self.workspace_order.get(&ws).cloned().unwrap_or_default();
            if !ids.is_empty() || ws == self.active_workspace {
                let has_fullscreen = ids.iter().any(|id| {
                    self.clients.get(id).map(|c| c.fullscreen).unwrap_or(false)
                });
                out.push(WorkspaceInfo {
                    id: ws,
                    name: ws.to_string(),
                    windows: ids.len(),
                    has_fullscreen,
                });
            }
        }
        out
    }
}

/// Recursive Binary Space Partitioning (Dwindle spiral) layout calculator.
pub fn compute_dwindle_rects(ids: &[u64], area: RectF, gap: f32) -> Vec<(u64, RectF)> {
    compute_dwindle_rects_with_ratios(ids, area, gap, 1, &HashMap::new())
}

pub fn compute_dwindle_rects_with_ratios(
    ids: &[u64],
    area: RectF,
    gap: f32,
    workspace_id: u32,
    ratios: &HashMap<(u32, usize), f32>,
) -> Vec<(u64, RectF)> {
    let mut out = Vec::with_capacity(ids.len());
    let mut current_box = area;

    for (i, &id) in ids.iter().enumerate() {
        let is_last = i + 1 == ids.len();
        if is_last {
            out.push((id, current_box));
            break;
        }

        let ratio = ratios
            .get(&(workspace_id, i))
            .copied()
            .unwrap_or(0.5)
            .clamp(0.18, 0.82);

        // Strictly alternate Horizontal (even `i`) and Vertical (odd `i`) splits in the Dwindle spiral
        // so ultrawide monitors (e.g. 3440x1440) never double-split horizontally into narrow strips
        // that violate Win32 minimum window widths (`WM_GETMINMAXINFO`).
        let split_horizontally = i % 2 == 0;

        if split_horizontally {
            let avail_w = (current_box.w - gap * 2.0).max(120.0);
            let left_w = avail_w * ratio;
            let right_w = avail_w - left_w;
            let left = RectF::new(current_box.x, current_box.y, left_w, current_box.h);
            out.push((id, left));
            current_box = RectF::new(
                current_box.x + left_w + gap * 2.0,
                current_box.y,
                right_w,
                current_box.h,
            );
        } else {
            let avail_h = (current_box.h - gap * 2.0).max(120.0);
            let top_h = avail_h * ratio;
            let bot_h = avail_h - top_h;
            let top = RectF::new(current_box.x, current_box.y, current_box.w, top_h);
            out.push((id, top));
            current_box = RectF::new(
                current_box.x,
                current_box.y + top_h + gap * 2.0,
                current_box.w,
                bot_h,
            );
        }
    }
    out
}

/// Master-Stack layout calculator (`master_mfact` controls master column width ratio).
pub fn compute_master_rects(
    ids: &[u64],
    area: RectF,
    gap: f32,
    mfact: f32,
) -> Vec<(u64, RectF)> {
    if ids.is_empty() {
        return Vec::new();
    }
    if ids.len() == 1 {
        return vec![(ids[0], area)];
    }

    let mut out = Vec::with_capacity(ids.len());
    let master_w = (area.w - gap * 2.0) * mfact.clamp(0.18, 0.82);
    let stack_w = (area.w - gap * 2.0) - master_w;

    out.push((ids[0], RectF::new(area.x, area.y, master_w, area.h)));

    let stack_count = (ids.len() - 1) as f32;
    let total_gaps = gap * 2.0 * (stack_count - 1.0);
    let item_h = ((area.h - total_gaps) / stack_count).max(32.0);
    let stack_x = area.x + master_w + gap * 2.0;

    for (idx, &id) in ids[1..].iter().enumerate() {
        let y = area.y + (idx as f32) * (item_h + gap * 2.0);
        out.push((id, RectF::new(stack_x, y, stack_w, item_h)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dwindle_splits_three_windows() {
        let area = RectF::new(0.0, 0.0, 1920.0, 1080.0);
        let rects = compute_dwindle_rects(&[1, 2, 3], area, 5.0);
        assert_eq!(rects.len(), 3);
        assert!((rects[0].1.w - 955.0).abs() < 1.0);
        assert!((rects[1].1.h - 535.0).abs() < 1.0);
    }
}
