//! DWM Hollowing — strips stock Windows `uDWM.dll` decorations, Mica/Acrylic shaders,
//! drop shadows, and animations from managed `HWND`s, and cloaks windows on inactive
//! Hyprland workspaces while keeping their off-screen GPU textures live.

use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use anyhow::Result;
use windows::Win32::Foundation::{BOOL, HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmSetWindowAttribute, DWMNCRP_ENABLED, DWMWA_BORDER_COLOR, DWMWA_CLOAK,
    DWMWA_EXTENDED_FRAME_BOUNDS, DWMWA_NCRENDERING_POLICY, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_TRANSITIONS_FORCEDISABLED, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DEFAULT, DWMWCP_ROUND,
    DWMSBT_AUTO, DWMSBT_NONE,
};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYEVENTF_KEYUP, VK_F24};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, GetForegroundWindow, GetWindowRect,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsZoomed, SetForegroundWindow, SetWindowPos,
    ShowWindowAsync, SystemParametersInfoW, ASFW_ANY, HWND_TOP, SPIF_SENDCHANGE,
    SPI_SETFOREGROUNDLOCKTIMEOUT, SWP_ASYNCWINDOWPOS, SWP_DEFERERASE, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOCOPYBITS, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
    SW_SHOWNOACTIVATE,
};

/// `DWMWA_COLOR_NONE` constant (`0xFFFFFFFE`) to suppress `uDWM.dll` border drawing completely.
const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;
/// `DWMWA_COLOR_DEFAULT` constant (`0xFFFFFFFF`) to restore standard Windows border drawing.
const DWMWA_COLOR_DEFAULT: u32 = 0xFFFF_FFFF;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Off-screen X coordinate base where windows on inactive workspaces are parked.
pub const OFFSCREEN_PARK_X_BASE: i32 = 16_000;

/// Strip stock `uDWM.dll` visual overhead (Mica, Acrylic, native rounding, native 1px border,
/// and minimize/restore transitions) from `hwnd` WITHOUT disabling `DWMWA_NCRENDERING_POLICY`.
///
/// Keeping `DWMNCRP_ENABLED` is critical for apps like Zen Browser / Firefox (`xul.dll`) and
/// Chromium: setting `DWMNCRP_DISABLED` sends `WM_DWMNCRENDERINGCHANGED(FALSE)`, which tricks
/// Gecko/Chromium into thinking DWM composition is off (Windows Classic theme) and makes them
/// draw ugly legacy Windows Vista/Classic bevel borders.
pub fn hollow_window_dwm_attributes(hwnd: HWND) -> Result<()> {
    unsafe {
        // 1. Keep Non-Client DWM rendering ENABLED so Zen Browser / Firefox / Chromium stay in
        //    modern Client-Side Decoration (borderless) mode instead of falling back to legacy frames.
        let ncrp = DWMNCRP_ENABLED.0 as u32;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_NCRENDERING_POLICY,
            &ncrp as *const u32 as *const _,
            std::mem::size_of::<u32>() as u32,
        );

        // 2. Enable hardware anti-aliased corner rounding in DWM so window client corners match
        //    Hyprland-Win's SDF rounded gradient border overlay (`rounding = 8..10px`).
        let corner = DWMWCP_ROUND.0 as u32;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const u32 as *const _,
            std::mem::size_of::<u32>() as u32,
        );

        // 3. Suppress stock uDWM 1px border AND drop shadow natively via DWMWA_COLOR_NONE
        let border_none = DWMWA_COLOR_NONE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border_none as *const u32 as *const _,
            std::mem::size_of::<u32>() as u32,
        );

        // 4. Disable Mica / Acrylic system backdrop in uDWM
        let backdrop = DWMSBT_NONE.0 as u32;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop as *const u32 as *const _,
            std::mem::size_of::<u32>() as u32,
        );

        // 5. Disable stock DWM transitions so Hyprland-Win bezier animations drive geometry
        let disable_transitions = BOOL(1);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_TRANSITIONS_FORCEDISABLED,
            &disable_transitions as *const BOOL as *const _,
            std::mem::size_of::<BOOL>() as u32,
        );

        // 6. Send SWP_FRAMECHANGED so apps like Zen Browser / Firefox immediately recalculate
        //    WM_NCCALCSIZE with DWMNCRP_ENABLED and discard any legacy non-client border fallback.
        let _ = SetWindowPos(
            hwnd,
            HWND::default(),
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED
                | SWP_NOMOVE
                | SWP_NOSIZE
                | SWP_NOZORDER
                | SWP_NOACTIVATE
                | SWP_NOOWNERZORDER
                | SWP_ASYNCWINDOWPOS,
        );
    }
    Ok(())
}

