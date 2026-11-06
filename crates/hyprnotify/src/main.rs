#![windows_subsystem = "windows"]
//! `hyprnotify` — macOS-Style Toast Notification Daemon for `Hyprland-Win`.
//!
//! - Listens on `\\.\pipe\hyprnotify-ipc` for `HyprNotifyMessage` payloads (`hyprctl notify`, `hyprshot`).
//! - Fetches live Windows 10/11 system & app notifications via `UserNotificationListener` AND
//!   live tailing of `%LOCALAPPDATA%\Microsoft\Windows\Notifications\wpndatabase.db-wal`.
//! - Renders dynamically-sized macOS-style notification cards with Apple (`SF Pro Text`) typography.

mod ui;

use anyhow::Result;
use hyprland_win_core::{hyprland_win_config_file, HyprNotifyMessage, HYPRNOTIFY_IPC_PIPE};
use std::collections::HashSet;
use std::io::Read;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, GetDC, ReleaseDC, PAINTSTRUCT};
use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics,
    GetWindowRect, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassW, SetTimer,
    SetWindowPos, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, HWND_TOPMOST, IDC_ARROW,
    MSG, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE, WM_DESTROY,
    WM_ERASEBKGND, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_PAINT, WM_TIMER, WM_USER, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use ui::{NotifyRenderModel, ToastCardView, TOAST_GAP, TOAST_MIN_HEIGHT, TOAST_WIDTH};

const WM_NOTIFY_ARRIVED: u32 = WM_USER + 77;
const TIMER_ID_TICK: usize = 1;

static NOTIFY_HWND: AtomicIsize = AtomicIsize::new(0);

struct ActiveToast {
    summary: String,
    body: String,
    app_name: String,
    urgency: u8,
    created_at: Instant,
    timeout_ms: u32,
    dismissed_at: Option<Instant>,
    card_h: i32,
    anim_y: f32,
    offset_x: f32,
    opacity: f32,
    is_media: bool,
}

struct NotifyState {
    screen_w: i32,
    screen_h: i32,
    toasts: Vec<ActiveToast>,
    win_dedup: Vec<(String, String, Instant)>,
    visible: bool,
    last_win_size: (i32, i32, i32, i32),
    hover_pt: Option<(i32, i32)>,
    last_tick: Instant,
}

static STATE: OnceLock<Mutex<NotifyState>> = OnceLock::new();

fn attach_current_thread_to_default_desktop() {
    #[link(name = "user32")]
    extern "system" {
        fn OpenDesktopW(
            lpszdesktop: *const u16,
            dwflags: u32,
            finherit: i32,
            dwdesiredaccess: u32,
        ) -> *mut core::ffi::c_void;
        fn SetThreadDesktop(hdesktop: *mut core::ffi::c_void) -> i32;
    }
    unsafe {
        let desk_name: Vec<u16> = "Default\0".encode_utf16().collect();
        let h_desk = OpenDesktopW(desk_name.as_ptr(), 0, 0, 0x01FF);
        if !h_desk.is_null() {
            let _ = SetThreadDesktop(h_desk);
        }
    }
}

fn is_dnd_silenced() -> bool {
    std::env::temp_dir().join("hyprland_win_dnd.flag").is_file()
}

