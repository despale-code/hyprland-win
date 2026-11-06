//! `hyprland-win-core` — Hyprland-compatible configuration, Dwindle/Master tiling engine,
//! Cubic Bezier animation evaluator, and `hyprctl` IPC protocol.

pub mod bezier;
pub mod config;
pub mod ipc;
pub mod layout;

pub use bezier::{CubicBezier, RectF};
pub use config::{
    hyprland_win_config_dir, hyprland_win_config_file, HyprConfig, Keybind, LayoutMode,
    WindowRuleDecision, WindowRuleEffect, WindowRuleV2,
};
pub use ipc::{
    HyprBarStateSnapshot, HyprIpcRequest, HyprIpcResponse, HyprNotifyMessage,
    HYPRLAND_WIN_IPC_PIPE, HYPRNOTIFY_IPC_PIPE,
};
pub use layout::{
    compute_dwindle_rects, compute_master_rects, ManagedClient, ResizeCorner, WorkspaceInfo,
    WorkspaceScene, SPECIAL_WORKSPACE_ID,
};