/// Compute the invisible Win32 resize border margins (`GetWindowRect` vs `DWMWA_EXTENDED_FRAME_BOUNDS`)
/// so that when `Hyprland-Win` places a window at `(x, y, w, h)`, its visible pixels align flush with
/// the tile box without 7px invisible border gaps or legacy frame bleed.
#[inline]
pub fn query_invisible_frame_margins(hwnd: HWND) -> (i32, i32, i32, i32) {
    unsafe {
        let mut win_rect = RECT::default();
        let mut ext_rect = RECT::default();
        if GetWindowRect(hwnd, &mut win_rect).is_ok()
            && DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                &mut ext_rect as *mut RECT as *mut _,
                std::mem::size_of::<RECT>() as u32,
            )
            .is_ok()
        {
            let left = (ext_rect.left - win_rect.left).clamp(0, 16);
            let top = (ext_rect.top - win_rect.top).clamp(0, 16);
            let right = (win_rect.right - ext_rect.right).clamp(0, 16);
            let bottom = (win_rect.bottom - ext_rect.bottom).clamp(0, 16);
            return (left, top, right, bottom);
        }
    }
    (0, 0, 0, 0)
}

/// Per-window latest-state geometry command (never queues; newer commands overwrite older ones).
#[derive(Debug, Clone, Copy)]
enum PendingGeometry {
    MoveOnly { x: i32, y: i32 },
    MoveAndSize { x: i32, y: i32, w: i32, h: i32 },
}

struct WindowWorkerSlot {
    pending: Mutex<Option<PendingGeometry>>,
    cv: Condvar,
}

static WINDOW_WORKERS: OnceLock<Mutex<HashMap<u64, Arc<WindowWorkerSlot>>>> = OnceLock::new();

fn dispatch_to_window_worker(hwnd: HWND, cmd: PendingGeometry) {
    let key = hwnd.0 as u64;
    if key == 0 {
        return;
    }
    let map_mutex = WINDOW_WORKERS.get_or_init(|| Mutex::new(HashMap::new()));
    let slot = {
        let mut map = map_mutex.lock().unwrap();
        if let Some(existing) = map.get(&key) {
            Arc::clone(existing)
        } else {
            let new_slot = Arc::new(WindowWorkerSlot {
                pending: Mutex::new(None),
                cv: Condvar::new(),
            });
            map.insert(key, Arc::clone(&new_slot));
            spawn_per_window_geometry_thread(key, Arc::clone(&new_slot));
            new_slot
        }
    };

    {
        let mut guard = slot.pending.lock().unwrap();
        // If a MoveAndSize was already pending and a MoveOnly arrives, preserve the size;
        // otherwise overwrite in-place so 0 intermediate frames ever accumulate.
        *guard = Some(match (*guard, cmd) {
            (Some(PendingGeometry::MoveAndSize { w, h, .. }), PendingGeometry::MoveOnly { x, y }) => {
                PendingGeometry::MoveAndSize { x, y, w, h }
            }
            (_, latest) => latest,
        });
    }
    slot.cv.notify_one();
}

