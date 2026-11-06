//! Low-Level Win32 Keyboard (`WH_KEYBOARD_LL`) & Mouse (`WH_MOUSE_LL`) Hook Engine
//! for Hyprland-Win Keybinds and `SUPER` + Mouse Window Dragging/Scaling (`bindm`).
//!
//! Features:
//! - `SUPER + Left-Click Drag`: Move floating windows at 1000Hz (`SWP_ASYNCWINDOWPOS`)
//!   or drag-and-swap tiled windows across the Dwindle/Master tree.
//! - `SUPER + Right-Click Drag`: Scale/resize floating windows from the nearest corner
//!   quadrant (`TopLeft`, `TopRight`, `BottomLeft`, `BottomRight`) or resize tiled splits.
//! - `SUPER + Middle-Click` (or `SUPER + V`): Toggle window between Tiled and Floating.
//! - `SUPER + Scroll Wheel`: Cycle through Workspaces `1..=10`.
//! - Suppresses the stock Windows 11 Start Menu when `SUPER` is used as a modifier chord.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::OnceLock;
use tokio::sync::mpsc;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, GetAsyncKeyState, KEYEVENTF_KEYUP, VK_DOWN, VK_F24, VK_LEFT, VK_LMENU, VK_LSHIFT,
    VK_LWIN, VK_MENU, VK_RIGHT, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetAncestor, GetForegroundWindow, GetMessageW, GetWindowRect,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WindowFromPoint, GA_ROOT,
    KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use hyprland_win_core::{RectF, ResizeCorner};