fn enqueue_toast_ex(
    summary: String,
    body: String,
    app_name: String,
    urgency: u8,
    timeout_ms: u32,
    is_media: bool,
    dedup_window_sec: f32,
) {
    if is_dnd_silenced() && urgency < 2 {
        return;
    }
    let clean_sum = summary
        .trim_start_matches(|c: char| ('\u{E000}'..='\u{F8FF}').contains(&c) || c.is_whitespace())
        .trim()
        .to_string();
    let clean_body = body.trim().to_string();
    if clean_sum.is_empty() && clean_body.is_empty() {
        return;
    }

    let measured_h = unsafe { ui::measure_toast_card_height(&clean_sum, &clean_body) };

    if let Some(st_mutex) = STATE.get() {
        if let Ok(mut st) = st_mutex.lock() {
            let now = Instant::now();
            let timeout_clamped = timeout_ms.max(2500);

            st.win_dedup
                .retain(|(_, _, ts)| now.duration_since(*ts).as_secs_f32() < 60.0);

            let s_low = clean_sum.to_lowercase();
            let b_low = clean_body.to_lowercase();

            // Coalesce if an identical non-dismissed notification is already visible on screen
            if let Some(existing_dup) = st.toasts.iter_mut().find(|t| {
                t.dismissed_at.is_none()
                    && t.summary.to_lowercase() == s_low
                    && t.body.to_lowercase() == b_low
            }) {
                existing_dup.created_at = now;
                existing_dup.timeout_ms = timeout_clamped;
                return;
            }

            if dedup_window_sec > 0.0 {
                if st.win_dedup.iter().any(|(s, b, ts)| {
                    now.duration_since(*ts).as_secs_f32() < dedup_window_sec
                        && s == &s_low
                        && b == &b_low
                }) {
                    return;
                }
                st.win_dedup.push((s_low, b_low, now));
            }

            // Only replace in-place for "Now Playing" media notifications so one playing media
            // never stacks multiple media notifications, while distinct Windows notifications stack!
            if is_media {
                if let Some(existing) = st
                    .toasts
                    .iter_mut()
                    .find(|t| t.dismissed_at.is_none() && t.is_media)
                {
                    existing.summary = clean_sum;
                    existing.body = clean_body;
                    existing.app_name = app_name;
                    existing.urgency = urgency;
                    existing.card_h = measured_h;
                    existing.created_at = now;
                    existing.timeout_ms = timeout_clamped;
                    return;
                }
            }

            // Calculate starting Y at the bottom of the current stack so cards never overlap at y=0
            let mut next_y = 0.0_f32;
            for t in &st.toasts {
                next_y += (t.card_h + TOAST_GAP) as f32;
            }

            st.toasts.push(ActiveToast {
                summary: clean_sum,
                body: clean_body,
                app_name,
                urgency,
                created_at: now,
                timeout_ms: timeout_clamped,
                dismissed_at: None,
                card_h: measured_h,
                anim_y: next_y,
                offset_x: (TOAST_WIDTH as f32) + 20.0,
                opacity: 0.0,
                is_media,
            });

            // Cap visible stack at 5 notification cards
            let mut active_non_dismissed = 0usize;
            for t in st.toasts.iter_mut().rev() {
                if t.dismissed_at.is_none() {
                    active_non_dismissed += 1;
                    if active_non_dismissed > 5 {
                        t.dismissed_at = Some(now);
                    }
                }
            }
            if st.toasts.len() > 8 {
                st.toasts.remove(0);
            }
        }
    }
    let h_val = NOTIFY_HWND.load(Ordering::SeqCst);
    if h_val != 0 {
        let hwnd = HWND(h_val as *mut core::ffi::c_void);
        unsafe {
            let _ = PostMessageW(hwnd, WM_NOTIFY_ARRIVED, WPARAM(0), LPARAM(0));
        }
    }
}

fn update_active_media_toast_body_if_present(new_body: &str) {
    let clean_body = new_body.trim();
    if let Some(st_mutex) = STATE.get() {
        if let Ok(mut st) = st_mutex.lock() {
            if let Some(existing) = st
                .toasts
                .iter_mut()
                .find(|t| t.dismissed_at.is_none() && t.is_media)
            {
                let sum_clone = existing.summary.clone();
                existing.body = clean_body.to_string();
                existing.card_h = unsafe { ui::measure_toast_card_height(&sum_clone, clean_body) };
            }
        }
    }
}