fn spawn_per_window_geometry_thread(hwnd_u64: u64, slot: Arc<WindowWorkerSlot>) {
    std::thread::Builder::new()
        .name(format!("hyprland-win-geom-{hwnd_u64:x}"))
        .spawn(move || unsafe {
            let hwnd = HWND(hwnd_u64 as *mut core::ffi::c_void);
            // Cache invisible border margins once on thread startup (0 ALPC calls to dwm.exe during drag!)
            let (ml, mt, mr, mb) = query_invisible_frame_margins(hwnd);
            let mut last_size: Option<(i32, i32)> = None;

            loop {
                let cmd = {
                    let mut guard = slot.pending.lock().unwrap();
                    while guard.is_none() {
                        let (g, timeout) = slot
                            .cv
                            .wait_timeout(guard, std::time::Duration::from_secs(5))
                            .unwrap();
                        guard = g;
                        if timeout.timed_out() && guard.is_none() {
                            if !IsWindow(hwnd).as_bool() {
                                if let Some(m) = WINDOW_WORKERS.get() {
                                    m.lock().unwrap().remove(&hwnd_u64);
                                }
                                return;
                            }
                        }
                    }
                    guard.take().unwrap()
                };

                if !IsWindow(hwnd).as_bool() {
                    if let Some(m) = WINDOW_WORKERS.get() {
                        m.lock().unwrap().remove(&hwnd_u64);
                    }
                    return;
                }

                // If the user clicked a native Maximize (□) or Minimize (—) button or double-clicked
                // the titlebar (`IsZoomed` / `IsIconic`), immediately restore to normal state (`SW_SHOWNOACTIVATE`)
                // so `SetWindowPos` can tile/position the window cleanly!
                if IsIconic(hwnd).as_bool() || IsZoomed(hwnd).as_bool() {
                    let _ = ShowWindowAsync(hwnd, SW_SHOWNOACTIVATE);
                }

                // Synchronous SetWindowPos (NO SWP_ASYNCWINDOWPOS) on this window's dedicated thread:
                // - All windows resize in parallel across threads (never waiting one-by-one).
                // - Provides natural backpressure: while this window is rendering its current resize frame,
                //   any new mouse movements overwrite `slot.pending` in RAM instead of flooding `win32kfull.sys`!
                match cmd {
                    PendingGeometry::MoveOnly { x, y } => {
                        let _ = SetWindowPos(
                            hwnd,
                            HWND::default(),
                            x - ml,
                            y - mt,
                            0,
                            0,
                            SWP_NOSIZE
                                | SWP_NOZORDER
                                | SWP_NOACTIVATE
                                | SWP_NOOWNERZORDER
                                | SWP_NOCOPYBITS
                                | SWP_DEFERERASE,
                        );
                    }
                    PendingGeometry::MoveAndSize { x, y, w, h } => {
                        let adj_w = (w + ml + mr).max(64);
                        let adj_h = (h + mt + mb).max(64);
                        let size_unchanged = last_size == Some((adj_w, adj_h));
                        last_size = Some((adj_w, adj_h));

                        let flags = if size_unchanged {
                            SWP_NOSIZE
                                | SWP_NOZORDER
                                | SWP_NOACTIVATE
                                | SWP_NOOWNERZORDER
                                | SWP_NOCOPYBITS
                                | SWP_DEFERERASE
                        } else {
                            SWP_NOZORDER
                                | SWP_NOACTIVATE
                                | SWP_NOOWNERZORDER
                                | SWP_NOCOPYBITS
                                | SWP_DEFERERASE
                        };

                        if SetWindowPos(
                            hwnd,
                            HWND::default(),
                            x - ml,
                            y - mt,
                            adj_w,
                            adj_h,
                            flags,
                        )
                        .is_err()
                        {
                            // Fallback if target HWND is UIPI-elevated and current process is non-elevated
                            elevated_set_window_pos_fallback(hwnd, x - ml, y - mt, adj_w, adj_h);
                        }
                    }
                }
            }
        })
        .ok();
}

static ELEVATED_ATTEMPTED: OnceLock<Mutex<std::collections::HashSet<u64>>> = OnceLock::new();

fn elevated_set_window_pos_fallback(hwnd: HWND, x: i32, y: i32, w: i32, h: i32) {
    let id = hwnd.0 as u64;
    let set_mutex = ELEVATED_ATTEMPTED.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    if let Ok(mut set) = set_mutex.lock() {
        if !set.insert(id) {
            return;
        }
    }
    let ps_script = format!(
        "Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class W {{ [DllImport(\"user32.dll\")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int ht, uint f); }}'; [W]::SetWindowPos([IntPtr]{id}, [IntPtr]0, {x}, {y}, {w}, {h}, 0x0014)"
    );
    let _ = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &format!(
                "Start-Process powershell.exe -Verb RunAs -WindowStyle Hidden -ArgumentList '-NoProfile -WindowStyle Hidden -Command {}'",
                ps_script.replace('\'', "''")
            ),
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

/// Park or unpark `hwnd` when switching Hyprland workspaces.
pub fn set_window_cloaked(hwnd: HWND, cloaked: bool) -> Result<()> {
    unsafe {
        let val = BOOL(i32::from(cloaked));
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_CLOAK,
            &val as *const BOOL as *const _,
            std::mem::size_of::<BOOL>() as u32,
        );
        if cloaked {
            dispatch_to_window_worker(
                hwnd,
                PendingGeometry::MoveOnly {
                    x: OFFSCREEN_PARK_X_BASE,
                    y: 0,
                },
            );
        }
    }
    Ok(())
}