/// Events dispatched from the low-level input thread to the Hyprland-Win compositor state machine.
#[derive(Debug, Clone)]
pub enum InputAction {
    SwitchWorkspace(u32),
    CycleWorkspace(i32),
    MoveActiveToWorkspace(u32),
    MoveActiveToWorkspaceSilent(u32),
    ToggleSpecialWorkspace,
    MoveActiveToSpecialWorkspace,
    ToggleFloating(Option<u64>),
    ToggleFullscreen,
    TogglePhysicalTiling,
    ToggleLayoutMode,
    ToggleGroup,
    ChangeGroupActive(bool),
    FocusDirection(&'static str),
    SwapDirection(&'static str),
    ResizeActive(i32, i32),
    CenterActiveWindow,
    CloseActiveWindow,
    ExecCommand(String),
    ReloadConfig,
    ReloadBar,
    PanicExitAndRestoreExplorer,
    BeginDragMove {
        hwnd: u64,
        cursor_x: i32,
        cursor_y: i32,
        initial_rect: RectF,
    },
    EndDragMove {
        hwnd: u64,
    },
    BeginDragResize {
        hwnd: u64,
        cursor_x: i32,
        cursor_y: i32,
        initial_rect: RectF,
        corner: ResizeCorner,
    },
    EndDragResize {
        hwnd: u64,
    },
    /// Coalesced zero-backlog notification that the mouse cursor moved during an active
    /// `SUPER + Left-Click` move or `SUPER + Right-Click` resize.
    PollActiveDrag,
    /// Coalesced notification that the mouse cursor moved (`follow_mouse = 1` focus tracking).
    MouseHoverMoved,
}

/// Snapshot of the latest coalesced mouse drag/resize state (always 0 queue lag).
#[derive(Debug, Clone, Copy)]
pub struct CoalescedDragUpdate {
    pub mode: i32, // 1 = Move, 2 = Resize
    pub hwnd: u64,
    pub cursor_x: i32,
    pub cursor_y: i32,
    pub dx: i32,
    pub dy: i32,
    pub step_dx: i32,
    pub step_dy: i32,
    pub initial_rect: RectF,
    pub corner: ResizeCorner,
}

static INPUT_TX: OnceLock<mpsc::UnboundedSender<InputAction>> = OnceLock::new();

#[derive(Debug, Clone)]
struct CompiledKeybind {
    need_super: bool,
    need_shift: bool,
    need_alt: bool,
    need_ctrl: bool,
    on_release: bool,
    vk: u16,
    action: InputAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseBindTarget {
    LeftButton,
    RightButton,
    MiddleButton,
    WheelDown,
    WheelUp,
}

#[derive(Debug, Clone)]
enum MouseBindBehavior {
    DragMove,
    DragResize,
    Action(InputAction),
}

#[derive(Debug, Clone)]
struct CompiledMousebind {
    need_super: bool,
    need_shift: bool,
    need_alt: bool,
    need_ctrl: bool,
    target: MouseBindTarget,
    behavior: MouseBindBehavior,
}

static LIVE_KEYBINDS: OnceLock<std::sync::RwLock<Vec<CompiledKeybind>>> = OnceLock::new();
static LIVE_MOUSEBINDS: OnceLock<std::sync::RwLock<Vec<CompiledMousebind>>> = OnceLock::new();

fn parse_mouse_target(key: &str) -> Option<MouseBindTarget> {
    match key.trim().to_ascii_lowercase().as_str() {
        "mouse:272" | "mouse_left" | "lmb" => Some(MouseBindTarget::LeftButton),
        "mouse:273" | "mouse_right" | "rmb" => Some(MouseBindTarget::RightButton),
        "mouse:274" | "mouse_middle" | "mmb" => Some(MouseBindTarget::MiddleButton),
        "mouse_down" | "wheel_down" => Some(MouseBindTarget::WheelDown),
        "mouse_up" | "wheel_up" => Some(MouseBindTarget::WheelUp),
        _ => None,
    }
}

fn parse_key_name_to_vk(key: &str) -> Option<u16> {
    let u = key.trim().to_ascii_uppercase();
    if u.len() == 1 {
        let b = u.as_bytes()[0];
        if b.is_ascii_uppercase() || b.is_ascii_digit() {
            return Some(b as u16);
        }
    }
    match u.as_str() {
        "SPACE" | "SPACEBAR" => Some(0x20),
        "RETURN" | "ENTER" | "KP_ENTER" => Some(0x0D),
        "TAB" => Some(0x09),
        "ESCAPE" | "ESC" => Some(0x1B),
        "BACKSPACE" | "BACK" => Some(0x08),
        "DELETE" | "DEL" => Some(0x2E),
        "INSERT" | "INS" => Some(0x2D),
        "HOME" => Some(0x24),
        "END" => Some(0x23),
        "PAGEUP" | "PGUP" | "PRIOR" => Some(0x21),
        "PAGEDOWN" | "PGDN" | "NEXT" => Some(0x22),
        "PRINT" | "PRINTSCREEN" | "SNAPSHOT" | "SYSRQ" => Some(0x2C),
        "LEFT" => Some(VK_LEFT.0),
        "RIGHT" => Some(VK_RIGHT.0),
        "UP" => Some(VK_UP.0),
        "DOWN" => Some(VK_DOWN.0),
        "F1" => Some(0x70),
        "F2" => Some(0x71),
        "F3" => Some(0x72),
        "F4" => Some(0x73),
        "F5" => Some(0x74),
        "F6" => Some(0x75),
        "F7" => Some(0x76),
        "F8" => Some(0x77),
        "F9" => Some(0x78),
        "F10" => Some(0x79),
        "F11" => Some(0x7A),
        "F12" => Some(0x7B),
        "F13" => Some(0x7C),
        "F14" => Some(0x7D),
        "F15" => Some(0x7E),
        "F16" => Some(0x7F),
        "F17" => Some(0x80),
        "F18" => Some(0x81),
        "F19" => Some(0x82),
        "F20" => Some(0x83),
        "F21" => Some(0x84),
        "F22" => Some(0x85),
        "F23" => Some(0x86),
        "F24" => Some(0x87),
        "CODE:10" => Some(b'1' as u16),
        "CODE:11" => Some(b'2' as u16),
        "CODE:12" => Some(b'3' as u16),
        "CODE:13" => Some(b'4' as u16),
        "CODE:14" => Some(b'5' as u16),
        "CODE:15" => Some(b'6' as u16),
        "CODE:16" => Some(b'7' as u16),
        "CODE:17" => Some(b'8' as u16),
        "CODE:18" => Some(b'9' as u16),
        "CODE:19" => Some(b'0' as u16),
        "KP_0" | "NUMPAD0" => Some(0x60),
        "KP_1" | "NUMPAD1" => Some(0x61),
        "KP_2" | "NUMPAD2" => Some(0x62),
        "KP_3" | "NUMPAD3" => Some(0x63),
        "KP_4" | "NUMPAD4" => Some(0x64),
        "KP_5" | "NUMPAD5" => Some(0x65),
        "KP_6" | "NUMPAD6" => Some(0x66),
        "KP_7" | "NUMPAD7" => Some(0x67),
        "KP_8" | "NUMPAD8" => Some(0x68),
        "KP_9" | "NUMPAD9" => Some(0x69),
        "KP_MULTIPLY" | "ASTERISK" => Some(0x6A),
        "KP_ADD" | "PLUS" => Some(0x6B),
        "KP_SUBTRACT" => Some(0x6D),
        "KP_DECIMAL" => Some(0x6E),
        "KP_DIVIDE" => Some(0x6F),
        "XF86AUDIOMUTE" => Some(0xAD),
        "XF86AUDIOLOWERVOLUME" => Some(0xAE),
        "XF86AUDIORAISEVOLUME" => Some(0xAF),
        "XF86AUDIONEXT" => Some(0xB0),
        "XF86AUDIOPREV" => Some(0xB1),
        "XF86AUDIOSTOP" => Some(0xB2),
        "XF86AUDIOPLAY" | "XF86AUDIOPAUSE" => Some(0xB3),
        "COMMA" | "," => Some(0xBC),
        "PERIOD" | "." => Some(0xBE),
        "SLASH" | "/" => Some(0xBF),
        "SEMICOLON" | ";" => Some(0xBA),
        "APOSTROPHE" | "'" => Some(0xDE),
        "BRACKETLEFT" | "[" => Some(0xDB),
        "BRACKETRIGHT" | "]" => Some(0xDD),
        "BACKSLASH" | "\\" => Some(0xDC),
        "MINUS" | "-" => Some(0xBD),
        "EQUAL" | "=" => Some(0xBB),
        "GRAVE" | "`" | "TILDE" => Some(0xC0),
        _ => None,
    }
}

fn map_dispatcher_to_action(dispatcher: &str, arg: &str) -> Option<InputAction> {
    let d = dispatcher.trim().to_ascii_lowercase();
    let a = arg.trim();
    match d.as_str() {
        "exec" | "execr" => {
            if !a.is_empty() {
                Some(InputAction::ExecCommand(a.to_string()))
            } else {
                None
            }
        }
        "killactive" | "closewindow" | "forcekillactive" => Some(InputAction::CloseActiveWindow),
        "exit" => Some(InputAction::PanicExitAndRestoreExplorer),
        "togglefloating" | "setfloating" | "settiled" => Some(InputAction::ToggleFloating(None)),
        "fullscreen" | "fakefullscreen" => Some(InputAction::ToggleFullscreen),
        "togglesplit" | "pseudo" | "layout" | "togglelayout" | "swapnext" => {
            Some(InputAction::ToggleLayoutMode)
        }
        "tiling" | "toggletiling" => Some(InputAction::TogglePhysicalTiling),
        "togglegroup" => Some(InputAction::ToggleGroup),
        "changegroupactive" => {
            let forward = !a.eq_ignore_ascii_case("b") && !a.eq_ignore_ascii_case("prev");
            Some(InputAction::ChangeGroupActive(forward))
        }
        "togglespecialworkspace" => Some(InputAction::ToggleSpecialWorkspace),
        "movetoworkspacesilent" => {
            if a.to_ascii_lowercase().starts_with("special") {
                Some(InputAction::MoveActiveToSpecialWorkspace)
            } else if let Ok(ws) = a.parse::<u32>() {
                let norm = if ws == 0 { 10 } else { ws.clamp(1, 10) };
                Some(InputAction::MoveActiveToWorkspaceSilent(norm))
            } else {
                None
            }
        }
        "movetoworkspace" => {
            if a.to_ascii_lowercase().starts_with("special") {
                Some(InputAction::MoveActiveToSpecialWorkspace)
            } else if let Ok(ws) = a.parse::<u32>() {
                let norm = if ws == 0 { 10 } else { ws.clamp(1, 10) };
                Some(InputAction::MoveActiveToWorkspace(norm))
            } else {
                None
            }
        }
        "workspace" => {
            if a.to_ascii_lowercase().starts_with("special") {
                Some(InputAction::ToggleSpecialWorkspace)
            } else if matches!(
                a.to_ascii_lowercase().as_str(),
                "e+1" | "+1" | "r+1" | "m+1" | "next"
            ) {
                Some(InputAction::CycleWorkspace(1))
            } else if matches!(
                a.to_ascii_lowercase().as_str(),
                "e-1" | "-1" | "r-1" | "m-1" | "prev" | "previous"
            ) {
                Some(InputAction::CycleWorkspace(-1))
            } else if let Ok(ws) = a.parse::<u32>() {
                let norm = if ws == 0 { 10 } else { ws.clamp(1, 10) };
                Some(InputAction::SwitchWorkspace(norm))
            } else {
                None
            }
        }
        "movefocus" => {
            let dir = match a.to_ascii_lowercase().as_str() {
                "l" | "left" => "l",
                "r" | "right" => "r",
                "u" | "up" => "u",
                "d" | "down" => "d",
                _ => return None,
            };
            Some(InputAction::FocusDirection(dir))
        }
        "swapwindow" | "movewindow" => {
            let dir = match a.to_ascii_lowercase().as_str() {
                "l" | "left" => "l",
                "r" | "right" => "r",
                "u" | "up" => "u",
                "d" | "down" => "d",
                _ => return None,
            };
            Some(InputAction::SwapDirection(dir))
        }
        "resizeactive" | "resizewindowpixel" => {
            let mut nums = a.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty());
            let dx = nums.next().and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
            let dy = nums.next().and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
            if dx != 0 || dy != 0 {
                Some(InputAction::ResizeActive(dx, dy))
            } else {
                None
            }
        }
        "centerwindow" => Some(InputAction::CenterActiveWindow),
        "reload" | "forcerendererreload" => Some(InputAction::ReloadConfig),
        "reloadbar" => Some(InputAction::ReloadBar),
        _ => None,
    }
}

fn parse_modifier_flags(modifiers: &str) -> (bool, bool, bool, bool) {
    let mods_upper = modifiers.to_ascii_uppercase();
    let need_super = mods_upper.contains("SUPER")
        || mods_upper.contains("WIN")
        || mods_upper.contains("MAINMOD")
        || mods_upper.contains("MOD4")
        || mods_upper.contains("LOGO");
    let need_shift = mods_upper.contains("SHIFT");
    let need_alt = mods_upper.contains("ALT") || mods_upper.contains("MOD1");
    let need_ctrl = mods_upper.contains("CTRL") || mods_upper.contains("CONTROL");
    (need_super, need_shift, need_alt, need_ctrl)
}

/// Compiles and hot-swaps the active keybind and mousebind tables from `HyprConfig::binds` (`hyprland.conf`).
pub fn update_live_keybinds(binds: &[hyprland_win_core::Keybind]) {
    let mut compiled_keys = Vec::with_capacity(binds.len());
    let mut compiled_mouse = Vec::new();

    for b in binds {
        let (need_super, need_shift, need_alt, need_ctrl) = parse_modifier_flags(&b.modifiers);

        if let Some(mouse_target) = parse_mouse_target(&b.key) {
            let d_lower = b.dispatcher.trim().to_ascii_lowercase();
            let behavior = if d_lower == "movewindow" && b.arg.trim().is_empty() {
                Some(MouseBindBehavior::DragMove)
            } else if d_lower == "resizewindow" && b.arg.trim().is_empty() {
                Some(MouseBindBehavior::DragResize)
            } else {
                map_dispatcher_to_action(&b.dispatcher, &b.arg).map(MouseBindBehavior::Action)
            };

            if let Some(behavior) = behavior {
                compiled_mouse.push(CompiledMousebind {
                    need_super,
                    need_shift,
                    need_alt,
                    need_ctrl,
                    target: mouse_target,
                    behavior,
                });
            }
            continue;
        }

        let Some(vk) = parse_key_name_to_vk(&b.key) else {
            continue;
        };
        let Some(action) = map_dispatcher_to_action(&b.dispatcher, &b.arg) else {
            continue;
        };
        let on_release = b.flags.contains('r');

        compiled_keys.push(CompiledKeybind {
            need_super,
            need_shift,
            need_alt,
            need_ctrl,
            on_release,
            vk,
            action,
        });
    }

    let k_lock = LIVE_KEYBINDS.get_or_init(|| std::sync::RwLock::new(Vec::new()));
    if let Ok(mut guard) = k_lock.write() {
        *guard = compiled_keys;
    }

    let m_lock = LIVE_MOUSEBINDS.get_or_init(|| std::sync::RwLock::new(Vec::new()));
    if let Ok(mut guard) = m_lock.write() {
        *guard = compiled_mouse;
    }
}

static SUPER_DOWN: AtomicBool = AtomicBool::new(false);
static SUPER_LAST_EVENT_MS: AtomicU64 = AtomicU64::new(0);
static SHIFT_DOWN: AtomicBool = AtomicBool::new(false);
static ALT_DOWN: AtomicBool = AtomicBool::new(false);
static CTRL_DOWN: AtomicBool = AtomicBool::new(false);
static SUPER_CHORD_USED: AtomicBool = AtomicBool::new(false);

#[inline]
fn now_tick_ms() -> u64 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}

#[inline]
fn is_super_effectively_held() -> bool {
    if !SUPER_DOWN.load(Ordering::SeqCst) {
        return false;
    }
    let elapsed = now_tick_ms().wrapping_sub(SUPER_LAST_EVENT_MS.load(Ordering::SeqCst));

    // If > 2.5 seconds have elapsed with zero SUPER activity while not dragging (e.g., if a
    // Windows UAC / secure-desktop transition swallowed WM_KEYUP), self-heal SUPER_DOWN.
    if elapsed > 2_500 && DRAG_MODE.load(Ordering::SeqCst) == 0 {
        SUPER_DOWN.store(false, Ordering::SeqCst);
        SUPER_CHORD_USED.store(false, Ordering::SeqCst);
        dwm_hollow::force_release_stuck_os_modifiers();
        return false;
    }
    true
}

fn trigger_emergency_panic_shutdown() {
    SUPER_DOWN.store(false, Ordering::SeqCst);
    SHIFT_DOWN.store(false, Ordering::SeqCst);
    ALT_DOWN.store(false, Ordering::SeqCst);
    CTRL_DOWN.store(false, Ordering::SeqCst);
    DRAG_MODE.store(0, Ordering::SeqCst);
    dwm_hollow::force_release_stuck_os_modifiers();
    send_action(InputAction::PanicExitAndRestoreExplorer);
    // Hardware-level failsafe thread in case the async loop is ever blocked
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(250));
        for w in dwm_hollow::enumerate_manageable_windows(None) {
            let hwnd = HWND(w.hwnd as *mut core::ffi::c_void);
            let _ = dwm_hollow::restore_window_dwm_attributes(hwnd);
        }
        dwm_hollow::restore_explorer_shell();
        std::process::exit(0);
    });
}

