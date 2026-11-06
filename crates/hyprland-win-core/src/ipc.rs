//! `hyprctl` IPC Protocol over Windows Named Pipe (`\\.\pipe\hyprland-win-ipc`).
//!
//! Supports standard Hyprland IPC commands:
//! - `clients` / `clients -j`
//! - `workspaces` / `workspaces -j`
//! - `activewindow` / `activewindow -j`
//! - `dispatch <dispatcher> <arg>` (e.g. `dispatch workspace 2`, `dispatch togglefloating`)
//! - `keyword <key> <val>` (e.g. `keyword decoration:rounding 12`)
//! - `reload`
//! - `version` / `status`

use serde::{Deserialize, Serialize};

pub const HYPRLAND_WIN_IPC_PIPE: &str = r"\\.\pipe\hyprland-win-ipc";
pub const HYPRNOTIFY_IPC_PIPE: &str = r"\\.\pipe\hyprnotify-ipc";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyprNotifyMessage {
    pub summary: String,
    pub body: String,
    pub urgency: u8, // 0 = Low/Mint, 1 = Normal/Cyan, 2 = Critical/Coral
    pub timeout_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HyprIpcRequest {
    Clients { json: bool },
    Workspaces { json: bool },
    ActiveWindow { json: bool },
    Dispatch { dispatcher: String, arg: String },
    Keyword { key: String, value: String },
    Reload,
    Status { json: bool },
    BarState,
}

/// Complete single-trip snapshot of the compositor state for `hyprbar`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyprBarStateSnapshot {
    pub active_workspace: u32,
    pub occupied_workspaces: [u16; 10], // Window count per workspace 1..=10
    #[serde(default)]
    pub workspace_hwnds: [Vec<u64>; 10], // Ordered HWNDs per workspace 1..=10 for icon extraction
    pub layout_mode: String,            // "DWINDLE" or "MASTER"
    pub tiling_active: bool,
    pub active_class: String,
    pub active_title: String,
    pub active_floating: bool,
    pub active_fullscreen: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyprIpcResponse {
    pub ok: bool,
    pub payload: String,
}