/// If a managed window was minimized (`IsIconic`) or maximized (`IsZoomed`) via its native
/// titlebar buttons, restore it to normal (`SW_SHOWNOACTIVATE`) so it is never stranded
/// in a taskbar-less Hyprland-Win session or locked out of tiling geometry.
pub fn restore_if_minimized_or_maximized(hwnd_u64: u64) -> bool {
    unsafe {
        let hwnd = HWND(hwnd_u64 as *mut core::ffi::c_void);
        if !IsWindow(hwnd).as_bool() {
            return false;
        }
        if IsIconic(hwnd).as_bool() || IsZoomed(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_SHOWNOACTIVATE);
            return true;
        }
        false
    }
}

/// Synchronize the physical Win32 `HWND` rectangle with the logical Hyprland tile geometry.
pub fn apply_window_geometry(hwnd: HWND, x: i32, y: i32, width: i32, height: i32) -> Result<()> {
    dispatch_to_window_worker(
        hwnd,
        PendingGeometry::MoveAndSize {
            x,
            y,
            w: width,
            h: height,
        },
    );
    Ok(())
}

/// Non-blocking parallel per-window geometry dispatch (`0` queue backlog, all windows resize concurrently).
pub fn apply_window_geometry_async(
    hwnd: HWND,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Result<()> {
    dispatch_to_window_worker(
        hwnd,
        PendingGeometry::MoveAndSize {
            x,
            y,
            w: width,
            h: height,
        },
    );
    Ok(())
}

/// Fast move-only dispatch (`SWP_NOSIZE`) used during `SUPER + Left-Click` window dragging.
pub fn move_window_async(hwnd: HWND, x: i32, y: i32) -> Result<()> {
    dispatch_to_window_worker(hwnd, PendingGeometry::MoveOnly { x, y });
    Ok(())
}

/// Restore stock Windows 11 DWM attributes and uncloak `hwnd` when exiting Hyprland-Win
/// or triggering the `Win + Alt + Shift + W` panic recovery combo.
pub fn restore_window_dwm_attributes(hwnd: HWND) -> Result<()> {
    unsafe {
        // 1. Always uncloak and unpark off-screen windows so no window stays hidden on an inactive workspace
        let uncloak = BOOL(0);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_CLOAK,
            &uncloak as *const BOOL as *const _,
            std::mem::size_of::<BOOL>() as u32,
        );
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_ok() && rect.left >= OFFSCREEN_PARK_X_BASE - 2000 {
            let _ = SetWindowPos(
                hwnd,
                HWND::default(),
                120,
                80,
                0,
                0,
                SWP_ASYNCWINDOWPOS | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }

        // 2. Restore Non-Client area DWM rendering
        let ncrp = DWMNCRP_ENABLED.0 as u32;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_NCRENDERING_POLICY,
            &ncrp as *const u32 as *const _,
            std::mem::size_of::<u32>() as u32,
        );

        // 3. Restore default corner rounding
        let corner = DWMWCP_DEFAULT.0 as u32;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const u32 as *const _,
            std::mem::size_of::<u32>() as u32,
        );

        // 4. Restore default border color
        let border_def = DWMWA_COLOR_DEFAULT;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border_def as *const u32 as *const _,
            std::mem::size_of::<u32>() as u32,
        );

        // 5. Restore automatic backdrop
        let backdrop = DWMSBT_AUTO.0 as u32;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop as *const u32 as *const _,
            std::mem::size_of::<u32>() as u32,
        );

        // 6. Re-enable standard DWM transitions
        let enable_transitions = BOOL(0);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_TRANSITIONS_FORCEDISABLED,
            &enable_transitions as *const BOOL as *const _,
            std::mem::size_of::<BOOL>() as u32,
        );
    }
    Ok(())
}