// Active drag state tracked atomically for <0.01ms hook response and 0 queue backlog
// 0 = None, 1 = Move (Left-Click), 2 = Resize (Right-Click)
static DRAG_MODE: AtomicI32 = AtomicI32::new(0);
static DRAG_HWND: AtomicU64 = AtomicU64::new(0);
static DRAG_START_CURSOR_X: AtomicI32 = AtomicI32::new(0);
static DRAG_START_CURSOR_Y: AtomicI32 = AtomicI32::new(0);
static DRAG_CUR_CURSOR_X: AtomicI32 = AtomicI32::new(0);
static DRAG_CUR_CURSOR_Y: AtomicI32 = AtomicI32::new(0);
static DRAG_LAST_CONSUMED_X: AtomicI32 = AtomicI32::new(0);
static DRAG_LAST_CONSUMED_Y: AtomicI32 = AtomicI32::new(0);
static DRAG_WIN_X: AtomicI32 = AtomicI32::new(0);
static DRAG_WIN_Y: AtomicI32 = AtomicI32::new(0);
static DRAG_WIN_W: AtomicI32 = AtomicI32::new(0);
static DRAG_WIN_H: AtomicI32 = AtomicI32::new(0);
// 0 = TopLeft, 1 = TopRight, 2 = BottomLeft, 3 = BottomRight
static DRAG_CORNER: AtomicI32 = AtomicI32::new(3);
static DRAG_PENDING_DISPATCH: AtomicBool = AtomicBool::new(false);