fn main() -> Result<()> {
    attach_current_thread_to_default_desktop();

    unsafe {
        let mutex_name: Vec<u16> = "Local\\HyprNotifySingleInstanceMutex\0"
            .encode_utf16()
            .collect();
        let _mutex = CreateMutexW(None, true, PCWSTR(mutex_name.as_ptr()));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Ok(());
        }

        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN).max(1920) };
    let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN).max(1080) };

    let _ = STATE.set(Mutex::new(NotifyState {
        screen_w,
        screen_h,
        toasts: Vec::new(),
        win_dedup: Vec::new(),
        visible: false,
        last_win_size: (0, 0, 0, 0),
        hover_pt: None,
        last_tick: Instant::now(),
    }));

    // 1. Spawn Named Pipe IPC server (`\\.\pipe\hyprnotify-ipc`)
    std::thread::Builder::new()
        .name("hyprnotify-ipc".to_string())
        .spawn(run_notify_pipe_server)?;

    // 2. Spawn Windows GSMTC Media & Battery bridge
    std::thread::Builder::new()
        .name("hyprnotify-win-bridge".to_string())
        .spawn(run_windows_event_toast_bridge)?;

    // 3. Spawn Windows System Notification fetcher (`UserNotificationListener` + `wpndatabase.db-wal`)
    std::thread::Builder::new()
        .name("hyprnotify-winrt-listener".to_string())
        .spawn(run_windows_notification_fetcher)?;

    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let class_name: Vec<u16> = "HyprNotifyPopupClass\0".encode_utf16().collect();
        let window_title: Vec<u16> = "HyprNotify\0".encode_utf16().collect();

        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(notify_wnd_proc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let win_x = (screen_w - TOAST_WIDTH - 16).max(12);
        let win_y = 46;

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(window_title.as_ptr()),
            WS_POPUP,
            win_x,
            win_y,
            TOAST_WIDTH,
            TOAST_MIN_HEIGHT,
            HWND::default(),
            None,
            hinstance,
            None,
        )?;

        let round_pref = DWMWCP_DONOTROUND.0 as u32;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &round_pref as *const u32 as *const _,
            4,
        );
        let border_none: u32 = 0xFFFF_FFFE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border_none as *const u32 as *const _,
            4,
        );

        NOTIFY_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
        let _ = SetTimer(hwnd, TIMER_ID_TICK, 14, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

fn run_notify_pipe_server() {
    use windows::Win32::Security::{
        InitializeSecurityDescriptor, SetSecurityDescriptorDacl, PSECURITY_DESCRIPTOR,
        SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
    };

    let pipe_name: Vec<u16> = format!("{HYPRNOTIFY_IPC_PIPE}\0").encode_utf16().collect();
    loop {
        unsafe {
            let mut sd = SECURITY_DESCRIPTOR::default();
            let psd = PSECURITY_DESCRIPTOR(&mut sd as *mut _ as *mut core::ffi::c_void);
            let _ = InitializeSecurityDescriptor(psd, 1);
            let _ = SetSecurityDescriptorDacl(psd, true, None, false);
            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: psd.0,
                bInheritHandle: false.into(),
            };

            let handle = CreateNamedPipeW(
                PCWSTR(pipe_name.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                8,
                4096,
                4096,
                0,
                Some(&sa),
            );
            if handle.is_invalid() {
                std::thread::sleep(std::time::Duration::from_millis(200));
                continue;
            }

            let connected = ConnectNamedPipe(handle, None).is_ok();
            if connected {
                let mut f = <std::fs::File as std::os::windows::io::FromRawHandle>::from_raw_handle(
                    handle.0 as *mut _,
                );
                let mut buf = [0u8; 4096];
                if let Ok(n) = f.read(&mut buf) {
                    if n > 0 {
                        if let Ok(msg) = serde_json::from_slice::<HyprNotifyMessage>(&buf[..n]) {
                            enqueue_toast_ex(
                                msg.summary,
                                msg.body,
                                String::new(),
                                msg.urgency,
                                msg.timeout_ms,
                                false,
                                0.0,
                            );
                        }
                    }
                }
                let _ = DisconnectNamedPipe(handle);
                drop(f);
            } else {
                let _ = windows::Win32::Foundation::CloseHandle(handle);
            }
        }
    }
}

fn current_utc_filetime_100ns() -> i64 {
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        dw_low_date_time: u32,
        dw_high_date_time: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetSystemTimeAsFileTime(lp_system_time_as_file_time: *mut FileTime);
    }
    unsafe {
        let mut ft = FileTime::default();
        GetSystemTimeAsFileTime(&mut ft);
        ((ft.dw_high_date_time as i64) << 32) | (ft.dw_low_date_time as i64)
    }
}