/// Instantly suspend and terminate the Windows desktop shell (`explorer.exe`) in `< 0.1ms`
/// via `NtSuspendProcess` (`ntdll.dll`) + `TerminateProcess` so the taskbar, desktop icons,
/// and stock `Win` shortcuts never draw or intercept input.
pub fn kill_explorer_shell() {
    #[link(name = "ntdll")]
    extern "system" {
        fn NtSuspendProcess(process_handle: *mut core::ffi::c_void) -> i32;
    }
    unsafe {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_SUSPEND_RESUME, PROCESS_TERMINATE,
        };
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, ShowWindow, SW_HIDE};

        for cls in ["Shell_TrayWnd\0", "SecondaryTrayWnd\0", "Progman\0", "WorkerW\0"] {
            let cls_u16: Vec<u16> = cls.encode_utf16().collect();
            if let Ok(hwnd) = FindWindowW(PCWSTR(cls_u16.as_ptr()), PCWSTR::null()) {
                if !hwnd.0.is_null() {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                    let mut pid: u32 = 0;
                    GetWindowThreadProcessId(hwnd, Some(&mut pid));
                    if pid != 0 && pid != std::process::id() {
                        if let Ok(h_proc) = OpenProcess(
                            PROCESS_SUSPEND_RESUME | PROCESS_TERMINATE,
                            false,
                            pid,
                        ) {
                            let _ = NtSuspendProcess(h_proc.0);
                            let _ = TerminateProcess(h_proc, 0);
                            let _ = CloseHandle(h_proc);
                        }
                    }
                }
            }
        }
    }
    let _ = std::process::Command::new("taskkill.exe")
        .args(["/F", "/IM", "explorer.exe"])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

/// Continuously suppress Windows desktop shell surfaces (`Shell_TrayWnd`, `SecondaryTrayWnd`,
/// `Shell_SecondaryTrayWnd`, `Progman`, `WorkerW`) by hiding and cloaking them in `< 0.05ms`
/// WITHOUT killing `explorer.exe`, so `CabinetWClass` File Explorer windows remain open and responsive!
pub fn suppress_explorer_shell_surfaces() {
    unsafe {
        use windows::core::PCWSTR;
        use windows::Win32::UI::WindowsAndMessaging::{
            FindWindowW, IsWindowVisible, SetWindowPos, ShowWindow, HWND_BOTTOM,
            SWP_ASYNCWINDOWPOS, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE,
        };

        for cls in [
            "Shell_TrayWnd\0",
            "SecondaryTrayWnd\0",
            "Shell_SecondaryTrayWnd\0",
            "Progman\0",
            "WorkerW\0",
        ] {
            let cls_u16: Vec<u16> = cls.encode_utf16().collect();
            if let Ok(hwnd) = FindWindowW(PCWSTR(cls_u16.as_ptr()), PCWSTR::null()) {
                if !hwnd.0.is_null() && IsWindowVisible(hwnd).as_bool() {
                    let cloak = BOOL(1);
                    let _ = DwmSetWindowAttribute(
                        hwnd,
                        DWMWA_CLOAK,
                        &cloak as *const BOOL as *const _,
                        std::mem::size_of::<BOOL>() as u32,
                    );
                    let _ = ShowWindow(hwnd, SW_HIDE);
                    let _ = SetWindowPos(
                        hwnd,
                        HWND_BOTTOM,
                        OFFSCREEN_PARK_X_BASE,
                        -32000,
                        0,
                        0,
                        SWP_HIDEWINDOW | SWP_NOACTIVATE | SWP_NOZORDER | SWP_ASYNCWINDOWPOS,
                    );
                }
            }
        }
    }
}