static HOVER_CURSOR_X: AtomicI32 = AtomicI32::new(0);
static HOVER_CURSOR_Y: AtomicI32 = AtomicI32::new(0);
static HOVER_PENDING_DISPATCH: AtomicBool = AtomicBool::new(false);

/// Consume the latest coalesced hover cursor coordinates for `follow_mouse = 1`.
pub fn take_latest_hover_point() -> (i32, i32) {
    HOVER_PENDING_DISPATCH.store(false, Ordering::SeqCst);
    (
        HOVER_CURSOR_X.load(Ordering::SeqCst),
        HOVER_CURSOR_Y.load(Ordering::SeqCst),
    )
}

/// Consume the latest coalesced mouse drag/resize position in O(1) with zero backlog.
pub fn take_latest_drag_state() -> Option<CoalescedDragUpdate> {
    DRAG_PENDING_DISPATCH.store(false, Ordering::SeqCst);
    let mode = DRAG_MODE.load(Ordering::SeqCst);
    let hwnd = DRAG_HWND.load(Ordering::SeqCst);
    if mode == 0 || hwnd == 0 {
        return None;
    }
    let cur_x = DRAG_CUR_CURSOR_X.load(Ordering::SeqCst);
    let cur_y = DRAG_CUR_CURSOR_Y.load(Ordering::SeqCst);
    let start_x = DRAG_START_CURSOR_X.load(Ordering::SeqCst);
    let start_y = DRAG_START_CURSOR_Y.load(Ordering::SeqCst);
    let prev_x = DRAG_LAST_CONSUMED_X.swap(cur_x, Ordering::SeqCst);
    let prev_y = DRAG_LAST_CONSUMED_Y.swap(cur_y, Ordering::SeqCst);

    let corner = match DRAG_CORNER.load(Ordering::SeqCst) {
        0 => ResizeCorner::TopLeft,
        1 => ResizeCorner::TopRight,
        2 => ResizeCorner::BottomLeft,
        _ => ResizeCorner::BottomRight,
    };

    Some(CoalescedDragUpdate {
        mode,
        hwnd,
        cursor_x: cur_x,
        cursor_y: cur_y,
        dx: cur_x - start_x,
        dy: cur_y - start_y,
        step_dx: cur_x - prev_x,
        step_dy: cur_y - prev_y,
        initial_rect: RectF::new(
            DRAG_WIN_X.load(Ordering::SeqCst) as f32,
            DRAG_WIN_Y.load(Ordering::SeqCst) as f32,
            DRAG_WIN_W.load(Ordering::SeqCst) as f32,
            DRAG_WIN_H.load(Ordering::SeqCst) as f32,
        ),
        corner,
    })
}

