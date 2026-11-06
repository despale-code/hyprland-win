//! `dwm-hollow` — Windows DWM Hollower & `Win32Wayland` Zero-Copy Texture Bridge.

pub mod bridge;
pub mod hollow;
pub mod shared_surface;

pub use bridge::{enumerate_manageable_windows, BridgedWin32Window};
pub use hollow::{
    apply_window_geometry, apply_window_geometry_async, force_focus_window,
    force_release_stuck_os_modifiers, hollow_window_dwm_attributes, init_foreground_lock_bypass,
    kill_explorer_shell, move_window_async, query_invisible_frame_margins, restore_explorer_shell,
    restore_if_minimized_or_maximized, restore_window_dwm_attributes, set_window_cloaked,
    set_window_opacity, suppress_explorer_shell_surfaces, OFFSCREEN_PARK_X_BASE,
};
pub use shared_surface::{query_window_shared_texture, SharedWindowTexture, SurfaceCaptureKind};