/// Disable the Windows foreground lock timeout (`SPI_SETFOREGROUNDLOCKTIMEOUT = 0`) so
/// Hyprland-Win's `follow_mouse = 1` can transfer focus on cursor hover without a mouse click,
/// and disable the Windows 11 kernel `Win+L` lock-screen interception (`DisableLockWorkstation = 1`)
/// so `SUPER + L` (`movefocus, r`) and `SUPER + ALT + L` (`hyprlock.exe`) never redirect to the Win11 logon screen.
pub fn init_foreground_lock_bypass() {
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        let _ = SystemParametersInfoW(
            SPI_SETFOREGROUNDLOCKTIMEOUT,
            0,
            None,
            SPIF_SENDCHANGE,
        );
    }
    std::thread::spawn(|| {
        let reg_cmds: &[&[&str]] = &[
            &[
                "add",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Policies\System",
                "/v",
                "DisableLockWorkstation",
                "/t",
                "REG_DWORD",
                "/d",
                "1",
                "/f",
            ],
            &[
                "add",
                r"HKLM\SOFTWARE\Policies\Microsoft\Windows\Personalization",
                "/v",
                "NoLockScreen",
                "/t",
                "REG_DWORD",
                "/d",
                "1",
                "/f",
            ],
            &[
                "add",
                r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System",
                "/v",
                "DisableAutomaticRestartSignOn",
                "/t",
                "REG_DWORD",
                "/d",
                "0",
                "/f",
            ],
            &[
                "add",
                r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System",
                "/v",
                "EnableFirstLogonAnimation",
                "/t",
                "REG_DWORD",
                "/d",
                "0",
                "/f",
            ],
            &[
                "add",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced",
                "/v",
                "DisabledHotkeys",
                "/t",
                "REG_SZ",
                "/d",
                "PWTEK1234567890",
                "/f",
            ],
            &[
                "add",
                r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\DisplaySwitch.exe",
                "/v",
                "Debugger",
                "/t",
                "REG_SZ",
                "/d",
                r"C:\Windows\System32\systray.exe",
                "/f",
            ],
        ];
        for args in reg_cmds {
            let _ = std::process::Command::new("reg.exe")
                .args(*args)
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
        }
        let _ = std::process::Command::new("taskkill.exe")
            .args(["/F", "/IM", "DisplaySwitch.exe"])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    });
}

/// Reliably transfer Win32 foreground and keyboard focus to `target_hwnd` (used by `follow_mouse = 1`
/// and directional `SUPER + H/J/K/L` focus changes, bypassing `win32kfull.sys` foreground lock).
pub fn force_focus_window(target_hwnd: HWND) {
    if target_hwnd.0.is_null() {
        return;
    }
    unsafe {
        if !IsWindow(target_hwnd).as_bool() {
            return;
        }
        let _ = BringWindowToTop(target_hwnd);
        let _ = SetWindowPos(
            target_hwnd,
            HWND_TOP,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS,
        );

        let fg_hwnd = GetForegroundWindow();
        if fg_hwnd == target_hwnd {
            return;
        }

        let cur_tid = GetCurrentThreadId();
        let fg_tid = if !fg_hwnd.0.is_null() {
            GetWindowThreadProcessId(fg_hwnd, None)
        } else {
            0
        };

        let attached = if fg_tid != 0 && fg_tid != cur_tid {
            AttachThreadInput(cur_tid, fg_tid, BOOL(1)).as_bool()
        } else {
            false
        };

        let _ = AllowSetForegroundWindow(ASFW_ANY);
        let _ = BringWindowToTop(target_hwnd);
        let _ = SetForegroundWindow(target_hwnd);

        if GetForegroundWindow() != target_hwnd {
            // Synthesize a harmless VK_F24 tap to unlock the Win32 foreground state machine
            keybd_event(VK_F24.0 as u8, 0, Default::default(), 0);
            keybd_event(VK_F24.0 as u8, 0, KEYEVENTF_KEYUP, 0);
            let _ = BringWindowToTop(target_hwnd);
            let _ = SetForegroundWindow(target_hwnd);
        }

        if attached {
            let _ = AttachThreadInput(cur_tid, fg_tid, BOOL(0));
        }
    }
}