/// Spawn a dedicated Win32 GUI message-pump thread that installs `WH_KEYBOARD_LL` and
/// `WH_MOUSE_LL` hooks and streams `InputAction` events to the compositor.
pub fn spawn_input_hook_thread(tx: mpsc::UnboundedSender<InputAction>) {
    let _ = INPUT_TX.set(tx);

    std::thread::Builder::new()
        .name("hyprland-win-input-ll".to_string())
        .spawn(move || unsafe {
            crate::attach_current_thread_to_default_desktop();
            dwm_hollow::force_release_stuck_os_modifiers();
            let kbd_hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), None, 0)
                .expect("Failed to install WH_KEYBOARD_LL hook");
            let mouse_hook = SetWindowsHookExW(WH_MOUSE_LL, Some(ll_mouse_proc), None, 0)
                .expect("Failed to install WH_MOUSE_LL hook");

            tracing::info!(
                "Installed low-level keyboard & mouse hooks (SUPER+LClick=Move, SUPER+RClick=Scale, SUPER+MClick/V=Float)"
            );

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            let _ = UnhookWindowsHookEx(kbd_hook);
            let _ = UnhookWindowsHookEx(mouse_hook);
        })
        .expect("Failed to spawn hyprland-win-input-ll thread");
}

#[inline]
fn send_action(action: InputAction) {
    if let Some(tx) = INPUT_TX.get() {
        let _ = tx.send(action);
    }
}

/// Mask `LWIN`/`RWIN` or `ALT` menu activation so `explorer.exe` / `DefWindowProcW` never pops up
/// the Windows 11 Start Menu or the Win32 `Alt+Space` / `SC_KEYMENU` titlebar context menu.
#[inline]
unsafe fn suppress_windows_start_menu() {
    SUPER_CHORD_USED.store(true, Ordering::SeqCst);
    // Sending a harmless VK_F24 keytap breaks both Explorer's Start Menu trigger and Win32 SC_KEYMENU
    keybd_event(VK_F24.0 as u8, 0, Default::default(), 0);
    keybd_event(VK_F24.0 as u8, 0, KEYEVENTF_KEYUP, 0);
}

#[inline]
unsafe fn is_lockscreen_active() -> bool {
    let cls: [u16; 21] = [
        b'H' as u16, b'y' as u16, b'p' as u16, b'r' as u16, b'L' as u16, b'o' as u16, b'c' as u16,
        b'k' as u16, b'O' as u16, b'v' as u16, b'e' as u16, b'r' as u16, b'l' as u16, b'a' as u16,
        b'y' as u16, b'C' as u16, b'l' as u16, b'a' as u16, b's' as u16, b's' as u16, 0,
    ];
    if let Ok(h) = windows::Win32::UI::WindowsAndMessaging::FindWindowW(
        windows::core::PCWSTR(cls.as_ptr()),
        windows::core::PCWSTR::null(),
    ) {
        return !h.0.is_null();
    }
    false
}