fn load_persisted_seen_ids() -> (u32, HashSet<u32>) {
    let path = hyprland_win_config_file("notify_seen.txt");
    let mut max_id = 0u32;
    let mut set = HashSet::new();
    if let Ok(text) = std::fs::read_to_string(&path) {
        for tok in text.split_whitespace() {
            if let Ok(id) = tok.parse::<u32>() {
                set.insert(id);
                if id > max_id {
                    max_id = id;
                }
            }
        }
    }
    (max_id, set)
}

fn save_persisted_seen_ids(max_id: u32, seen: &HashSet<u32>) {
    let path = hyprland_win_config_file("notify_seen.txt");
    let mut ids: Vec<u32> = seen.iter().copied().collect();
    ids.sort_unstable();
    if ids.len() > 256 {
        let drain_count = ids.len() - 256;
        ids.drain(0..drain_count);
    }
    if max_id > 0 && !ids.contains(&max_id) {
        ids.push(max_id);
    }
    let body = ids
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::write(path, body);
}

fn run_windows_notification_fetcher() {
    use windows::UI::Notifications::Management::{
        UserNotificationListener, UserNotificationListenerAccessStatus,
    };
    use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    let (mut max_seen_id, mut seen_ids) = load_persisted_seen_ids();
    let mut warmup_polls_remaining: u8 = 2;
    let startup_ft = current_utc_filetime_100ns();

    let listener_opt = UserNotificationListener::Current().ok();
    if let Some(ref listener) = listener_opt {
        if let Ok(op) = listener.RequestAccessAsync() {
            let _ = op.get();
        }
    }

    loop {
        std::thread::sleep(std::time::Duration::from_millis(350));

        // Poll WinRT `UserNotificationListener` for live Windows Toast Notifications
        if let Some(ref listener) = listener_opt {
            if listener.GetAccessStatus().ok()
                == Some(UserNotificationListenerAccessStatus::Allowed)
            {
                if let Ok(op) = listener.GetNotificationsAsync(NotificationKinds::Toast) {
                    if let Ok(notifs) = op.get() {
                        let count = notifs.Size().unwrap_or(0);
                        let now_ft = current_utc_filetime_100ns();

                        if warmup_polls_remaining > 0 {
                            let mut changed = false;
                            for i in 0..count {
                                if let Ok(n) = notifs.GetAt(i) {
                                    if let Ok(id) = n.Id() {
                                        if seen_ids.insert(id) {
                                            changed = true;
                                        }
                                        if id > max_seen_id {
                                            max_seen_id = id;
                                            changed = true;
                                        }
                                    }
                                }
                            }
                            if changed {
                                save_persisted_seen_ids(max_seen_id, &seen_ids);
                            }
                            if count > 0 || warmup_polls_remaining == 1 {
                                warmup_polls_remaining = warmup_polls_remaining.saturating_sub(1);
                            }
                            continue;
                        }

                        let mut state_dirty = false;
                        for i in 0..count {
                            let Ok(n) = notifs.GetAt(i) else { continue };
                            let Ok(id) = n.Id() else { continue };

                            // Strictly ignore any notification ID we have already seen or any ID <= max_seen_id
                            if id <= max_seen_id || !seen_ids.insert(id) {
                                continue;
                            }
                            max_seen_id = max_seen_id.max(id);
                            state_dirty = true;

                            // Verify notification CreationTime is fresh (created within the last 15s and not prior to startup)
                            if let Ok(ct) = n.CreationTime() {
                                let age_100ns = now_ft.saturating_sub(ct.UniversalTime);
                                // 15 seconds = 150_000_000 * 100ns
                                if ct.UniversalTime < startup_ft.saturating_sub(30_000_000)
                                    || age_100ns > 150_000_000
                                {
                                    continue;
                                }
                            }

                            let app_name = n
                                .AppInfo()
                                .and_then(|ai| ai.DisplayInfo())
                                .and_then(|di| di.DisplayName())
                                .map(|s| s.to_string_lossy())
                                .unwrap_or_default();

                            let mut texts: Vec<String> = Vec::new();
                            if let Ok(notif_obj) = n.Notification() {
                                if let Ok(visual) = notif_obj.Visual() {
                                    if let Ok(binding_name) =
                                        KnownNotificationBindings::ToastGeneric()
                                    {
                                        if let Ok(binding) = visual.GetBinding(&binding_name) {
                                            if let Ok(elems) = binding.GetTextElements() {
                                                let el_count = elems.Size().unwrap_or(0);
                                                for j in 0..el_count {
                                                    if let Ok(el) = elems.GetAt(j) {
                                                        if let Ok(t) = el.Text() {
                                                            let s = t
                                                                .to_string_lossy()
                                                                .trim()
                                                                .to_string();
                                                            if !s.is_empty() {
                                                                texts.push(s);
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            let (summary, body) = if texts.len() >= 2 {
                                (texts[0].clone(), texts[1..].join("\n"))
                            } else if texts.len() == 1 {
                                let sum = if !app_name.trim().is_empty() {
                                    app_name.trim().to_string()
                                } else {
                                    "System Notification".to_string()
                                };
                                (sum, texts[0].clone())
                            } else {
                                continue;
                            };

                            enqueue_toast_ex(
                                summary,
                                body,
                                app_name,
                                1,
                                5500,
                                false,
                                45.0,
                            );
                        }

                        if state_dirty {
                            save_persisted_seen_ids(max_seen_id, &seen_ids);
                        }
                    }
                }
            }
        }
    }
}

fn normalize_media_title(raw: &str) -> String {
    let mut s = raw.trim();
    if s.starts_with('(') {
        if let Some(close_idx) = s.find(')') {
            let inside = &s[1..close_idx];
            if !inside.is_empty() && inside.chars().all(|c| c.is_ascii_digit()) {
                s = s[close_idx + 1..].trim_start();
            }
        }
    }
    for suffix in [
        " - YouTube Music",
        " - YouTube",
        " — YouTube",
        " - Mozilla Firefox",
        " - Google Chrome",
        " - Microsoft Edge",
        " - Brave",
    ] {
        if let Some(stripped) = s.strip_suffix(suffix) {
            s = stripped.trim();
        }
    }
    s.to_lowercase()
}

fn run_windows_event_toast_bridge() {
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    let mut last_notified_title = String::new();
    let mut candidate_title = String::new();
    let mut candidate_polls: u8 = 0;
    let mut warned_low_bat = false;

    loop {
        std::thread::sleep(std::time::Duration::from_millis(1200));

        // 1. Bridge Windows GSMTC Media Track Changes -> Single stabilized "Now Playing" Toast
        if let Ok(mgr_op) =
            windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
        {
            if let Ok(mgr) = mgr_op.get() {
                if let Ok(session) = mgr.GetCurrentSession() {
                    let is_playing = session
                        .GetPlaybackInfo()
                        .and_then(|i| i.PlaybackStatus())
                        .map(|s| {
                            s == windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing
                        })
                        .unwrap_or(false);

                    if is_playing {
                        let source_app = session
                            .SourceAppUserModelId()
                            .map(|s| s.to_string_lossy())
                            .unwrap_or_default();
                        if let Ok(props_op) = session.TryGetMediaPropertiesAsync() {
                            if let Ok(props) = props_op.get() {
                                let title = props
                                    .Title()
                                    .map(|s| s.to_string_lossy())
                                    .unwrap_or_default();
                                let artist = props
                                    .Artist()
                                    .map(|s| s.to_string_lossy())
                                    .unwrap_or_default();
                                let clean_title = title.trim();
                                let clean_artist = artist.trim();
                                let norm_title = normalize_media_title(clean_title);

                                if !norm_title.is_empty() {
                                    let body = if clean_artist.is_empty() {
                                        clean_title.to_string()
                                    } else {
                                        format!("{clean_artist} — {clean_title}")
                                    };

                                    if last_notified_title.is_empty() {
                                        last_notified_title = norm_title;
                                    } else if norm_title == last_notified_title {
                                        candidate_polls = 0;
                                        update_active_media_toast_body_if_present(&body);
                                    } else {
                                        if candidate_title == norm_title {
                                            candidate_polls = candidate_polls.saturating_add(1);
                                        } else {
                                            candidate_title = norm_title.clone();
                                            candidate_polls = 1;
                                        }

                                        if candidate_polls >= 2 {
                                            last_notified_title = norm_title;
                                            candidate_polls = 0;
                                            enqueue_toast_ex(
                                                "Now Playing".to_string(),
                                                body,
                                                source_app,
                                                0,
                                                4000,
                                                true,
                                                0.0,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // 2. Bridge Windows Power / Low Battery Warnings (`<= 20%` and discharging)
        unsafe {
            let mut sps = SYSTEM_POWER_STATUS::default();
            if GetSystemPowerStatus(&mut sps).is_ok() {
                let has_bat = sps.BatteryFlag != 128 && sps.BatteryFlag != 255;
                let pct = sps.BatteryLifePercent;
                let charging = sps.ACLineStatus == 1 || (sps.BatteryFlag & 8) != 0;
                if has_bat && !charging && pct <= 20 {
                    if !warned_low_bat {
                        warned_low_bat = true;
                        enqueue_toast_ex(
                            "Low Battery".to_string(),
                            format!("Battery is at {pct}%. Connect charger soon."),
                            String::new(),
                            2,
                            6000,
                            false,
                            5.0,
                        );
                    }
                } else if charging || pct > 25 {
                    warned_low_bat = false;
                }
            }
        }
    }
}

unsafe fn sync_notify_window_state(hwnd: HWND) {
    let Some(st_mutex) = STATE.get() else {
        return;
    };

    // Check cursor hover position for the macOS top-left close button (`×`)
    let mut pt = POINT::default();
    let mut win_rc = RECT::default();
    let has_cursor = GetCursorPos(&mut pt).is_ok() && GetWindowRect(hwnd, &mut win_rc).is_ok();

    let (count, was_visible, size_changed, win_rect, cards) = {
        let mut st = match st_mutex.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let now = Instant::now();
        let dt = now
            .duration_since(st.last_tick)
            .as_secs_f32()
            .clamp(0.001, 0.050);
        st.last_tick = now;

        if has_cursor
            && pt.x >= win_rc.left
            && pt.x <= win_rc.right
            && pt.y >= win_rc.top
            && pt.y <= win_rc.bottom
        {
            st.hover_pt = Some((pt.x - win_rc.left, pt.y - win_rc.top));
        } else {
            st.hover_pt = None;
        }

        const ENTER_MS: f32 = 240.0;
        const EXIT_MS: f32 = 200.0;
        let y_lerp = (1.0 - (-18.0 * dt).exp()).clamp(0.0, 1.0);

        // Remove toasts that have completed their exit animation
        st.toasts.retain(|t| {
            let elapsed_ms = now.duration_since(t.created_at).as_secs_f32() * 1000.0;
            let dismissed_done = t
                .dismissed_at
                .map(|d| now.duration_since(d).as_secs_f32() * 1000.0 >= EXIT_MS)
                .unwrap_or(false);
            !dismissed_done && elapsed_ms < (t.timeout_ms as f32)
        });

        let mut built_cards = Vec::with_capacity(st.toasts.len());
        let mut cur_target_y = 0.0_f32;
        let mut max_bottom_px: i32 = TOAST_MIN_HEIGHT;
        let hover_pt = st.hover_pt;

        for t in st.toasts.iter_mut() {
            let target_y = cur_target_y;
            cur_target_y += (t.card_h + TOAST_GAP) as f32;

            t.anim_y += (target_y - t.anim_y) * y_lerp;
            if (target_y - t.anim_y).abs() < 0.15 {
                t.anim_y = target_y;
            }

            let elapsed_ms = now.duration_since(t.created_at).as_secs_f32() * 1000.0;
            let total_ms = (t.timeout_ms.max(1)) as f32;

            let enter_t = (elapsed_ms / ENTER_MS).clamp(0.0, 1.0);
            let ease_enter = 1.0 - (1.0 - enter_t).powi(3);
            let mut off_x = (1.0 - ease_enter) * ((TOAST_WIDTH as f32) + 16.0);
            let mut alpha = (enter_t * 1.4).clamp(0.0, 1.0);

            let exit_t = if let Some(d_at) = t.dismissed_at {
                (now.duration_since(d_at).as_secs_f32() * 1000.0 / EXIT_MS).clamp(0.0, 1.0)
            } else if elapsed_ms > (total_ms - EXIT_MS).max(0.0) {
                ((elapsed_ms - (total_ms - EXIT_MS).max(0.0)) / EXIT_MS).clamp(0.0, 1.0)
            } else {
                0.0
            };

            if exit_t > 0.0 {
                let ease_exit = exit_t * exit_t * (3.0 - 2.0 * exit_t);
                off_x = off_x.max(ease_exit * ((TOAST_WIDTH as f32) + 20.0));
                alpha *= 1.0 - ease_exit;
            }

            t.offset_x = off_x;
            t.opacity = alpha.clamp(0.0, 1.0);

            let card_bottom = (t.anim_y.ceil() as i32) + t.card_h + 4;
            if card_bottom > max_bottom_px {
                max_bottom_px = card_bottom;
            }

            let is_hovered = if let Some((hx, hy)) = hover_pt {
                let cy = t.anim_y.round() as i32;
                hx >= 0 && hx <= TOAST_WIDTH && hy >= cy && hy <= cy + t.card_h
            } else {
                false
            };

            let progress = (1.0 - (elapsed_ms / total_ms)).clamp(0.0, 1.0);
            built_cards.push(ToastCardView {
                summary: t.summary.clone(),
                body: t.body.clone(),
                app_name: t.app_name.clone(),
                urgency: t.urgency,
                progress,
                card_h: t.card_h,
                offset_x: t.offset_x,
                offset_y: t.anim_y,
                opacity: t.opacity,
                hovered: is_hovered,
            });
        }

        let c = st.toasts.len() as i32;
        let vis = st.visible;
        st.visible = c > 0;

        let win_x = (st.screen_w - TOAST_WIDTH - 16).max(12);
        let win_y = 46;
        let nominal_h = (cur_target_y.ceil() as i32).max(max_bottom_px).max(TOAST_MIN_HEIGHT);
        let new_rect = (win_x, win_y, TOAST_WIDTH, nominal_h);
        let changed = st.last_win_size != new_rect || !vis;
        if c > 0 {
            st.last_win_size = new_rect;
        }

        (c, vis, changed, new_rect, built_cards)
    };

    if count == 0 {
        if was_visible {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        return;
    }

    let (win_x, win_y, win_w, total_h) = win_rect;
    if size_changed {
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            win_x,
            win_y,
            win_w,
            total_h,
            SWP_NOACTIVATE,
        );
    }
    if !was_visible {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }

    let screen_dc = GetDC(HWND::default());
    if !screen_dc.is_invalid() {
        let model = NotifyRenderModel {
            width: win_w,
            height: total_h,
            cards,
        };
        ui::render_notifications(hwnd, screen_dc, &model);
        let _ = ReleaseDC(HWND::default(), screen_dc);
    }
}

unsafe extern "system" fn notify_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_NOTIFY_ARRIVED => {
            sync_notify_window_state(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            sync_notify_window_state(hwnd);
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_ID_TICK {
                let has_toasts = STATE
                    .get()
                    .and_then(|m| m.lock().ok().map(|s| !s.toasts.is_empty() || s.visible))
                    .unwrap_or(false);
                if has_toasts {
                    sync_notify_window_state(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let y_f = y as f32;
            if let Some(st_mutex) = STATE.get() {
                if let Ok(mut st) = st_mutex.lock() {
                    let now = Instant::now();
                    if let Some(t) = st.toasts.iter_mut().find(|t| {
                        t.dismissed_at.is_none()
                            && y_f >= t.anim_y
                            && y_f <= t.anim_y + (t.card_h as f32)
                    }) {
                        t.dismissed_at = Some(now);
                    }
                }
            }
            sync_notify_window_state(hwnd);
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