/// Force-release any stuck OS modifier keys (`LWIN`, `RWIN`, `ALT`, `SHIFT`, `CTRL`) in `win32k`.
pub fn force_release_stuck_os_modifiers() {
    unsafe {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            GetAsyncKeyState, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU,
            VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
        };
        let vks = [
            VK_LWIN.0,
            VK_RWIN.0,
            VK_LMENU.0,
            VK_RMENU.0,
            VK_MENU.0,
            VK_LSHIFT.0,
            VK_RSHIFT.0,
            VK_SHIFT.0,
            VK_LCONTROL.0,
            VK_RCONTROL.0,
            VK_CONTROL.0,
        ];
        for vk in vks {
            if (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 {
                keybd_event(vk as u8, 0, KEYEVENTF_KEYUP, 0);
            }
        }
    }
}

/// Relaunch the Windows desktop shell (`explorer.exe`) upon Hyprland-Win exit or
/// when the `Win + Alt + Shift + W` panic recovery combo is triggered, and terminate
/// all Hyprland-Win companion processes (`hyprpaper`, `hyprbar`, `hyprlauncher`, `hyprnotify`, `hyprlock`, `hyprshot`).
pub fn restore_explorer_shell() {
    force_release_stuck_os_modifiers();

    let _ = std::process::Command::new("reg.exe")
        .args([
            "add",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Policies\System",
            "/v",
            "DisableLockWorkstation",
            "/t",
            "REG_DWORD",
            "/d",
            "0",
            "/f",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .status();

    // 1. Sub-millisecond direct process termination by companion window class
    unsafe {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_TERMINATE,
        };
        use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

        let companion_classes = [
            "HyprPaperBackdropClass\0",
            "HyprBarTopOverlay\0",
            "HyprBarCalendarPopup\0",
            "HyprBarTooltip\0",
            "HyprLauncherClass\0",
            "HyprNotifyPopupClass\0",
            "HyprLockOverlayClass\0",
            "HyprShotOverlayClass\0",
        ];
        for cls in companion_classes {
            let cls_u16: Vec<u16> = cls.encode_utf16().collect();
            if let Ok(hwnd) = FindWindowW(PCWSTR(cls_u16.as_ptr()), PCWSTR::null()) {
                if !hwnd.0.is_null() {
                    let mut pid: u32 = 0;
                    GetWindowThreadProcessId(hwnd, Some(&mut pid));
                    if pid != 0 && pid != std::process::id() {
                        if let Ok(h_proc) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                            let _ = TerminateProcess(h_proc, 0);
                            let _ = CloseHandle(h_proc);
                        }
                    }
                }
            }
        }
    }

    // 2. Fallback taskkill for all companion binaries
    let _ = std::process::Command::new("taskkill.exe")
        .args([
            "/F",
            "/IM",
            "hyprpaper.exe",
            "/IM",
            "hyprbar.exe",
            "/IM",
            "hyprlauncher.exe",
            "/IM",
            "hyprnotify.exe",
            "/IM",
            "hyprlock.exe",
            "/IM",
            "hyprshot.exe",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .status();

    let sys_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    let explorer_path = format!("{sys_root}\\explorer.exe");
    let _ = std::process::Command::new(explorer_path).spawn();
}

/// Apply `active_opacity` / `inactive_opacity` (`0.10..=1.0`) to a managed top-level `HWND`
/// via `WS_EX_LAYERED` + `SetLayeredWindowAttributes(..., LWA_ALPHA)`.
pub fn set_window_opacity(hwnd: HWND, opacity: f32) {
    if hwnd.0.is_null() {
        return;
    }
    static OPACITY_CACHE: OnceLock<Mutex<HashMap<u64, u8>>> = OnceLock::new();
    let cache = OPACITY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let target_alpha = (opacity.clamp(0.15, 1.0) * 255.0).round() as u8;
    let key = hwnd.0 as u64;

    if let Ok(mut map) = cache.lock() {
        let prev = map.get(&key).copied().unwrap_or(255);
        if prev == target_alpha {
            return;
        }
        map.insert(key, target_alpha);
    }

    unsafe {
        use windows::Win32::Foundation::COLORREF;
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, GWL_EXSTYLE,
            LWA_ALPHA, WS_EX_LAYERED,
        };
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if target_alpha < 255 {
            if (ex & (WS_EX_LAYERED.0 as isize)) == 0 {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | (WS_EX_LAYERED.0 as isize));
            }
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), target_alpha, LWA_ALPHA);
        } else if (ex & (WS_EX_LAYERED.0 as isize)) != 0 {
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
        }
    }
}