unsafe extern "system" fn ll_keyboard_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
    let vk = kb.vkCode as u16;
    // Ignore our own synthesized VK_F24 keytaps
    if vk == VK_F24.0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let msg = wparam.0 as u32;
    let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP || (kb.flags.0 & 0x80) != 0;
    let is_down = !is_up && (msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN);

    // Always process modifier key releases BEFORE checking `is_lockscreen_active()` so releasing
    // a key while `hyprlock` is open can never leave `SUPER_DOWN` or `ALT_DOWN` stuck `true`!
    if is_up {
        if vk == VK_LWIN.0 || vk == VK_RWIN.0 {
            SUPER_DOWN.store(false, Ordering::SeqCst);
            SUPER_CHORD_USED.store(false, Ordering::SeqCst);
            return LRESULT(1);
        }
        if vk == VK_SHIFT.0 || vk == VK_LSHIFT.0 || vk == VK_RSHIFT.0 {
            SHIFT_DOWN.store(false, Ordering::SeqCst);
        }
        if vk == VK_MENU.0 || vk == VK_LMENU.0 || vk == VK_RMENU.0 {
            ALT_DOWN.store(false, Ordering::SeqCst);
        }
        if vk == 0x11 || vk == 0xA2 || vk == 0xA3 {
            CTRL_DOWN.store(false, Ordering::SeqCst);
        }
    }

    if is_lockscreen_active() {
        SUPER_DOWN.store(false, Ordering::SeqCst);
        SHIFT_DOWN.store(false, Ordering::SeqCst);
        ALT_DOWN.store(false, Ordering::SeqCst);
        CTRL_DOWN.store(false, Ordering::SeqCst);
        if vk == VK_LWIN.0 || vk == VK_RWIN.0 {
            return LRESULT(1);
        }
        return CallNextHookEx(None, code, wparam, lparam);
    }

    // Track and swallow SUPER (Left/Right Windows Key) completely (`LRESULT(1)`) so neither
    // Windows nor any focused window ever receives raw Windows key input!
    if vk == VK_LWIN.0 || vk == VK_RWIN.0 {
        if is_down {
            SUPER_LAST_EVENT_MS.store(now_tick_ms(), Ordering::SeqCst);
            if !SUPER_DOWN.swap(true, Ordering::SeqCst) {
                suppress_windows_start_menu();
            }
        }
        return LRESULT(1);
    }

    // Track SHIFT and CTRL directly from the low-level hook stream
    if vk == VK_SHIFT.0 || vk == VK_LSHIFT.0 || vk == VK_RSHIFT.0 {
        if is_down {
            SHIFT_DOWN.store(true, Ordering::SeqCst);
        }
        return CallNextHookEx(None, code, wparam, lparam);
    }
    if vk == 0x11 || vk == 0xA2 || vk == 0xA3 {
        if is_down {
            CTRL_DOWN.store(true, Ordering::SeqCst);
        }
        return CallNextHookEx(None, code, wparam, lparam);
    }

    // Track ALT (VK_MENU / VK_LMENU / VK_RMENU) and disarm Win32 `SC_KEYMENU` menu bar activation
    if vk == VK_MENU.0 || vk == VK_LMENU.0 || vk == VK_RMENU.0 {
        if is_down {
            if !ALT_DOWN.swap(true, Ordering::SeqCst) {
                suppress_windows_start_menu();
            }
        } else if is_up {
            suppress_windows_start_menu();
        }
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let super_held = is_super_effectively_held()
        || (GetAsyncKeyState(VK_LWIN.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_RWIN.0 as i32) as u16 & 0x8000) != 0;

    // Unconditionally disarm Windows' `Win + P` (0x50) projection shortcut so `win32kfull.sys` /
    // `DisplaySwitch.exe` can never hijack `Win + P`.
    if super_held && vk == 0x50 {
        suppress_windows_start_menu();
    }

    let shift = SHIFT_DOWN.load(Ordering::SeqCst)
        || (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_LSHIFT.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_RSHIFT.0 as i32) as u16 & 0x8000) != 0;
    let alt = ALT_DOWN.load(Ordering::SeqCst)
        || (kb.flags.0 & 0x20) != 0
        || (GetAsyncKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_LMENU.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_RMENU.0 as i32) as u16 & 0x8000) != 0;
    let ctrl = CTRL_DOWN.load(Ordering::SeqCst)
        || (GetAsyncKeyState(0x11) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(0xA2) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(0xA3) as u16 & 0x8000) != 0;

    // Universal Escape self-heal: pressing bare `Escape` (0x1B) clears any stale drag or modifier state
    if vk == 0x1B && !shift && !alt && !ctrl {
        SUPER_DOWN.store(false, Ordering::SeqCst);
        DRAG_MODE.store(0, Ordering::SeqCst);
    }

    // Hardware Failsafe Emergency Panic Combo: `Ctrl + Alt + Backspace` (0x08)
    if is_down && ctrl && alt && vk == 0x08 {
        trigger_emergency_panic_shutdown();
        return LRESULT(1);
    }

    // ------------------------------------------------------------------------
    // 1. EVALUATE LIVE CONFIGURED KEYBINDS FROM `<userfolder>/.config/hyprwin/hyprland.conf`
    // ------------------------------------------------------------------------
    if is_down || is_up {
        let mut matched_action: Option<InputAction> = None;
        let mut has_any_live_binds = false;
        if let Some(lock) = LIVE_KEYBINDS.get() {
            if let Ok(binds) = lock.read() {
                has_any_live_binds = !binds.is_empty();
                // Search in reverse so later `bind = ...` lines in `hyprland.conf` override earlier ones
                for b in binds.iter().rev() {
                    if b.vk == vk
                        && b.on_release == is_up
                        && b.need_super == super_held
                        && b.need_shift == shift
                        && b.need_alt == alt
                        && b.need_ctrl == ctrl
                    {
                        matched_action = Some(b.action.clone());
                        break;
                    }
                }
            }
        }

        if let Some(action) = matched_action {
            if super_held {
                SUPER_LAST_EVENT_MS.store(now_tick_ms(), Ordering::SeqCst);
            }
            if alt {
                suppress_windows_start_menu();
            }
            if let InputAction::SwitchWorkspace(ws) = action {
                notify_hyprbar_fast_workspace(ws);
            }
            if let InputAction::PanicExitAndRestoreExplorer = action {
                trigger_emergency_panic_shutdown();
                return LRESULT(1);
            }
            send_action(action);
            return LRESULT(1);
        }

        // Only if `LIVE_KEYBINDS` is completely empty (e.g. missing/corrupt config file),
        // provide minimal emergency survival binds so the user is never stranded.
        if is_down && !has_any_live_binds && super_held && !ctrl && !alt && !shift {
            match vk {
                0x54 => send_action(InputAction::ExecCommand("alacritty.exe".to_string())),
                0x52 | 0x20 => {
                    send_action(InputAction::ExecCommand("hyprlauncher.exe --toggle".to_string()))
                }
                0x57 => send_action(InputAction::CloseActiveWindow),
                0x4D => trigger_emergency_panic_shutdown(),
                _ => {}
            }
            return LRESULT(1);
        }
    }

    // Suppress Windows 11 system menu popups (`Alt+Esc`, `Alt+F6`, `Alt+Minus`, `Alt+F10`, `Ctrl+Esc`)
    if !super_held && alt && !ctrl && matches!(vk, 0x1B | 0x75 | 0xBD | 0x79) {
        suppress_windows_start_menu();
        return LRESULT(1);
    }
    if !super_held && ctrl && !alt && !shift && vk == 0x1B {
        suppress_windows_start_menu();
        return LRESULT(1);
    }

    if !super_held {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    // While SUPER is held, swallow ALL non-modifier key events (both key-down and key-up)
    // so focused windows NEVER receive raw Windows-key chords!
    LRESULT(1)
}

unsafe fn notify_hyprbar_fast_workspace(ws: u32) {
    let vk_equiv: usize = if ws == 10 {
        0x30
    } else {
        (0x30 + ws.clamp(1, 9)) as usize
    };
    let bar_cls: [u16; 18] = [
        b'H' as u16, b'y' as u16, b'p' as u16, b'r' as u16, b'B' as u16, b'a' as u16,
        b'r' as u16, b'T' as u16, b'o' as u16, b'p' as u16, b'O' as u16, b'v' as u16,
        b'e' as u16, b'r' as u16, b'l' as u16, b'a' as u16, b'y' as u16, 0,
    ];
    if let Ok(bar_hwnd) = windows::Win32::UI::WindowsAndMessaging::FindWindowW(
        windows::core::PCWSTR(bar_cls.as_ptr()),
        windows::core::PCWSTR::null(),
    ) {
        if !bar_hwnd.0.is_null() {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                bar_hwnd,
                0x8000 + 77,
                WPARAM(vk_equiv),
                LPARAM(0),
            );
        }
    }
}

unsafe fn match_live_mousebind(
    target: MouseBindTarget,
    super_held: bool,
    shift: bool,
    alt: bool,
    ctrl: bool,
) -> Option<MouseBindBehavior> {
    if let Some(lock) = LIVE_MOUSEBINDS.get() {
        if let Ok(binds) = lock.read() {
            for b in binds.iter().rev() {
                if b.target == target
                    && b.need_super == super_held
                    && b.need_shift == shift
                    && b.need_alt == alt
                    && b.need_ctrl == ctrl
                {
                    return Some(b.behavior.clone());
                }
            }
        }
    }
    None
}

unsafe fn execute_mouse_behavior(behavior: MouseBindBehavior, pt: POINT) -> bool {
    match behavior {
        MouseBindBehavior::DragMove => {
            if let Some((root_hwnd, rect)) = top_level_window_at(pt) {
                DRAG_HWND.store(root_hwnd, Ordering::SeqCst);
                DRAG_START_CURSOR_X.store(pt.x, Ordering::SeqCst);
                DRAG_START_CURSOR_Y.store(pt.y, Ordering::SeqCst);
                DRAG_CUR_CURSOR_X.store(pt.x, Ordering::SeqCst);
                DRAG_CUR_CURSOR_Y.store(pt.y, Ordering::SeqCst);
                DRAG_LAST_CONSUMED_X.store(pt.x, Ordering::SeqCst);
                DRAG_LAST_CONSUMED_Y.store(pt.y, Ordering::SeqCst);
                DRAG_WIN_X.store(rect.left, Ordering::SeqCst);
                DRAG_WIN_Y.store(rect.top, Ordering::SeqCst);
                DRAG_WIN_W.store(rect.right - rect.left, Ordering::SeqCst);
                DRAG_WIN_H.store(rect.bottom - rect.top, Ordering::SeqCst);
                DRAG_PENDING_DISPATCH.store(false, Ordering::SeqCst);
                DRAG_MODE.store(1, Ordering::SeqCst);

                send_action(InputAction::BeginDragMove {
                    hwnd: root_hwnd,
                    cursor_x: pt.x,
                    cursor_y: pt.y,
                    initial_rect: RectF::new(
                        rect.left as f32,
                        rect.top as f32,
                        (rect.right - rect.left) as f32,
                        (rect.bottom - rect.top) as f32,
                    ),
                });
                return true;
            }
            false
        }
        MouseBindBehavior::DragResize => {
            if let Some((root_hwnd, rect)) = top_level_window_at(pt) {
                let w = (rect.right - rect.left).max(1);
                let h = (rect.bottom - rect.top).max(1);
                let rel_x = pt.x - rect.left;
                let rel_y = pt.y - rect.top;

                let (corner_enum, corner_id) = match (rel_x < w / 2, rel_y < h / 2) {
                    (true, true) => (ResizeCorner::TopLeft, 0),
                    (false, true) => (ResizeCorner::TopRight, 1),
                    (true, false) => (ResizeCorner::BottomLeft, 2),
                    (false, false) => (ResizeCorner::BottomRight, 3),
                };

                DRAG_HWND.store(root_hwnd, Ordering::SeqCst);
                DRAG_START_CURSOR_X.store(pt.x, Ordering::SeqCst);
                DRAG_START_CURSOR_Y.store(pt.y, Ordering::SeqCst);
                DRAG_CUR_CURSOR_X.store(pt.x, Ordering::SeqCst);
                DRAG_CUR_CURSOR_Y.store(pt.y, Ordering::SeqCst);
                DRAG_LAST_CONSUMED_X.store(pt.x, Ordering::SeqCst);
                DRAG_LAST_CONSUMED_Y.store(pt.y, Ordering::SeqCst);
                DRAG_WIN_X.store(rect.left, Ordering::SeqCst);
                DRAG_WIN_Y.store(rect.top, Ordering::SeqCst);
                DRAG_WIN_W.store(w, Ordering::SeqCst);
                DRAG_WIN_H.store(h, Ordering::SeqCst);
                DRAG_CORNER.store(corner_id, Ordering::SeqCst);
                DRAG_PENDING_DISPATCH.store(false, Ordering::SeqCst);
                DRAG_MODE.store(2, Ordering::SeqCst);

                send_action(InputAction::BeginDragResize {
                    hwnd: root_hwnd,
                    cursor_x: pt.x,
                    cursor_y: pt.y,
                    initial_rect: RectF::new(
                        rect.left as f32,
                        rect.top as f32,
                        w as f32,
                        h as f32,
                    ),
                    corner: corner_enum,
                });
                return true;
            }
            false
        }
        MouseBindBehavior::Action(InputAction::ToggleFloating(None)) => {
            if let Some((root_hwnd, _)) = top_level_window_at(pt) {
                send_action(InputAction::ToggleFloating(Some(root_hwnd)));
                return true;
            }
            send_action(InputAction::ToggleFloating(None));
            true
        }
        MouseBindBehavior::Action(act) => {
            if let InputAction::SwitchWorkspace(ws) = act {
                notify_hyprbar_fast_workspace(ws);
            }
            send_action(act);
            true
        }
    }
}

unsafe extern "system" fn ll_mouse_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code < 0 || is_lockscreen_active() {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let ms = &*(lparam.0 as *const MSLLHOOKSTRUCT);
    let msg = wparam.0 as u32;
    let super_held = is_super_effectively_held()
        || (GetAsyncKeyState(VK_LWIN.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_RWIN.0 as i32) as u16 & 0x8000) != 0;
    let shift = SHIFT_DOWN.load(Ordering::SeqCst)
        || (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
    let alt = ALT_DOWN.load(Ordering::SeqCst)
        || (GetAsyncKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0;
    let ctrl = CTRL_DOWN.load(Ordering::SeqCst)
        || (GetAsyncKeyState(0x11) as u16 & 0x8000) != 0;
    let current_drag = DRAG_MODE.load(Ordering::SeqCst);

    match msg {
        WM_LBUTTONDOWN => {
            if let Some(beh) =
                match_live_mousebind(MouseBindTarget::LeftButton, super_held, shift, alt, ctrl)
            {
                if execute_mouse_behavior(beh, ms.pt) {
                    return LRESULT(1);
                }
            }
        }
        WM_LBUTTONUP if current_drag != 0 => {
            let prev_mode = DRAG_MODE.swap(0, Ordering::SeqCst);
            DRAG_PENDING_DISPATCH.store(false, Ordering::SeqCst);
            let _ = is_super_effectively_held();
            let hwnd = DRAG_HWND.load(Ordering::SeqCst);
            if hwnd != 0 {
                if prev_mode == 1 {
                    send_action(InputAction::EndDragMove { hwnd });
                } else if prev_mode == 2 {
                    send_action(InputAction::EndDragResize { hwnd });
                }
            }
            return LRESULT(1);
        }

        WM_RBUTTONDOWN => {
            if let Some(beh) =
                match_live_mousebind(MouseBindTarget::RightButton, super_held, shift, alt, ctrl)
            {
                if execute_mouse_behavior(beh, ms.pt) {
                    return LRESULT(1);
                }
            }
        }
        WM_RBUTTONUP if current_drag != 0 => {
            let prev_mode = DRAG_MODE.swap(0, Ordering::SeqCst);
            DRAG_PENDING_DISPATCH.store(false, Ordering::SeqCst);
            let _ = is_super_effectively_held();
            let hwnd = DRAG_HWND.load(Ordering::SeqCst);
            if hwnd != 0 {
                if prev_mode == 2 {
                    send_action(InputAction::EndDragResize { hwnd });
                } else if prev_mode == 1 {
                    send_action(InputAction::EndDragMove { hwnd });
                }
            }
            return LRESULT(1);
        }

        WM_MBUTTONDOWN => {
            if let Some(beh) =
                match_live_mousebind(MouseBindTarget::MiddleButton, super_held, shift, alt, ctrl)
            {
                if execute_mouse_behavior(beh, ms.pt) {
                    return LRESULT(1);
                }
            }
        }

        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let delta = ((ms.mouseData >> 16) as i16) as i32;
            let target = if delta > 0 {
                Some(MouseBindTarget::WheelUp)
            } else if delta < 0 {
                Some(MouseBindTarget::WheelDown)
            } else {
                None
            };
            if let Some(t) = target {
                if let Some(beh) = match_live_mousebind(t, super_held, shift, alt, ctrl) {
                    if execute_mouse_behavior(beh, ms.pt) {
                        return LRESULT(1);
                    }
                }
            }
        }

        // Coalesced Real-Time Mouse Drag Move / Scale update (0 queue backlog)
        WM_MOUSEMOVE if current_drag != 0 => {
            DRAG_CUR_CURSOR_X.store(ms.pt.x, Ordering::SeqCst);
            DRAG_CUR_CURSOR_Y.store(ms.pt.y, Ordering::SeqCst);
            if !DRAG_PENDING_DISPATCH.swap(true, Ordering::SeqCst) {
                send_action(InputAction::PollActiveDrag);
            }
        }

        // `follow_mouse = 1`: Coalesced focus-follows-mouse hover tracking
        WM_MOUSEMOVE => {
            HOVER_CURSOR_X.store(ms.pt.x, Ordering::SeqCst);
            HOVER_CURSOR_Y.store(ms.pt.y, Ordering::SeqCst);
            if !HOVER_PENDING_DISPATCH.swap(true, Ordering::SeqCst) {
                send_action(InputAction::MouseHoverMoved);
            }
        }
        _ => {}
    }

    CallNextHookEx(None, code, wparam, lparam)
}

unsafe fn is_unmoveable_shell_or_companion_window(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return true;
    }
    let mut cls_buf = [0u16; 128];
    let len = windows::Win32::UI::WindowsAndMessaging::GetClassNameW(hwnd, &mut cls_buf);
    if len <= 0 {
        return false;
    }
    let cls = String::from_utf16_lossy(&cls_buf[..len as usize]);
    matches!(
        cls.as_str(),
        "HyprPaperBackdropClass"
            | "HyprBarTopOverlay"
            | "HyprBarCalendarPopup"
            | "HyprBarTooltip"
            | "HyprNotifyPopupClass"
            | "HyprLauncherClass"
            | "HyprLockOverlayClass"
            | "HyprShotOverlayClass"
            | "HyprlandWinDCompOverlayClass"
            | "HyprlandWinNativeDCompOverlay"
            | "Progman"
            | "WorkerW"
            | "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
    )
}

unsafe fn top_level_window_at(pt: POINT) -> Option<(u64, RECT)> {
    let child = WindowFromPoint(pt);
    let target = if child.0.is_null() {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return None;
        }
        fg
    } else {
        if is_unmoveable_shell_or_companion_window(child) {
            return None;
        }
        let root = GetAncestor(child, GA_ROOT);
        if root.0.is_null() { child } else { root }
    };
    if is_unmoveable_shell_or_companion_window(target) {
        return None;
    }
    let mut r = RECT::default();
    GetWindowRect(target, &mut r).ok()?;
    let (ml, mt, mr, mb) = dwm_hollow::query_invisible_frame_margins(target);
    r.left += ml;
    r.top += mt;
    r.right -= mr;
    r.bottom -= mb;
    Some((target.0 as u64, r))
}
