//! Live System & Media Widgets for `hyprbar` V2 (`atif-1402/minimal-waybar-themes` V2).
//!
//! Provides non-blocking background telemetry & interactive controllers for:
//! - `mpris`: Windows GSMTC (`GlobalSystemMediaTransportControlsSessionManager`) + Win32 media window fallback +
//!   Play/Pause, Next, Previous, and Scroll-Wheel Volume.
//! - `pulseaudio`: Windows CoreAudio (`IMMDeviceEnumerator` + `IAudioEndpointVolume`) master volume %, mute state,
//!   right-click mute toggle, scroll-wheel +/-5% volume control, and left-click mixer launch.
//! - `network`: Live Windows IP Helper (`GetIfTable`) Ethernet / Wi-Fi / Disconnected state.
//! - `bluetooth`: Live Bluetooth state & settings launcher.
//! - `battery`: `GetSystemPowerStatus` battery detection, capacity %, and charging state.
//! - `custom/idle-indicator`: Windows `SetThreadExecutionState` sleep/display inhibit toggle.
//! - `custom/notification-silencing-indicator`: Do-Not-Disturb toggle.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator,
};
use windows::Win32::NetworkManagement::IpHelper::{GetIfTable, MIB_IFTABLE};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Power::{
    GetSystemPowerStatus, SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED,
    ES_SYSTEM_REQUIRED, SYSTEM_POWER_STATUS,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, KEYEVENTF_KEYUP, VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaPlaybackStatus {
    Stopped,
    Playing,
    Paused,
}

#[derive(Debug, Clone)]
pub struct MediaSessionSnapshot {
    pub status: MediaPlaybackStatus,
    pub player_name: String,
    pub player_icon: &'static str,
    pub title: String,
    pub artist: String,
    pub position_sec: u32,
    pub duration_sec: u32,
}

impl Default for MediaSessionSnapshot {
    fn default() -> Self {
        Self {
            status: MediaPlaybackStatus::Stopped,
            player_name: String::new(),
            player_icon: "\u{F001}", // 
            title: String::new(),
            artist: String::new(),
            position_sec: 0,
            duration_sec: 0,
        }
    }
}

impl MediaSessionSnapshot {
    /// Formats the media string according to `V2/config.jsonc` (`{player_icon} {artist} - {title}` or `{status_icon} {dynamic}`, max-length 40).
    pub fn format_display(&self) -> String {
        if self.status == MediaPlaybackStatus::Stopped || self.title.is_empty() {
            return "\u{F001}  No media".to_string();
        }

        let icon = match self.status {
            MediaPlaybackStatus::Paused => "\u{F04C}", // 
            MediaPlaybackStatus::Playing => self.player_icon,
            MediaPlaybackStatus::Stopped => "\u{F001}",
        };

        let track = if !self.artist.is_empty() && !self.title.contains(&self.artist) {
            format!("{} - {}", self.artist.trim(), self.title.trim())
        } else {
            self.title.trim().to_string()
        };

        let truncated = truncate_chars(&track, 36);
        format!("{icon}  {truncated}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkKind {
    Disconnected,
    Wifi,
    Ethernet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayAppItem {
    pub hwnd: u64,
    pub pid: u32,
    pub name: String,
    pub exe_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDeviceEntry {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub is_input: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BluetoothDeviceEntry {
    pub address: u64,
    pub name: String,
    pub connected: bool,
    pub remembered: bool,
    pub category: &'static str, // "audio", "input", "phone", "device"
}

#[derive(Debug, Clone)]
pub struct SystemWidgetsSnapshot {
    pub media: MediaSessionSnapshot,
    pub volume_pct: u32,
    pub volume_muted: bool,
    pub mic_volume_pct: u32,
    pub mic_muted: bool,
    pub audio_outputs: Vec<AudioDeviceEntry>,
    pub audio_inputs: Vec<AudioDeviceEntry>,
    pub network_kind: NetworkKind,
    pub network_adapter: String,
    pub network_speed_mbps: u32,
    pub bluetooth_enabled: bool,
    pub bluetooth_devices: Vec<BluetoothDeviceEntry>,
    pub has_battery: bool,
    pub battery_pct: u32,
    pub battery_charging: bool,
    pub idle_inhibited: bool,
    pub dnd_silenced: bool,
    pub tray_items: Vec<TrayAppItem>,
    pub os_version_label: String,
    pub weather_temp_c: i32,
    pub weather_desc: String,
    pub weather_location: String,
    pub weather_icon: &'static str,
}

impl Default for SystemWidgetsSnapshot {
    fn default() -> Self {
        Self {
            media: MediaSessionSnapshot::default(),
            volume_pct: 65,
            volume_muted: false,
            mic_volume_pct: 80,
            mic_muted: false,
            audio_outputs: Vec::new(),
            audio_inputs: Vec::new(),
            network_kind: NetworkKind::Ethernet,
            network_adapter: "Ethernet Adapter".to_string(),
            network_speed_mbps: 1000,
            bluetooth_enabled: true,
            bluetooth_devices: Vec::new(),
            has_battery: false,
            battery_pct: 100,
            battery_charging: false,
            idle_inhibited: false,
            dnd_silenced: false,
            tray_items: Vec::new(),
            os_version_label: query_windows_11_build_label(),
            weather_temp_c: 18,
            weather_desc: "Clear · Tuzla".to_string(),
            weather_location: "Istanbul, Tuzla".to_string(),
            weather_icon: "\u{F185}",
        }
    }
}

static WIDGETS_STATE: OnceLock<Arc<Mutex<SystemWidgetsSnapshot>>> = OnceLock::new();
static WORKER_STARTED: OnceLock<()> = OnceLock::new();

pub fn get_widgets_snapshot() -> SystemWidgetsSnapshot {
    ensure_widgets_worker_started();
    WIDGETS_STATE
        .get_or_init(|| Arc::new(Mutex::new(SystemWidgetsSnapshot::default())))
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

pub fn ensure_widgets_worker_started() {
    let state = WIDGETS_STATE
        .get_or_init(|| Arc::new(Mutex::new(SystemWidgetsSnapshot::default())))
        .clone();

    WORKER_STARTED.get_or_init(|| {
        std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let os_label = query_windows_11_build_label();
            let mut last_weather_loc = String::new();
            let mut tick: u32 = 0;
            loop {
                let media = poll_media_session();
                let (vol_pct, vol_muted) = poll_master_volume().unwrap_or((65, false));
                let (net_kind, net_adapter, net_speed) = poll_network_details();
                let (has_bat, bat_pct, bat_chg) = poll_battery_status();
                let tray_items = if tick % 6 == 0 {
                    Some(poll_system_tray_items())
                } else {
                    None
                };
                let audio_and_bt = if tick % 4 == 0 {
                    let (outs, ins, mic_pct, mic_mute) = poll_audio_devices_and_mic();
                    let (bt_on, bt_devs) = poll_bluetooth_devices();
                    Some((outs, ins, mic_pct, mic_mute, bt_on, bt_devs))
                } else {
                    None
                };
                let cfg_loc = read_configured_weather_location();
                let weather_upd = if tick == 0 || tick % 1200 == 0 || cfg_loc != last_weather_loc {
                    last_weather_loc = cfg_loc.clone();
                    poll_weather_for_location(&cfg_loc)
                } else {
                    None
                };
                tick = tick.wrapping_add(1);

                if let Ok(mut guard) = state.lock() {
                    guard.os_version_label = os_label.clone();
                    guard.media = media;
                    guard.volume_pct = vol_pct;
                    guard.volume_muted = vol_muted;
                    guard.network_kind = net_kind;
                    guard.network_adapter = net_adapter;
                    guard.network_speed_mbps = net_speed;
                    guard.has_battery = has_bat;
                    guard.battery_pct = bat_pct;
                    guard.battery_charging = bat_chg;
                    if let Some((temp_c, desc, loc, icon)) = weather_upd {
                        guard.weather_temp_c = temp_c;
                        guard.weather_desc = desc;
                        guard.weather_location = loc;
                        guard.weather_icon = icon;
                    }
                    if let Some(items) = tray_items {
                        guard.tray_items = items;
                    }
                    if let Some((outs, ins, mic_pct, mic_mute, bt_on, bt_devs)) = audio_and_bt {
                        guard.audio_outputs = outs;
                        guard.audio_inputs = ins;
                        guard.mic_volume_pct = mic_pct;
                        guard.mic_muted = mic_mute;
                        if let Some(hw_bt_on) = bt_on {
                            guard.bluetooth_enabled = hw_bt_on;
                        }
                        guard.bluetooth_devices = bt_devs;
                    }
                }

                std::thread::sleep(Duration::from_millis(250));
            }
        });
    });
}

// ----------------------------------------------------------------------------
// Interactive Widget Actions
// ----------------------------------------------------------------------------

pub fn media_play_pause() {
    std::thread::spawn(|| {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let mut handled_by_gsmtc = false;
        if let Ok(mgr_op) = windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager::RequestAsync() {
            if let Ok(mgr) = mgr_op.get() {
                if let Ok(session) = mgr.GetCurrentSession() {
                    if let Ok(op) = session.TryTogglePlayPauseAsync() {
                        handled_by_gsmtc = op.get().unwrap_or(false);
                    }
                }
            }
        }
        if !handled_by_gsmtc {
            unsafe {
                keybd_event(VK_MEDIA_PLAY_PAUSE.0 as u8, 0, Default::default(), 0);
                keybd_event(VK_MEDIA_PLAY_PAUSE.0 as u8, 0, KEYEVENTF_KEYUP, 0);
            }
        }
    });
}

pub fn media_next_track() {
    std::thread::spawn(|| {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let mut handled_by_gsmtc = false;
        if let Ok(mgr_op) = windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager::RequestAsync() {
            if let Ok(mgr) = mgr_op.get() {
                if let Ok(session) = mgr.GetCurrentSession() {
                    if let Ok(op) = session.TrySkipNextAsync() {
                        handled_by_gsmtc = op.get().unwrap_or(false);
                    }
                }
            }
        }
        if !handled_by_gsmtc {
            unsafe {
                keybd_event(VK_MEDIA_NEXT_TRACK.0 as u8, 0, Default::default(), 0);
                keybd_event(VK_MEDIA_NEXT_TRACK.0 as u8, 0, KEYEVENTF_KEYUP, 0);
            }
        }
    });
}

pub fn media_prev_track() {
    std::thread::spawn(|| {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let mut handled_by_gsmtc = false;
        if let Ok(mgr_op) = windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager::RequestAsync() {
            if let Ok(mgr) = mgr_op.get() {
                if let Ok(session) = mgr.GetCurrentSession() {
                    if let Ok(op) = session.TrySkipPreviousAsync() {
                        handled_by_gsmtc = op.get().unwrap_or(false);
                    }
                }
            }
        }
        if !handled_by_gsmtc {
            unsafe {
                keybd_event(VK_MEDIA_PREV_TRACK.0 as u8, 0, Default::default(), 0);
                keybd_event(VK_MEDIA_PREV_TRACK.0 as u8, 0, KEYEVENTF_KEYUP, 0);
            }
        }
    });
}

pub fn adjust_master_volume(delta_pct: i32) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Some(ep) = get_audio_endpoint_volume() {
            if let Ok(cur) = ep.GetMasterVolumeLevelScalar() {
                let next = (cur + (delta_pct as f32 / 100.0)).clamp(0.0, 1.0);
                let _ = ep.SetMasterVolumeLevelScalar(next, std::ptr::null());
                if let Some(st) = WIDGETS_STATE.get() {
                    if let Ok(mut guard) = st.lock() {
                        guard.volume_pct = (next * 100.0).round() as u32;
                    }
                }
            }
        }
    }
}

pub fn set_master_volume_pct(pct: u32) {
    let clamped = pct.min(100);
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Some(ep) = get_audio_endpoint_volume() {
            let scalar = (clamped as f32 / 100.0).clamp(0.0, 1.0);
            let _ = ep.SetMasterVolumeLevelScalar(scalar, std::ptr::null());
            if clamped > 0 {
                let _ = ep.SetMute(false, std::ptr::null());
            }
            if let Some(st) = WIDGETS_STATE.get() {
                if let Ok(mut guard) = st.lock() {
                    guard.volume_pct = clamped;
                    if clamped > 0 {
                        guard.volume_muted = false;
                    }
                }
            }
        }
    }
}

pub fn toggle_master_mute() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Some(ep) = get_audio_endpoint_volume() {
            if let Ok(muted) = ep.GetMute() {
                let next_mute = !muted.as_bool();
                let _ = ep.SetMute(next_mute, std::ptr::null());
                if let Some(st) = WIDGETS_STATE.get() {
                    if let Ok(mut guard) = st.lock() {
                        guard.volume_muted = next_mute;
                    }
                }
            }
        }
    }
}

pub fn set_mic_volume_pct(pct: u32) {
    let clamped = pct.min(100);
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Some(ep) = get_capture_endpoint_volume() {
            let scalar = (clamped as f32 / 100.0).clamp(0.0, 1.0);
            let _ = ep.SetMasterVolumeLevelScalar(scalar, std::ptr::null());
            if clamped > 0 {
                let _ = ep.SetMute(false, std::ptr::null());
            }
            if let Some(st) = WIDGETS_STATE.get() {
                if let Ok(mut guard) = st.lock() {
                    guard.mic_volume_pct = clamped;
                    if clamped > 0 {
                        guard.mic_muted = false;
                    }
                }
            }
        }
    }
}

pub fn toggle_mic_mute() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Some(ep) = get_capture_endpoint_volume() {
            if let Ok(muted) = ep.GetMute() {
                let next_mute = !muted.as_bool();
                let _ = ep.SetMute(next_mute, std::ptr::null());
                if let Some(st) = WIDGETS_STATE.get() {
                    if let Ok(mut guard) = st.lock() {
                        guard.mic_muted = next_mute;
                    }
                }
            }
        }
    }
}

/// Switches Windows' active default audio output (`is_input == false`) or microphone (`is_input == true`)
/// device across `eConsole`, `eMultimedia`, and `eCommunications` roles via `IPolicyConfig::SetDefaultEndpoint`.
pub fn set_default_audio_device(device_id: &str, is_input: bool) {
    if device_id.is_empty() {
        return;
    }
    if let Some(st) = WIDGETS_STATE.get() {
        if let Ok(mut guard) = st.lock() {
            let list = if is_input {
                &mut guard.audio_inputs
            } else {
                &mut guard.audio_outputs
            };
            for dev in list.iter_mut() {
                dev.is_default = dev.id == device_id;
            }
        }
    }

    let id_owned = device_id.to_string();
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        set_default_endpoint_via_policy_config(&id_owned);
        let (outs, ins, mic_pct, mic_mute) = poll_audio_devices_and_mic();
        let (vol_pct, vol_muted) = poll_master_volume().unwrap_or((65, false));
        if let Some(st) = WIDGETS_STATE.get() {
            if let Ok(mut guard) = st.lock() {
                guard.audio_outputs = outs;
                guard.audio_inputs = ins;
                guard.mic_volume_pct = mic_pct;
                guard.mic_muted = mic_mute;
                guard.volume_pct = vol_pct;
                guard.volume_muted = vol_muted;
            }
        }
    });
}

pub fn toggle_bluetooth_enabled() -> bool {
    let mut new_state = true;
    if let Some(st) = WIDGETS_STATE.get() {
        if let Ok(mut guard) = st.lock() {
            guard.bluetooth_enabled = !guard.bluetooth_enabled;
            new_state = guard.bluetooth_enabled;
        }
    }
    std::thread::spawn(move || {
        set_bluetooth_radio_state(new_state);
    });
    new_state
}

pub fn toggle_bluetooth_device_connection(address: u64) {
    if address == 0 {
        return;
    }
    let mut target_connect = true;
    if let Some(st) = WIDGETS_STATE.get() {
        if let Ok(mut guard) = st.lock() {
            if let Some(dev) = guard
                .bluetooth_devices
                .iter_mut()
                .find(|d| d.address == address)
            {
                dev.connected = !dev.connected;
                target_connect = dev.connected;
            }
        }
    }
    std::thread::spawn(move || {
        set_bluetooth_device_service_connection(address, target_connect);
        let (bt_on, bt_devs) = poll_bluetooth_devices();
        if let Some(st) = WIDGETS_STATE.get() {
            if let Ok(mut guard) = st.lock() {
                if let Some(hw_bt_on) = bt_on {
                    guard.bluetooth_enabled = hw_bt_on;
                }
                guard.bluetooth_devices = bt_devs;
            }
        }
    });
}

pub fn refresh_bluetooth_devices() {
    std::thread::spawn(|| {
        let (bt_on, bt_devs) = poll_bluetooth_devices();
        if let Some(st) = WIDGETS_STATE.get() {
            if let Ok(mut guard) = st.lock() {
                if let Some(hw_bt_on) = bt_on {
                    guard.bluetooth_enabled = hw_bt_on;
                }
                guard.bluetooth_devices = bt_devs;
            }
        }
    });
}

pub fn toggle_idle_inhibit() -> bool {
    let mut new_state = false;
    if let Some(st) = WIDGETS_STATE.get() {
        if let Ok(mut guard) = st.lock() {
            guard.idle_inhibited = !guard.idle_inhibited;
            new_state = guard.idle_inhibited;
        }
    }
    let flag_path = std::env::temp_dir().join("hyprland_win_idle_inhibit.flag");
    if new_state {
        let _ = std::fs::write(&flag_path, "1");
    } else {
        let _ = std::fs::remove_file(&flag_path);
    }
    unsafe {
        if new_state {
            let _ = SetThreadExecutionState(
                ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED,
            );
        } else {
            let _ = SetThreadExecutionState(ES_CONTINUOUS);
        }
    }
    new_state
}

pub fn toggle_dnd_silenced() -> bool {
    let mut new_state = false;
    if let Some(st) = WIDGETS_STATE.get() {
        if let Ok(mut guard) = st.lock() {
            guard.dnd_silenced = !guard.dnd_silenced;
            new_state = guard.dnd_silenced;
        }
    }
    let flag_path = std::env::temp_dir().join("hyprland_win_dnd.flag");
    if new_state {
        let _ = std::fs::write(&flag_path, "1");
    } else {
        let _ = std::fs::remove_file(&flag_path);
    }
    new_state
}

pub fn activate_tray_item(item: &TrayAppItem) {
    unsafe {
        use windows::Win32::Foundation::{LPARAM, POINT, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{
            AllowSetForegroundWindow, GetCursorPos, IsWindow, PostMessageW, SetForegroundWindow,
            ASFW_ANY, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
        };
        // Only use system-provided Shell_NotifyIcon callbacks for this tray item
        let regs = find_all_registered_tray_icons_for_pid(item.pid);
        for reg in &regs {
            let tray_hwnd = HWND(reg.hwnd as *mut core::ffi::c_void);
            if IsWindow(tray_hwnd).as_bool() && reg.callback_msg != 0 {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let _ = AllowSetForegroundWindow(ASFW_ANY);
                let _ = SetForegroundWindow(tray_hwnd);
                if reg.version >= 4 {
                    let wp = WPARAM(
                        ((pt.x & 0xFFFF) as usize) | (((pt.y & 0xFFFF) as usize) << 16),
                    );
                    let lp_down =
                        LPARAM((WM_LBUTTONDOWN as isize) | ((reg.uid as isize) << 16));
                    let lp_up = LPARAM((WM_LBUTTONUP as isize) | ((reg.uid as isize) << 16));
                    let lp_dbl =
                        LPARAM((WM_LBUTTONDBLCLK as isize) | ((reg.uid as isize) << 16));
                    let _ = PostMessageW(tray_hwnd, reg.callback_msg, wp, lp_down);
                    let _ = PostMessageW(tray_hwnd, reg.callback_msg, wp, lp_up);
                    let _ = PostMessageW(tray_hwnd, reg.callback_msg, wp, lp_dbl);
                } else {
                    let wp = WPARAM(reg.uid as usize);
                    let _ = PostMessageW(
                        tray_hwnd,
                        reg.callback_msg,
                        wp,
                        LPARAM(WM_LBUTTONDOWN as isize),
                    );
                    let _ = PostMessageW(
                        tray_hwnd,
                        reg.callback_msg,
                        wp,
                        LPARAM(WM_LBUTTONUP as isize),
                    );
                    let _ = PostMessageW(
                        tray_hwnd,
                        reg.callback_msg,
                        wp,
                        LPARAM(WM_LBUTTONDBLCLK as isize),
                    );
                }
            }
        }
    }
}

pub fn context_menu_tray_item(item: &TrayAppItem) {
    let item_clone = item.clone();
    std::thread::spawn(move || unsafe {
        use windows::Win32::Foundation::{LPARAM, POINT, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{
            AllowSetForegroundWindow, GetCursorPos, IsWindow, PostMessageW, SetForegroundWindow,
            ASFW_ANY, WM_APP, WM_CONTEXTMENU, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_USER,
        };

        // Attach worker thread to Default desktop
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
        let desk_name: Vec<u16> = "Default\0".encode_utf16().collect();
        let h_desk = OpenDesktopW(desk_name.as_ptr(), 0, 0, 0x01FF);
        if !h_desk.is_null() {
            let _ = SetThreadDesktop(h_desk);
        }

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = AllowSetForegroundWindow(ASFW_ANY);

        let mut dispatched_native = false;

        // 1. Check if we captured an exact Shell_NotifyIcon registration for this process
        let regs = find_all_registered_tray_icons_for_pid(item_clone.pid);
        for reg in &regs {
            let tray_hwnd = HWND(reg.hwnd as *mut core::ffi::c_void);
            if IsWindow(tray_hwnd).as_bool() && reg.callback_msg != 0 {
                let _ = SetForegroundWindow(tray_hwnd);
                let wp_v4 = WPARAM(
                    ((pt.x & 0xFFFF) as usize) | (((pt.y & 0xFFFF) as usize) << 16),
                );
                if reg.version >= 4 {
                    let _ = PostMessageW(
                        tray_hwnd,
                        reg.callback_msg,
                        wp_v4,
                        LPARAM((WM_RBUTTONDOWN as isize) | ((reg.uid as isize) << 16)),
                    );
                    let _ = PostMessageW(
                        tray_hwnd,
                        reg.callback_msg,
                        wp_v4,
                        LPARAM((WM_RBUTTONUP as isize) | ((reg.uid as isize) << 16)),
                    );
                    let _ = PostMessageW(
                        tray_hwnd,
                        reg.callback_msg,
                        wp_v4,
                        LPARAM((WM_CONTEXTMENU as isize) | ((reg.uid as isize) << 16)),
                    );
                } else {
                    let wp = WPARAM(reg.uid as usize);
                    let _ = PostMessageW(
                        tray_hwnd,
                        reg.callback_msg,
                        wp,
                        LPARAM(WM_RBUTTONDOWN as isize),
                    );
                    let _ = PostMessageW(
                        tray_hwnd,
                        reg.callback_msg,
                        wp,
                        LPARAM(WM_RBUTTONUP as isize),
                    );
                    let _ = PostMessageW(
                        tray_hwnd,
                        reg.callback_msg,
                        wp,
                        LPARAM(WM_CONTEXTMENU as isize),
                    );
                }
                dispatched_native = true;
            }
        }

        // 2. Also forward to framework tray listener windows (Electron/Chromium `Chrome_StatusTrayWindow`,
        // Qt `QSystemTrayIconSys`, AutoHotkey, .NET `WindowsForms10`, etc.) if not in Shell_NotifyIcon cache
        if !dispatched_native && item_clone.pid != 0 {
            let candidate_hwnds = collect_pid_tray_candidate_hwnds(item_clone.pid);
            for (cand_hwnd, cls_name) in candidate_hwnds {
                let lower_cls = cls_name.to_ascii_lowercase();
                let callback_msgs: &[u32] = if lower_cls.contains("chrome_statustraywindow") {
                    &[WM_APP + 1, WM_APP + 2, WM_USER + 1]
                } else if lower_cls.contains("qsystemtrayiconsys") {
                    &[WM_APP + 101, WM_USER + 101]
                } else if lower_cls.contains("autohotkey") {
                    &[0x0404, WM_USER + 1]
                } else if lower_cls.contains("windowsforms10") {
                    &[WM_USER + 1024, WM_USER + 1, WM_APP + 1]
                } else {
                    &[
                        WM_APP + 1,
                        WM_APP + 2,
                        WM_USER + 1,
                        WM_USER + 2,
                        WM_USER + 4,
                        WM_USER + 5,
                        WM_USER + 100,
                        WM_USER + 1024,
                    ]
                };

                let _ = SetForegroundWindow(cand_hwnd);
                for &cb_msg in callback_msgs {
                    for uid in [0usize, 1usize] {
                        let _ = PostMessageW(
                            cand_hwnd,
                            cb_msg,
                            WPARAM(uid),
                            LPARAM(WM_RBUTTONDOWN as isize),
                        );
                        let _ = PostMessageW(
                            cand_hwnd,
                            cb_msg,
                            WPARAM(uid),
                            LPARAM(WM_RBUTTONUP as isize),
                        );
                        let _ = PostMessageW(
                            cand_hwnd,
                            cb_msg,
                            WPARAM(uid),
                            LPARAM(WM_CONTEXTMENU as isize),
                        );
                    }
                }
            }
        }
    });
}

pub fn spawn_detached_command(program: &str, args: &[&str]) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let prog_lower = program.to_ascii_lowercase();
    if prog_lower == "explorer.exe" || prog_lower == "explorer" {
        if let Some(first) = args.first() {
            if first.starts_with("ms-settings:") {
                let _ = std::process::Command::new("cmd.exe")
                    .args(["/C", "start", "", first])
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn();
                return;
            }
        }
        let mut cmd = std::process::Command::new("C:\\Windows\\explorer.exe");
        cmd.arg("/NoShellRegistrationCheck");
        if args.is_empty() {
            cmd.arg("/e,::{20D04FE0-3AEA-1069-A2D8-08002B30309D}");
        } else {
            cmd.args(args);
        }
        let _ = cmd.creation_flags(CREATE_NO_WINDOW).spawn();
        return;
    }
    let _ = std::process::Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

pub fn launch_hyprlauncher_toggle() {
    use std::os::windows::process::CommandExt;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};
    use windows::Win32::Foundation::WPARAM;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    unsafe {
        let cls_u16: Vec<u16> = "HyprLauncherClass\0".encode_utf16().collect();
        if let Ok(hwnd) = FindWindowW(PCWSTR(cls_u16.as_ptr()), PCWSTR::null()) {
            if !hwnd.0.is_null() {
                let _ = PostMessageW(hwnd, 0x0400 + 42, WPARAM(0), LPARAM(0));
                return;
            }
        }
    }

    if let Ok(cur_exe) = std::env::current_exe() {
        if let Some(dir) = cur_exe.parent() {
            let sibling = dir.join("hyprlauncher.exe");
            if sibling.is_file() {
                if std::process::Command::new(&sibling)
                    .arg("--toggle")
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn()
                    .is_ok()
                {
                    return;
                }
            }
        }
    }

    let candidates = [
        "target\\release\\hyprlauncher.exe",
        "target\\debug\\hyprlauncher.exe",
        "C:\\Users\\mbura\\Projects\\Other\\hyprland-win\\target\\release\\hyprlauncher.exe",
        "C:\\Users\\mbura\\Projects\\Other\\hyprland-win\\target\\debug\\hyprlauncher.exe",
        "hyprlauncher.exe",
    ];
    for c in candidates {
        if std::path::Path::new(c).is_file() || c == "hyprlauncher.exe" {
            if std::process::Command::new(c)
                .arg("--toggle")
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .is_ok()
            {
                return;
            }
        }
    }
}

pub fn launch_hyprlock() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    if let Ok(cur_exe) = std::env::current_exe() {
        if let Some(dir) = cur_exe.parent() {
            let sibling = dir.join("hyprlock.exe");
            if sibling.is_file() {
                if std::process::Command::new(&sibling)
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn()
                    .is_ok()
                {
                    return;
                }
            }
        }
    }

    let candidates = [
        "target\\release\\hyprlock.exe",
        "target\\debug\\hyprlock.exe",
        "C:\\Users\\mbura\\Projects\\Other\\hyprland-win\\target\\release\\hyprlock.exe",
        "C:\\Users\\mbura\\Projects\\Other\\hyprland-win\\target\\debug\\hyprlock.exe",
        "hyprlock.exe",
    ];
    for c in candidates {
        if std::path::Path::new(c).is_file() || c == "hyprlock.exe" {
            if std::process::Command::new(c)
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .is_ok()
            {
                return;
            }
        }
    }
}

pub fn launch_hyprpower_menu() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    if let Ok(cur_exe) = std::env::current_exe() {
        if let Some(dir) = cur_exe.parent() {
            let sibling = dir.join("hyprlock.exe");
            if sibling.is_file() {
                if std::process::Command::new(&sibling)
                    .arg("--power-menu")
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn()
                    .is_ok()
                {
                    return;
                }
            }
        }
    }

    let candidates = [
        "target\\release\\hyprlock.exe",
        "target\\debug\\hyprlock.exe",
        "C:\\Users\\mbura\\Projects\\Other\\hyprland-win\\target\\release\\hyprlock.exe",
        "C:\\Users\\mbura\\Projects\\Other\\hyprland-win\\target\\debug\\hyprlock.exe",
        "hyprlock.exe",
    ];
    for c in candidates {
        if std::path::Path::new(c).is_file() || c == "hyprlock.exe" {
            if std::process::Command::new(c)
                .arg("--power-menu")
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .is_ok()
            {
                return;
            }
        }
    }
}

pub fn launch_hyprshot(region: bool) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let mode_arg = if region { "region" } else { "output" };
    if let Ok(cur_exe) = std::env::current_exe() {
        if let Some(dir) = cur_exe.parent() {
            let sibling = dir.join("hyprshot.exe");
            if sibling.is_file() {
                if std::process::Command::new(&sibling)
                    .args(["-m", mode_arg])
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn()
                    .is_ok()
                {
                    return;
                }
            }
        }
    }

    let candidates = [
        "target\\release\\hyprshot.exe",
        "target\\debug\\hyprshot.exe",
        "C:\\Users\\mbura\\Projects\\Other\\hyprland-win\\target\\release\\hyprshot.exe",
        "C:\\Users\\mbura\\Projects\\Other\\hyprland-win\\target\\debug\\hyprshot.exe",
        "hyprshot.exe",
    ];
    for c in candidates {
        if std::path::Path::new(c).is_file() || c == "hyprshot.exe" {
            if std::process::Command::new(c)
                .args(["-m", mode_arg])
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .is_ok()
            {
                return;
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Internal Pollers (GSMTC Media, CoreAudio, Network, Battery)
// ----------------------------------------------------------------------------

fn classify_player_icon(app_id: &str) -> &'static str {
    let lower = app_id.to_ascii_lowercase();
    if lower.contains("spotify") {
        "\u{F1BC}" //  Spotify
    } else if lower.contains("chrome")
        || lower.contains("msedge")
        || lower.contains("zen")
        || lower.contains("firefox")
        || lower.contains("brave")
    {
        "\u{F04B}" //  Playing / Web Media
    } else {
        "\u{F001}" //  Music note
    }
}

struct MediaProgressTracker {
    title: String,
    raw_pos_sec: u32,
    base_pos_sec: u32,
    dur_sec: u32,
    last_sync: std::time::Instant,
}

static MEDIA_PROGRESS_TRACKER: OnceLock<Mutex<MediaProgressTracker>> = OnceLock::new();

fn compute_smooth_media_progress(
    title: &str,
    status: &MediaPlaybackStatus,
    raw_pos_sec: u32,
    raw_dur_sec: u32,
) -> (u32, u32) {
    let mtx = MEDIA_PROGRESS_TRACKER.get_or_init(|| {
        Mutex::new(MediaProgressTracker {
            title: String::new(),
            raw_pos_sec: 0,
            base_pos_sec: 0,
            dur_sec: 180,
            last_sync: std::time::Instant::now(),
        })
    });
    let Ok(mut tr) = mtx.lock() else {
        return (raw_pos_sec, raw_dur_sec.max(180));
    };
    let now = std::time::Instant::now();
    let dur = if raw_dur_sec > 0 { raw_dur_sec } else { 180 };

    if tr.title != title {
        tr.title = title.to_string();
        tr.raw_pos_sec = raw_pos_sec;
        tr.base_pos_sec = raw_pos_sec;
        tr.dur_sec = dur;
        tr.last_sync = now;
    } else if raw_dur_sec > 0 && raw_pos_sec != tr.raw_pos_sec {
        tr.raw_pos_sec = raw_pos_sec;
        tr.base_pos_sec = raw_pos_sec;
        tr.dur_sec = dur;
        tr.last_sync = now;
    } else if *status != MediaPlaybackStatus::Playing {
        let elapsed = now.duration_since(tr.last_sync).as_secs() as u32;
        tr.base_pos_sec = (tr.base_pos_sec + elapsed).min(tr.dur_sec);
        tr.last_sync = now;
    }

    let cur_pos = if *status == MediaPlaybackStatus::Playing {
        let elapsed = now.duration_since(tr.last_sync).as_secs() as u32;
        if raw_dur_sec > 0 {
            (tr.base_pos_sec + elapsed).min(tr.dur_sec)
        } else {
            (tr.base_pos_sec + elapsed) % tr.dur_sec.max(1)
        }
    } else {
        tr.base_pos_sec.min(tr.dur_sec)
    };

    (cur_pos, tr.dur_sec.max(1))
}

fn poll_media_session() -> MediaSessionSnapshot {
    // 1. Primary: Windows GSMTC (GlobalSystemMediaTransportControlsSessionManager)
    if let Ok(mgr_op) = windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager::RequestAsync() {
        if let Ok(mgr) = mgr_op.get() {
            if let Ok(session) = mgr.GetCurrentSession() {
                let status = session
                    .GetPlaybackInfo()
                    .and_then(|info| info.PlaybackStatus())
                    .map(|st| match st {
                        windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing => {
                            MediaPlaybackStatus::Playing
                        }
                        windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus::Paused => {
                            MediaPlaybackStatus::Paused
                        }
                        _ => MediaPlaybackStatus::Stopped,
                    })
                    .unwrap_or(MediaPlaybackStatus::Stopped);

                let app_id = session
                    .SourceAppUserModelId()
                    .map(|s| s.to_string_lossy())
                    .unwrap_or_default();

                let (raw_pos_sec, raw_dur_sec) = session
                    .GetTimelineProperties()
                    .map(|tl| {
                        let start_100ns = tl.StartTime().map(|t| t.Duration).unwrap_or(0);
                        let pos_100ns = tl.Position().map(|t| t.Duration).unwrap_or(0);
                        let end_100ns = tl.EndTime().map(|t| t.Duration).unwrap_or(0);
                        let pos_s = ((pos_100ns - start_100ns).max(0) / 10_000_000) as u32;
                        let dur_s = ((end_100ns - start_100ns).max(0) / 10_000_000) as u32;
                        (pos_s, dur_s)
                    })
                    .unwrap_or((0, 0));

                if let Ok(props_op) = session.TryGetMediaPropertiesAsync() {
                    if let Ok(props) = props_op.get() {
                        let title = props.Title().map(|s| s.to_string_lossy()).unwrap_or_default();
                        let artist = props.Artist().map(|s| s.to_string_lossy()).unwrap_or_default();
                        if !title.trim().is_empty() {
                            let eff_status = if status == MediaPlaybackStatus::Stopped {
                                MediaPlaybackStatus::Paused
                            } else {
                                status
                            };
                            let clean_title = title.trim().to_string();
                            let (pos_sec, dur_sec) = compute_smooth_media_progress(
                                &clean_title,
                                &eff_status,
                                raw_pos_sec,
                                raw_dur_sec,
                            );
                            return MediaSessionSnapshot {
                                status: eff_status,
                                player_icon: classify_player_icon(&app_id),
                                player_name: app_id,
                                title: clean_title,
                                artist: artist.trim().to_string(),
                                position_sec: pos_sec,
                                duration_sec: dur_sec,
                            };
                        }
                    }
                }
            }
        }
    }

    // 2. Fallback: Scan visible top-level windows for Spotify / VLC / MPV / Foobar2000 / YouTube / SoundCloud
    if let Some(mut win_media) = scan_win32_media_windows() {
        let (pos_sec, dur_sec) =
            compute_smooth_media_progress(&win_media.title, &win_media.status, 0, 0);
        win_media.position_sec = pos_sec;
        win_media.duration_sec = dur_sec;
        return win_media;
    }

    MediaSessionSnapshot::default()
}

struct MediaWinScanCtx {
    found: Option<MediaSessionSnapshot>,
}

fn scan_win32_media_windows() -> Option<MediaSessionSnapshot> {
    let mut ctx = MediaWinScanCtx { found: None };
    unsafe {
        let _ = EnumWindows(
            Some(enum_media_win_proc),
            LPARAM(&mut ctx as *mut MediaWinScanCtx as isize),
        );
    }
    ctx.found
}

unsafe extern "system" fn enum_media_win_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }
    let len = GetWindowTextLengthW(hwnd);
    if len <= 0 || len > 512 {
        return BOOL(1);
    }
    let mut buf = vec![0u16; (len + 1) as usize];
    let read_len = GetWindowTextW(hwnd, &mut buf);
    if read_len <= 0 {
        return BOOL(1);
    }
    let title = String::from_utf16_lossy(&buf[..read_len as usize])
        .trim()
        .to_string();
    if title.is_empty() {
        return BOOL(1);
    }

    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    let exe_name = query_process_basename(pid).to_ascii_lowercase();

    let ctx = &mut *(lparam.0 as *mut MediaWinScanCtx);

    if exe_name == "spotify.exe" {
        if title.eq_ignore_ascii_case("Spotify")
            || title.eq_ignore_ascii_case("Spotify Free")
            || title.eq_ignore_ascii_case("Spotify Premium")
        {
            ctx.found = Some(MediaSessionSnapshot {
                status: MediaPlaybackStatus::Paused,
                player_name: "Spotify".to_string(),
                player_icon: "\u{F1BC}",
                title: "Spotify (Paused)".to_string(),
                artist: String::new(),
                position_sec: 0,
                duration_sec: 180,
            });
        } else {
            let (artist, song) = title
                .split_once(" - ")
                .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
                .unwrap_or_else(|| (String::new(), title.clone()));
            ctx.found = Some(MediaSessionSnapshot {
                status: MediaPlaybackStatus::Playing,
                player_name: "Spotify".to_string(),
                player_icon: "\u{F1BC}",
                title: song,
                artist,
                position_sec: 0,
                duration_sec: 180,
            });
            return BOOL(0);
        }
    } else if exe_name == "vlc.exe" && title.ends_with(" - VLC media player") {
        let clean = title.trim_end_matches(" - VLC media player").trim().to_string();
        if !clean.is_empty() {
            ctx.found = Some(MediaSessionSnapshot {
                status: MediaPlaybackStatus::Playing,
                player_name: "VLC".to_string(),
                player_icon: "\u{F04B}",
                title: clean,
                artist: String::new(),
                position_sec: 0,
                duration_sec: 180,
            });
            return BOOL(0);
        }
    } else if exe_name == "mpv.exe" && title.ends_with(" - mpv") {
        let clean = title.trim_end_matches(" - mpv").trim().to_string();
        if !clean.is_empty() {
            ctx.found = Some(MediaSessionSnapshot {
                status: MediaPlaybackStatus::Playing,
                player_name: "mpv".to_string(),
                player_icon: "\u{F04B}",
                title: clean,
                artist: String::new(),
                position_sec: 0,
                duration_sec: 180,
            });
            return BOOL(0);
        }
    } else if (exe_name == "zen.exe"
        || exe_name == "chrome.exe"
        || exe_name == "msedge.exe"
        || exe_name == "firefox.exe"
        || exe_name == "brave.exe")
        && (title.contains(" - YouTube") || title.contains(" - SoundCloud"))
    {
        let clean = title
            .split(" - YouTube")
            .next()
            .unwrap_or(&title)
            .split(" - SoundCloud")
            .next()
            .unwrap_or(&title)
            .trim()
            .to_string();
        if !clean.is_empty() {
            ctx.found = Some(MediaSessionSnapshot {
                status: MediaPlaybackStatus::Playing,
                player_name: exe_name,
                player_icon: "\u{F04B}",
                title: clean,
                artist: String::new(),
                position_sec: 0,
                duration_sec: 180,
            });
            return BOOL(0);
        }
    }

    BOOL(1)
}

unsafe fn query_process_basename(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }
    if let Ok(h) = windows::Win32::System::Threading::OpenProcess(
        windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION,
        false,
        pid,
    ) {
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = windows::Win32::System::Threading::QueryFullProcessImageNameW(
            h,
            windows::Win32::System::Threading::PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(h);
        if ok && len > 0 {
            let full = String::from_utf16_lossy(&buf[..len as usize]);
            if let Some(base) = full.rsplit(['\\', '/']).next() {
                return base.to_string();
            }
        }
    }
    String::new()
}

unsafe fn get_audio_endpoint_volume() -> Option<IAudioEndpointVolume> {
    let enumerator: IMMDeviceEnumerator =
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
    let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
    let endpoint: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None).ok()?;
    Some(endpoint)
}

unsafe fn get_capture_endpoint_volume() -> Option<IAudioEndpointVolume> {
    use windows::Win32::Media::Audio::eCapture;
    let enumerator: IMMDeviceEnumerator =
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
    let device = enumerator.GetDefaultAudioEndpoint(eCapture, eConsole).ok()?;
    let endpoint: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None).ok()?;
    Some(endpoint)
}

fn poll_master_volume() -> Option<(u32, bool)> {
    unsafe {
        let ep = get_audio_endpoint_volume()?;
        let scalar = ep.GetMasterVolumeLevelScalar().ok()?.clamp(0.0, 1.0);
        let muted = ep.GetMute().ok()?.as_bool();
        Some(((scalar * 100.0).round() as u32, muted))
    }
}

unsafe fn read_immdevice_friendly_name(device: &windows::Win32::Media::Audio::IMMDevice) -> String {
    use windows::core::Interface;
    use windows::Win32::System::Com::CoTaskMemFree;

    #[repr(C)]
    struct PropertyKey {
        fmtid: windows::core::GUID,
        pid: u32,
    }
    #[repr(C)]
    struct RawPropVariant {
        vt: u16,
        w_reserved1: u16,
        w_reserved2: u16,
        w_reserved3: u16,
        data0: usize,
        data1: usize,
    }

    let raw_dev = device.as_raw();
    if raw_dev.is_null() {
        return String::new();
    }
    let dev_vtbl = *(raw_dev as *const *const usize);
    // IMMDevice::OpenPropertyStore is vtable slot 4 (STGM_READ = 0)
    let open_prop_store: unsafe extern "system" fn(
        *mut core::ffi::c_void,
        u32,
        *mut *mut core::ffi::c_void,
    ) -> i32 = std::mem::transmute(*dev_vtbl.add(4));

    let mut prop_store: *mut core::ffi::c_void = core::ptr::null_mut();
    if open_prop_store(raw_dev, 0, &mut prop_store) < 0 || prop_store.is_null() {
        return String::new();
    }

    let store_vtbl = *(prop_store as *const *const usize);
    // IPropertyStore::GetValue is vtable slot 5, IUnknown::Release is vtable slot 2
    let get_value: unsafe extern "system" fn(
        *mut core::ffi::c_void,
        *const PropertyKey,
        *mut RawPropVariant,
    ) -> i32 = std::mem::transmute(*store_vtbl.add(5));
    let release_fn: unsafe extern "system" fn(*mut core::ffi::c_void) -> u32 =
        std::mem::transmute(*store_vtbl.add(2));

    // PKEY_Device_FriendlyName = {a45c254e-df1c-4efd-8020-67d146a850e0}, 14
    let pkey_friendly = PropertyKey {
        fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
        pid: 14,
    };
    let mut pv = RawPropVariant {
        vt: 0,
        w_reserved1: 0,
        w_reserved2: 0,
        w_reserved3: 0,
        data0: 0,
        data1: 0,
    };

    let mut result = String::new();
    // VT_LPWSTR = 31
    if get_value(prop_store, &pkey_friendly, &mut pv) >= 0 && pv.vt == 31 && pv.data0 != 0 {
        let ptr = pv.data0 as *const u16;
        let mut len = 0usize;
        while len < 256 && *ptr.add(len) != 0 {
            len += 1;
        }
        result = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
            .trim()
            .to_string();
        CoTaskMemFree( Some(pv.data0 as *const core::ffi::c_void) );
    }

    let _ = release_fn(prop_store);
    result
}

unsafe fn enumerate_endpoints_for_flow(
    enumerator: &IMMDeviceEnumerator,
    data_flow: windows::Win32::Media::Audio::EDataFlow,
    is_input: bool,
) -> Vec<AudioDeviceEntry> {
    use windows::Win32::Media::Audio::DEVICE_STATE_ACTIVE;
    use windows::Win32::System::Com::CoTaskMemFree;

    let default_id = enumerator
        .GetDefaultAudioEndpoint(data_flow, eConsole)
        .ok()
        .and_then(|dev| unsafe {
            let pwstr = dev.GetId().ok()?;
            let s = pwstr.to_string().unwrap_or_default();
            CoTaskMemFree(Some(pwstr.0 as *const core::ffi::c_void));
            Some(s)
        })
        .unwrap_or_default();

    let Ok(collection) = enumerator.EnumAudioEndpoints(data_flow, DEVICE_STATE_ACTIVE) else {
        return Vec::new();
    };
    let count = collection.GetCount().unwrap_or(0);
    let mut out = Vec::with_capacity(count as usize);

    for i in 0..count {
        let Ok(dev) = collection.Item(i) else {
            continue;
        };
        let Ok(id_pwstr) = dev.GetId() else {
            continue;
        };
        let id_str = id_pwstr.to_string().unwrap_or_default();
        CoTaskMemFree(Some(id_pwstr.0 as *const core::ffi::c_void));
        if id_str.is_empty() {
            continue;
        }
        let mut name = read_immdevice_friendly_name(&dev);
        if name.is_empty() {
            name = if is_input {
                format!("Microphone {}", i + 1)
            } else {
                format!("Audio Output {}", i + 1)
            };
        }
        let is_default = !default_id.is_empty() && id_str == default_id;
        out.push(AudioDeviceEntry {
            id: id_str,
            name,
            is_default,
            is_input,
        });
    }

    out
}

fn poll_audio_devices_and_mic() -> (Vec<AudioDeviceEntry>, Vec<AudioDeviceEntry>, u32, bool) {
    unsafe {
        use windows::Win32::Media::Audio::eCapture;
        let Ok(enumerator) =
            CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
        else {
            return (Vec::new(), Vec::new(), 80, false);
        };
        let outputs = enumerate_endpoints_for_flow(&enumerator, eRender, false);
        let inputs = enumerate_endpoints_for_flow(&enumerator, eCapture, true);
        let (mic_pct, mic_mute) = if let Some(ep) = get_capture_endpoint_volume() {
            let scalar = ep.GetMasterVolumeLevelScalar().unwrap_or(0.8).clamp(0.0, 1.0);
            let muted = ep.GetMute().map(|b| b.as_bool()).unwrap_or(false);
            (((scalar * 100.0).round() as u32), muted)
        } else {
            (80, false)
        };
        (outputs, inputs, mic_pct, mic_mute)
    }
}

unsafe fn set_default_endpoint_via_policy_config(device_id: &str) {
    #[link(name = "ole32")]
    extern "system" {
        fn CoCreateInstance(
            rclsid: *const windows::core::GUID,
            punkouter: *mut core::ffi::c_void,
            dwclscontext: u32,
            riid: *const windows::core::GUID,
            ppv: *mut *mut core::ffi::c_void,
        ) -> i32;
    }

    let clsid_policy_config =
        windows::core::GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
    let iids = [
        // IID_IPolicyConfig (Win10/Win11)
        windows::core::GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8),
        // IID_IPolicyConfigVista (Fallback)
        windows::core::GUID::from_u128(0x568b9108_44bf_40b4_9006_86afe5b5a620),
    ];

    let id_u16: Vec<u16> = device_id.encode_utf16().chain(Some(0)).collect();

    for iid in &iids {
        let mut p_policy: *mut core::ffi::c_void = core::ptr::null_mut();
        if CoCreateInstance(
            &clsid_policy_config,
            core::ptr::null_mut(),
            CLSCTX_ALL.0,
            iid,
            &mut p_policy,
        ) >= 0
            && !p_policy.is_null()
        {
            let vtbl = *(p_policy as *const *const usize);
            // IPolicyConfig::SetDefaultEndpoint is vtable slot 13
            let set_default_endpoint: unsafe extern "system" fn(
                *mut core::ffi::c_void,
                *const u16,
                u32,
            ) -> i32 = std::mem::transmute(*vtbl.add(13));
            let release_fn: unsafe extern "system" fn(*mut core::ffi::c_void) -> u32 =
                std::mem::transmute(*vtbl.add(2));

            // Set for eConsole (0), eMultimedia (1), and eCommunications (2)
            let _ = set_default_endpoint(p_policy, id_u16.as_ptr(), 0);
            let _ = set_default_endpoint(p_policy, id_u16.as_ptr(), 1);
            let _ = set_default_endpoint(p_policy, id_u16.as_ptr(), 2);
            let _ = release_fn(p_policy);
            break;
        }
    }
}

#[repr(C)]
struct BluetoothFindRadioParams {
    dw_size: u32,
}

#[repr(C)]
struct BluetoothDeviceSearchParams {
    dw_size: u32,
    f_return_authenticated: i32,
    f_return_remembered: i32,
    f_return_unknown: i32,
    f_return_connected: i32,
    f_issue_inquiry: i32,
    c_timeout_multiplier: u8,
    _pad: [u8; 7],
    h_radio: isize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct BluetoothDeviceInfo {
    dw_size: u32,
    _pad0: u32,
    address: u64,
    ul_class_of_device: u32,
    f_connected: i32,
    f_remembered: i32,
    f_authenticated: i32,
    st_last_seen: [u16; 8],
    st_last_used: [u16; 8],
    sz_name: [u16; 248],
}

impl Default for BluetoothDeviceInfo {
    fn default() -> Self {
        Self {
            dw_size: std::mem::size_of::<Self>() as u32,
            _pad0: 0,
            address: 0,
            ul_class_of_device: 0,
            f_connected: 0,
            f_remembered: 0,
            f_authenticated: 0,
            st_last_seen: [0; 8],
            st_last_used: [0; 8],
            sz_name: [0; 248],
        }
    }
}

fn classify_bt_cod(cod: u32) -> &'static str {
    let major = (cod >> 8) & 0x1F;
    match major {
        0x04 => "audio", // Audio/Video (Headphones, Headset, Speakers)
        0x05 => "input", // Peripheral (Mouse, Keyboard, Gamepad)
        0x02 => "phone", // Phone
        _ => "device",
    }
}

fn poll_bluetooth_devices() -> (Option<bool>, Vec<BluetoothDeviceEntry>) {
    unsafe {
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

        let dll_name: Vec<u16> = "BluetoothAPIs.dll\0".encode_utf16().collect();
        let Ok(hmod) = LoadLibraryW(PCWSTR(dll_name.as_ptr())) else {
            return (None, Vec::new());
        };

        let find_first_radio = GetProcAddress(
            hmod,
            windows::core::PCSTR(b"BluetoothFindFirstRadio\0".as_ptr()),
        );
        let find_radio_close = GetProcAddress(
            hmod,
            windows::core::PCSTR(b"BluetoothFindRadioClose\0".as_ptr()),
        );
        let is_connectable = GetProcAddress(
            hmod,
            windows::core::PCSTR(b"BluetoothIsConnectable\0".as_ptr()),
        );
        let find_first_dev = GetProcAddress(
            hmod,
            windows::core::PCSTR(b"BluetoothFindFirstDevice\0".as_ptr()),
        );
        let find_next_dev = GetProcAddress(
            hmod,
            windows::core::PCSTR(b"BluetoothFindNextDevice\0".as_ptr()),
        );
        let find_dev_close = GetProcAddress(
            hmod,
            windows::core::PCSTR(b"BluetoothFindDeviceClose\0".as_ptr()),
        );

        let mut radio_on: Option<bool> = None;
        let mut h_radio: isize = 0;

        if let (Some(ffr), Some(frc)) = (find_first_radio, find_radio_close) {
            let ffr_fn: unsafe extern "system" fn(
                *const BluetoothFindRadioParams,
                *mut isize,
            ) -> isize = std::mem::transmute(ffr);
            let frc_fn: unsafe extern "system" fn(isize) -> i32 = std::mem::transmute(frc);
            let r_params = BluetoothFindRadioParams { dw_size: 4 };
            let h_find = ffr_fn(&r_params, &mut h_radio);
            if h_find != 0 {
                let _ = frc_fn(h_find);
                if let Some(ic) = is_connectable {
                    let ic_fn: unsafe extern "system" fn(isize) -> i32 = std::mem::transmute(ic);
                    let _ = ic_fn(h_radio);
                }
                radio_on = Some(true);
            } else {
                radio_on = Some(false);
            }
        }

        let mut devices = Vec::new();
        if let (Some(ffd), Some(fnd), Some(fdc)) = (find_first_dev, find_next_dev, find_dev_close)
        {
            let ffd_fn: unsafe extern "system" fn(
                *const BluetoothDeviceSearchParams,
                *mut BluetoothDeviceInfo,
            ) -> isize = std::mem::transmute(ffd);
            let fnd_fn: unsafe extern "system" fn(isize, *mut BluetoothDeviceInfo) -> i32 =
                std::mem::transmute(fnd);
            let fdc_fn: unsafe extern "system" fn(isize) -> i32 = std::mem::transmute(fdc);

            let search = BluetoothDeviceSearchParams {
                dw_size: std::mem::size_of::<BluetoothDeviceSearchParams>() as u32,
                f_return_authenticated: 1,
                f_return_remembered: 1,
                f_return_unknown: 0,
                f_return_connected: 1,
                f_issue_inquiry: 0,
                c_timeout_multiplier: 0,
                _pad: [0; 7],
                h_radio,
            };
            let mut info = BluetoothDeviceInfo::default();
            let h_dev_find = ffd_fn(&search, &mut info);
            if h_dev_find != 0 {
                loop {
                    let mut len = 0usize;
                    while len < info.sz_name.len() && info.sz_name[len] != 0 {
                        len += 1;
                    }
                    let name = String::from_utf16_lossy(&info.sz_name[..len])
                        .trim()
                        .to_string();
                    if !name.is_empty() && info.address != 0 {
                        devices.push(BluetoothDeviceEntry {
                            address: info.address,
                            name,
                            connected: info.f_connected != 0,
                            remembered: info.f_remembered != 0 || info.f_authenticated != 0,
                            category: classify_bt_cod(info.ul_class_of_device),
                        });
                    }
                    info = BluetoothDeviceInfo::default();
                    if fnd_fn(h_dev_find, &mut info) == 0 {
                        break;
                    }
                }
                let _ = fdc_fn(h_dev_find);
            }
        }

        if h_radio != 0 {
            let _ = CloseHandle(HANDLE(h_radio as *mut core::ffi::c_void));
        }

        // Sort connected devices first, then alphabetically
        devices.sort_by(|a, b| {
            b.connected
                .cmp(&a.connected)
                .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
        });

        (radio_on, devices)
    }
}

fn set_bluetooth_radio_state(enable: bool) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let target_state = if enable { "On" } else { "Off" };
    let ps_cmd = format!(
        "Add-Type -AssemblyName System.Runtime.WindowsRuntime; \
         $asTaskGeneric = ([System.WindowsRuntimeSystemExtensions].GetMethods() | ? {{ $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' }})[0]; \
         Function Await($WinRtTask, $ResultType) {{ $asTask = $asTaskGeneric.MakeGenericMethod($ResultType); $netTask = $asTask.Invoke($null, @($WinRtTask)); $netTask.Wait(-1) | Out-Null; $netTask.Result }}; \
         [Windows.Devices.Radios.Radio,Windows.System.Devices,ContentType=WindowsRuntime] | Out-Null; \
         [Windows.Devices.Radios.RadioAccessStatus,Windows.System.Devices,ContentType=WindowsRuntime] | Out-Null; \
         Await ([Windows.Devices.Radios.Radio]::RequestAccessAsync()) ([Windows.Devices.Radios.RadioAccessStatus]) | Out-Null; \
         $radios = Await ([Windows.Devices.Radios.Radio]::GetRadiosAsync()) ([System.Collections.Generic.IReadOnlyList[Windows.Devices.Radios.Radio]]); \
         $bt = $radios | ? {{ $_.Kind -eq 'Bluetooth' }}; \
         [Windows.Devices.Radios.RadioState,Windows.System.Devices,ContentType=WindowsRuntime] | Out-Null; \
         foreach ($r in $bt) {{ Await ($r.SetStateAsync('{target_state}')) ([Windows.Devices.Radios.RadioAccessStatus]) | Out-Null }}"
    );
    let _ = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &ps_cmd])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

fn set_bluetooth_device_service_connection(target_addr: u64, connect: bool) {
    unsafe {
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

        let dll_name: Vec<u16> = "BluetoothAPIs.dll\0".encode_utf16().collect();
        let Ok(hmod) = LoadLibraryW(PCWSTR(dll_name.as_ptr())) else {
            return;
        };

        let find_first_radio = GetProcAddress(
            hmod,
            windows::core::PCSTR(b"BluetoothFindFirstRadio\0".as_ptr()),
        );
        let find_radio_close = GetProcAddress(
            hmod,
            windows::core::PCSTR(b"BluetoothFindRadioClose\0".as_ptr()),
        );
        let find_first_dev = GetProcAddress(
            hmod,
            windows::core::PCSTR(b"BluetoothFindFirstDevice\0".as_ptr()),
        );
        let find_next_dev = GetProcAddress(
            hmod,
            windows::core::PCSTR(b"BluetoothFindNextDevice\0".as_ptr()),
        );
        let find_dev_close = GetProcAddress(
            hmod,
            windows::core::PCSTR(b"BluetoothFindDeviceClose\0".as_ptr()),
        );
        let set_svc_state = GetProcAddress(
            hmod,
            windows::core::PCSTR(b"BluetoothSetServiceState\0".as_ptr()),
        );

        let (Some(ffr), Some(frc), Some(ffd), Some(fnd), Some(fdc), Some(sss)) = (
            find_first_radio,
            find_radio_close,
            find_first_dev,
            find_next_dev,
            find_dev_close,
            set_svc_state,
        ) else {
            return;
        };

        let ffr_fn: unsafe extern "system" fn(*const BluetoothFindRadioParams, *mut isize) -> isize =
            std::mem::transmute(ffr);
        let frc_fn: unsafe extern "system" fn(isize) -> i32 = std::mem::transmute(frc);
        let ffd_fn: unsafe extern "system" fn(
            *const BluetoothDeviceSearchParams,
            *mut BluetoothDeviceInfo,
        ) -> isize = std::mem::transmute(ffd);
        let fnd_fn: unsafe extern "system" fn(isize, *mut BluetoothDeviceInfo) -> i32 =
            std::mem::transmute(fnd);
        let fdc_fn: unsafe extern "system" fn(isize) -> i32 = std::mem::transmute(fdc);
        let sss_fn: unsafe extern "system" fn(
            isize,
            *const BluetoothDeviceInfo,
            *const windows::core::GUID,
            u32,
        ) -> u32 = std::mem::transmute(sss);

        let mut h_radio: isize = 0;
        let r_params = BluetoothFindRadioParams { dw_size: 4 };
        let h_find = ffr_fn(&r_params, &mut h_radio);
        if h_find != 0 {
            let _ = frc_fn(h_find);
        }

        let search = BluetoothDeviceSearchParams {
            dw_size: std::mem::size_of::<BluetoothDeviceSearchParams>() as u32,
            f_return_authenticated: 1,
            f_return_remembered: 1,
            f_return_unknown: 0,
            f_return_connected: 1,
            f_issue_inquiry: 0,
            c_timeout_multiplier: 0,
            _pad: [0; 7],
            h_radio,
        };
        let mut info = BluetoothDeviceInfo::default();
        let h_dev_find = ffd_fn(&search, &mut info);
        if h_dev_find != 0 {
            loop {
                if info.address == target_addr {
                    let service_guids = [
                        // AudioSinkServiceClass_UUID (A2DP)
                        windows::core::GUID::from_u128(0x0000110b_0000_1000_8000_00805f9b34fb),
                        // HandsfreeServiceClass_UUID (HFP)
                        windows::core::GUID::from_u128(0x0000111e_0000_1000_8000_00805f9b34fb),
                        // HumanInterfaceDeviceServiceClass_UUID (HID)
                        windows::core::GUID::from_u128(0x00001124_0000_1000_8000_00805f9b34fb),
                    ];
                    let flags = if connect { 0x01u32 } else { 0x00u32 };
                    for guid in &service_guids {
                        let _ = sss_fn(h_radio, &info, guid, flags);
                    }
                    break;
                }
                info = BluetoothDeviceInfo::default();
                if fnd_fn(h_dev_find, &mut info) == 0 {
                    break;
                }
            }
            let _ = fdc_fn(h_dev_find);
        }

        if h_radio != 0 {
            let _ = CloseHandle(HANDLE(h_radio as *mut core::ffi::c_void));
        }
    }
}

fn poll_network_details() -> (NetworkKind, String, u32) {
    unsafe {
        let mut size: u32 = 0;
        let _ = GetIfTable(None, &mut size, false);
        if size == 0 || size > 1024 * 1024 {
            return (NetworkKind::Ethernet, "Ethernet Adapter".to_string(), 1000);
        }
        let mut buf = vec![0u8; size as usize];
        let table_ptr = buf.as_mut_ptr() as *mut MIB_IFTABLE;
        if GetIfTable(Some(table_ptr), &mut size, false) != 0 {
            return (NetworkKind::Ethernet, "Ethernet Adapter".to_string(), 1000);
        }
        let num_entries = (*table_ptr).dwNumEntries as usize;
        let rows = std::slice::from_raw_parts((*table_ptr).table.as_ptr(), num_entries);

        let mut wifi_info: Option<(String, u32)> = None;
        let mut eth_info: Option<(String, u32)> = None;

        for row in rows {
            let oper = row.dwOperStatus.0;
            // 4 = IF_OPER_STATUS_CONNECTED, 5 = IF_OPER_STATUS_OPERATIONAL
            let is_up = oper == 4 || oper == 5;
            if !is_up {
                continue;
            }
            let d_len = (row.dwDescrLen as usize).min(row.bDescr.len());
            let descr = String::from_utf8_lossy(&row.bDescr[..d_len])
                .trim_end_matches('\0')
                .trim()
                .to_string();
            let mbps = (row.dwSpeed / 1_000_000).max(1);

            // 71 = IF_TYPE_IEEE80211 (Wi-Fi), 6 = IF_TYPE_ETHERNET_CSMACD
            if row.dwType == 71 {
                wifi_info = Some((
                    if descr.is_empty() {
                        "Wi-Fi Adapter".to_string()
                    } else {
                        descr
                    },
                    mbps,
                ));
            } else if row.dwType == 6 && (row.dwInOctets > 0 || row.dwOutOctets > 0) && eth_info.is_none() {
                eth_info = Some((
                    if descr.is_empty() {
                        "Ethernet Adapter".to_string()
                    } else {
                        descr
                    },
                    mbps,
                ));
            }
        }
        if let Some((name, mbps)) = wifi_info {
            (NetworkKind::Wifi, truncate_chars(&name, 26), mbps)
        } else if let Some((name, mbps)) = eth_info {
            (NetworkKind::Ethernet, truncate_chars(&name, 26), mbps)
        } else {
            (NetworkKind::Disconnected, "Disconnected".to_string(), 0)
        }
    }
}

#[derive(Debug, Clone)]
struct RegisteredNotifyIcon {
    hwnd: u64,
    pid: u32,
    uid: u32,
    callback_msg: u32,
    version: u32,
}

static SHELL_TRAY_ICONS: OnceLock<Mutex<Vec<RegisteredNotifyIcon>>> = OnceLock::new();
static SHELL_TRAY_HOST_STARTED: OnceLock<()> = OnceLock::new();

fn ensure_shell_tray_host_started() {
    SHELL_TRAY_HOST_STARTED.get_or_init(|| {
        std::thread::Builder::new()
            .name("hyprbar-shell-tray-host".to_string())
            .spawn(|| unsafe {
                use windows::Win32::Foundation::{LRESULT, WPARAM};
                use windows::Win32::System::DataExchange::COPYDATASTRUCT;
                use windows::Win32::UI::WindowsAndMessaging::{
                    CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowW, GetMessageW,
                    RegisterClassW, RegisterWindowMessageW, SendNotifyMessageW, TranslateMessage,
                    HWND_BROADCAST, MSG, WM_COPYDATA, WNDCLASSW, WS_EX_NOACTIVATE,
                    WS_EX_TOOLWINDOW, WS_POPUP,
                };

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
                let desk_name: Vec<u16> = "Default\0".encode_utf16().collect();
                let h_desk = OpenDesktopW(desk_name.as_ptr(), 0, 0, 0x01FF);
                if !h_desk.is_null() {
                    let _ = SetThreadDesktop(h_desk);
                }

                unsafe extern "system" fn shell_tray_wnd_proc(
                    hwnd: HWND,
                    msg: u32,
                    wparam: WPARAM,
                    lparam: LPARAM,
                ) -> LRESULT {
                    if msg == WM_COPYDATA && lparam.0 != 0 {
                        let cds = &*(lparam.0 as *const COPYDATASTRUCT);
                        if cds.dwData == 1 && !cds.lpData.is_null() && cds.cbData >= 32 {
                            let bytes = std::slice::from_raw_parts(
                                cds.lpData as *const u8,
                                cds.cbData as usize,
                            );
                            let sig = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                            let dw_msg =
                                u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                            if sig == 0x3475_3423 {
                                let nid_hwnd_u32 = u32::from_le_bytes([
                                    bytes[12], bytes[13], bytes[14], bytes[15],
                                ]);
                                let uid = u32::from_le_bytes([
                                    bytes[16], bytes[17], bytes[18], bytes[19],
                                ]);
                                let uflags = u32::from_le_bytes([
                                    bytes[20], bytes[21], bytes[22], bytes[23],
                                ]);
                                let cb_msg = u32::from_le_bytes([
                                    bytes[24], bytes[25], bytes[26], bytes[27],
                                ]);

                                let target_hwnd = if nid_hwnd_u32 != 0 {
                                    nid_hwnd_u32 as u64
                                } else {
                                    wparam.0 as u64
                                };
                                let mut pid = 0u32;
                                if target_hwnd != 0 {
                                    GetWindowThreadProcessId(
                                        HWND(target_hwnd as *mut core::ffi::c_void),
                                        Some(&mut pid),
                                    );
                                }

                                let store =
                                    SHELL_TRAY_ICONS.get_or_init(|| Mutex::new(Vec::new()));
                                if let Ok(mut list) = store.lock() {
                                    match dw_msg {
                                        0 | 1 => {
                                            // NIM_ADD (0) | NIM_MODIFY (1)
                                            if let Some(existing) = list.iter_mut().find(|it| {
                                                it.hwnd == target_hwnd && it.uid == uid
                                            }) {
                                                if (uflags & 0x01) != 0 && cb_msg != 0 {
                                                    existing.callback_msg = cb_msg;
                                                }
                                                if pid != 0 {
                                                    existing.pid = pid;
                                                }
                                            } else if target_hwnd != 0 {
                                                list.push(RegisteredNotifyIcon {
                                                    hwnd: target_hwnd,
                                                    pid,
                                                    uid,
                                                    callback_msg: cb_msg,
                                                    version: 0,
                                                });
                                            }
                                        }
                                        2 => {
                                            // NIM_DELETE (2)
                                            list.retain(|it| {
                                                !(it.hwnd == target_hwnd && it.uid == uid)
                                            });
                                        }
                                        4 => {
                                            // NIM_SETVERSION (4)
                                            if bytes.len() >= 812 {
                                                let ver = u32::from_le_bytes([
                                                    bytes[808], bytes[809], bytes[810], bytes[811],
                                                ]);
                                                if let Some(existing) =
                                                    list.iter_mut().find(|it| {
                                                        it.hwnd == target_hwnd && it.uid == uid
                                                    })
                                                {
                                                    existing.version = ver.min(4);
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                return LRESULT(1);
                            }
                        }
                    }
                    DefWindowProcW(hwnd, msg, wparam, lparam)
                }

                let cls: Vec<u16> = "Shell_TrayWnd\0".encode_utf16().collect();
                // Only host Shell_TrayWnd if Explorer's Shell_TrayWnd is not currently present
                let existing = FindWindowW(PCWSTR(cls.as_ptr()), PCWSTR::null()).unwrap_or_default();
                if existing.0.is_null() {
                    let wc = WNDCLASSW {
                        lpfnWndProc: Some(shell_tray_wnd_proc),
                        lpszClassName: PCWSTR(cls.as_ptr()),
                        ..Default::default()
                    };
                    let _ = RegisterClassW(&wc);
                    if let Ok(hwnd) = CreateWindowExW(
                        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                        PCWSTR(cls.as_ptr()),
                        PCWSTR::null(),
                        WS_POPUP,
                        0,
                        0,
                        0,
                        0,
                        HWND::default(),
                        None,
                        None,
                        None,
                    ) {
                        if !hwnd.0.is_null() {
                            let tb_msg_name: Vec<u16> =
                                "TaskbarCreated\0".encode_utf16().collect();
                            let wm_taskbar_created =
                                RegisterWindowMessageW(PCWSTR(tb_msg_name.as_ptr()));
                            if wm_taskbar_created != 0 {
                                let _ = SendNotifyMessageW(
                                    HWND_BROADCAST,
                                    wm_taskbar_created,
                                    WPARAM(0),
                                    LPARAM(0),
                                );
                            }
                            let mut msg = MSG::default();
                            while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                                let _ = TranslateMessage(&msg);
                                DispatchMessageW(&msg);
                            }
                        }
                    }
                }
            })
            .ok();
    });
}

fn find_registered_tray_icon_for_pid(pid: u32) -> Option<RegisteredNotifyIcon> {
    if pid == 0 {
        return None;
    }
    let store = SHELL_TRAY_ICONS.get()?;
    let guard = store.lock().ok()?;
    guard
        .iter()
        .find(|it| it.pid == pid && it.callback_msg != 0)
        .cloned()
}

fn find_all_registered_tray_icons_for_pid(pid: u32) -> Vec<RegisteredNotifyIcon> {
    if pid == 0 {
        return Vec::new();
    }
    let Some(store) = SHELL_TRAY_ICONS.get() else {
        return Vec::new();
    };
    let Ok(guard) = store.lock() else {
        return Vec::new();
    };
    guard
        .iter()
        .filter(|it| it.pid == pid && it.callback_msg != 0)
        .cloned()
        .collect()
}

unsafe fn collect_pid_tray_candidate_hwnds(target_pid: u32) -> Vec<(HWND, String)> {
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetClassNameW, HWND_MESSAGE};
    struct PidScanCtx {
        target_pid: u32,
        out: Vec<(HWND, String)>,
    }
    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut PidScanCtx);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == ctx.target_pid {
            let mut cls_buf = [0u16; 128];
            let n = GetClassNameW(hwnd, &mut cls_buf);
            let cls = String::from_utf16_lossy(&cls_buf[..n.max(0) as usize]);
            if cls != "IME" && cls != "MSCTFIME UI" && cls != "CiceroUIWndFrame" {
                ctx.out.push((hwnd, cls));
            }
        }
        BOOL(1)
    }
    let mut ctx = PidScanCtx {
        target_pid,
        out: Vec::new(),
    };
    let _ = EnumWindows(Some(enum_cb), LPARAM(&mut ctx as *mut PidScanCtx as isize));

    // Also inspect message-only windows (`HWND_MESSAGE`) for this process
    let mut cur = HWND::default();
    let mut steps = 0;
    while steps < 512 {
        let next = FindWindowExW(HWND_MESSAGE, cur, PCWSTR::null(), PCWSTR::null())
            .unwrap_or_default();
        if next.0.is_null() {
            break;
        }
        cur = next;
        steps += 1;
        let mut pid = 0u32;
        GetWindowThreadProcessId(cur, Some(&mut pid));
        if pid == target_pid {
            let mut cls_buf = [0u16; 128];
            let n = GetClassNameW(cur, &mut cls_buf);
            let cls = String::from_utf16_lossy(&cls_buf[..n.max(0) as usize]);
            if cls != "IME" && cls != "MSCTFIME UI" {
                ctx.out.push((cur, cls));
            }
        }
    }

    // Prioritize dedicated tray classes first (`Chrome_StatusTrayWindow`, `QSystemTrayIconSys`, etc.)
    ctx.out.sort_by_key(|(_, cls)| {
        let l = cls.to_ascii_lowercase();
        if l.contains("statustray") || l.contains("trayicon") || l.contains("notifyicon") {
            0
        } else if l.contains("autohotkey") || l.contains("windowsforms10") || l.contains("vgui") {
            1
        } else {
            2
        }
    });
    ctx.out
}

unsafe fn enable_win32_dark_context_menus() {
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    static DARK_MENU_INIT: OnceLock<()> = OnceLock::new();
    DARK_MENU_INIT.get_or_init(|| {
        let lib_name: Vec<u16> = "uxtheme.dll\0".encode_utf16().collect();
        if let Ok(hmod) = LoadLibraryW(PCWSTR(lib_name.as_ptr())) {
            // Ordinal 135: SetPreferredAppMode(AllowDark = 1)
            if let Some(p135) = GetProcAddress(hmod, windows::core::PCSTR(135usize as *const u8)) {
                let set_pref: unsafe extern "system" fn(i32) -> i32 =
                    std::mem::transmute(p135);
                let _ = set_pref(1);
            }
            // Ordinal 136: FlushMenuThemes()
            if let Some(p136) = GetProcAddress(hmod, windows::core::PCSTR(136usize as *const u8)) {
                let flush_themes: unsafe extern "system" fn() = std::mem::transmute(p136);
                flush_themes();
            }
        }
    });
}

fn poll_system_tray_items() -> Vec<TrayAppItem> {
    ensure_shell_tray_host_started();

    struct ScanCtx {
        items: Vec<TrayAppItem>,
    }
    unsafe extern "system" fn enum_tray_win(hwnd: HWND, lparam: LPARAM) -> BOOL {
        use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
        let ctx = &mut *(lparam.0 as *mut ScanCtx);
        if ctx.items.len() >= 8 {
            return BOOL(0);
        }

        // Skip system IME / helper windows so we always bind to the application's real window!
        let mut cls_buf = [0u16; 128];
        let cls_len = GetClassNameW(hwnd, &mut cls_buf);
        if cls_len > 0 {
            let cls_name = String::from_utf16_lossy(&cls_buf[..cls_len as usize]);
            if matches!(
                cls_name.as_str(),
                "IME"
                    | "MSCTFIME UI"
                    | "CiceroUIWndFrame"
                    | "GDI+ Hook Window Class"
                    | "DummyDWMListenerWindow"
                    | "WorkerW"
            ) {
                return BOOL(1);
            }
        }

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return BOOL(1);
        }

        let (exe_base, exe_full) = query_process_paths(pid);
        if exe_base.is_empty() {
            return BOOL(1);
        }
        let lower = exe_base.to_ascii_lowercase();

        // Recognize known system-tray resident apps (both minimized/hidden to tray and active)
        let tray_label: Option<&'static str> = match lower.as_str() {
            "discord.exe" => Some("Discord"),
            "steam.exe" | "steamwebhelper.exe" => Some("Steam"),
            "spotify.exe" => Some("Spotify"),
            "vgtray.exe" => Some("Riot Vanguard"),
            "riotclientservices.exe" | "riot client.exe" => Some("Riot Client"),
            "nvcontainer.exe" | "nvidia share.exe" | "nvidia app.exe" => Some("NVIDIA"),
            "securityhealthsystray.exe" => Some("Windows Security"),
            "onedrive.exe" => Some("OneDrive"),
            "obs64.exe" => Some("OBS Studio"),
            "sharex.exe" => Some("ShareX"),
            "everything.exe" => Some("Everything"),
            "autohotkey.exe" | "autohotkey64.exe" => Some("AutoHotkey"),
            "powertoys.runner.exe" | "powertoys.exe" => Some("PowerToys"),
            "qbittorrent.exe" => Some("qBittorrent"),
            "wireguard.exe" => Some("WireGuard"),
            "tailscale-ipn.exe" => Some("Tailscale"),
            "docker desktop.exe" => Some("Docker Desktop"),
            "lghub_system_tray.exe" | "lghub.exe" => Some("Logitech G HUB"),
            "razer central.exe" | "razer synapse 3.exe" => Some("Razer Synapse"),
            "twinkletray.exe" => Some("Twinkle Tray"),
            "eartrumpet.exe" => Some("EarTrumpet"),
            "f.lux.exe" => Some("f.lux"),
            "telegram.exe" => Some("Telegram"),
            "whatsapp.exe" => Some("WhatsApp"),
            "vesktop.exe" => Some("Vesktop"),
            _ => None,
        };

        let Some(label) = tray_label else {
            return BOOL(1);
        };

        let is_vis = IsWindowVisible(hwnd).as_bool();
        let has_title = GetWindowTextLengthW(hwnd) > 0;

        if let Some(existing) = ctx.items.iter_mut().find(|it| it.name == label) {
            // Upgrade to a visible/titled main window if the previously recorded HWND was hidden/untitled
            let prev_hwnd = HWND(existing.hwnd as *mut core::ffi::c_void);
            let prev_vis = IsWindowVisible(prev_hwnd).as_bool();
            if is_vis && !prev_vis && has_title {
                existing.hwnd = hwnd.0 as u64;
                existing.pid = pid;
            }
            return BOOL(1);
        }

        ctx.items.push(TrayAppItem {
            hwnd: hwnd.0 as u64,
            pid,
            name: label.to_string(),
            exe_path: exe_full,
        });
        BOOL(1)
    }

    let mut ctx = ScanCtx { items: Vec::new() };
    unsafe {
        let _ = EnumWindows(
            Some(enum_tray_win),
            LPARAM(&mut ctx as *mut ScanCtx as isize),
        );
    }
    ctx.items
}

unsafe fn query_process_paths(pid: u32) -> (String, String) {
    if pid == 0 {
        return (String::new(), String::new());
    }
    if let Ok(h) = windows::Win32::System::Threading::OpenProcess(
        windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION,
        false,
        pid,
    ) {
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = windows::Win32::System::Threading::QueryFullProcessImageNameW(
            h,
            windows::Win32::System::Threading::PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(h);
        if ok && len > 0 {
            let full = String::from_utf16_lossy(&buf[..len as usize]);
            if let Some(base) = full.rsplit(['\\', '/']).next() {
                return (base.to_string(), full);
            }
        }
    }
    (String::new(), String::new())
}

fn poll_battery_status() -> (bool, u32, bool) {
    unsafe {
        let mut sps = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut sps).is_ok() {
            // BatteryFlag: 128 = No system battery, 255 = Unknown
            let has_battery = sps.BatteryFlag != 128 && sps.BatteryFlag != 255;
            let pct = if sps.BatteryLifePercent <= 100 {
                sps.BatteryLifePercent as u32
            } else {
                100
            };
            let charging = sps.ACLineStatus == 1 || (sps.BatteryFlag & 8) != 0;
            return (has_battery, pct, charging);
        }
    }
    (false, 100, false)
}

pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Resolves the active window title for the top-center title bar island without premature truncation,
/// allowing `ui.rs` to display up to 60 characters and smoothly scroll longer titles left and right.
pub fn format_v2_window_title(title: &str, class: &str) -> String {
    let t = title.trim();
    let c = class.trim().trim_end_matches(".exe").trim();
    if t.is_empty() && c.is_empty() {
        return String::new();
    }

    let c_lower = c.to_ascii_lowercase();
    if c_lower == "progman" || c_lower == "workerw" {
        return String::new();
    }
    if t.eq_ignore_ascii_case("Hyprland-Win Desktop") {
        return String::new();
    }

    let t_clean = t
        .trim_end_matches(" (Administrator)")
        .trim_end_matches(" [Administrator]")
        .trim_end_matches(" (Admin)")
        .trim();

    if !t_clean.is_empty() {
        return t_clean.to_string();
    }

    match c_lower.as_str() {
        "cabinetwclass" | "explorewclass" => "File Explorer".to_string(),
        "cascadia_hosting_window_class" | "consolewindowclass" => "Terminal".to_string(),
        "mozillawindowclass" => "Firefox".to_string(),
        "notepad" => "Notepad".to_string(),
        "taskmanagerwindow" => "Task Manager".to_string(),
        "vguipopupwindow" => "Steam".to_string(),
        _ => c.to_string(),
    }
}

pub fn query_windows_11_build_label() -> String {
    unsafe {
        use windows::Win32::System::Registry::{
            RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
            REG_VALUE_TYPE,
        };
        let subkey: Vec<u16> = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\0"
            .encode_utf16()
            .collect();
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            0,
            KEY_READ,
            &mut hkey,
        )
        .is_ok()
        {
            let val_name: Vec<u16> = "CurrentBuildNumber\0".encode_utf16().collect();
            let mut buf = [0u16; 64];
            let mut cb = (buf.len() * 2) as u32;
            let mut kind = REG_VALUE_TYPE::default();
            let mut build_str = String::new();
            if RegQueryValueExW(
                hkey,
                PCWSTR(val_name.as_ptr()),
                None,
                Some(&mut kind),
                Some(buf.as_mut_ptr() as *mut u8),
                Some(&mut cb),
            )
            .is_ok()
            {
                let len = (cb as usize / 2).saturating_sub(1).min(buf.len());
                build_str = String::from_utf16_lossy(&buf[..len])
                    .trim_matches('\0')
                    .trim()
                    .to_string();
            }

            let ubr_name: Vec<u16> = "UBR\0".encode_utf16().collect();
            let mut ubr: u32 = 0;
            let mut ubr_cb = 4u32;
            let has_ubr = RegQueryValueExW(
                hkey,
                PCWSTR(ubr_name.as_ptr()),
                None,
                Some(&mut kind),
                Some(&mut ubr as *mut u32 as *mut u8),
                Some(&mut ubr_cb),
            )
            .is_ok();
            let _ = RegCloseKey(hkey);

            if !build_str.is_empty() {
                if has_ubr && ubr > 0 {
                    return format!("Windows 11 ({build_str}.{ubr})");
                }
                return format!("Windows 11 ({build_str})");
            }
        }
    }
    "Windows 11 (26100)".to_string()
}

pub fn read_configured_weather_location() -> String {
    let conf_path = hyprland_win_core::hyprland_win_config_file("hyprland.conf");
    let cfg = hyprland_win_core::HyprConfig::load_from_file(&conf_path);
    if !cfg.weather_location.trim().is_empty() {
        return cfg.weather_location;
    }
    "Istanbul, Tuzla".to_string()
}

fn poll_weather_for_location(location: &str) -> Option<(i32, String, String, &'static str)> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let loc_lower = location.to_ascii_lowercase();
    let (mut lat, mut lon) = (40.8161_f64, 29.3006_f64); // Default: Istanbul, Tuzla
    if !(loc_lower.contains("tuzla") || loc_lower.contains("istanbul")) {
        let query_token = location
            .split([',', ' '])
            .filter(|s| !s.trim().is_empty())
            .next_back()
            .unwrap_or(location)
            .trim();
        let geo_url = format!(
            "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=en&format=json",
            query_token
        );
        if let Ok(out) = std::process::Command::new("curl.exe")
            .args(["-s", "--max-time", "4", &geo_url])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                if let Some(first) = json.get("results").and_then(|r| r.get(0)) {
                    if let (Some(la), Some(lo)) = (
                        first.get("latitude").and_then(|v| v.as_f64()),
                        first.get("longitude").and_then(|v| v.as_f64()),
                    ) {
                        lat = la;
                        lon = lo;
                    }
                }
            }
        }
    }

    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat:.4}&longitude={lon:.4}&current=temperature_2m,weather_code,is_day"
    );
    let out = std::process::Command::new("curl.exe")
        .args(["-s", "--max-time", "4", &url])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let cur = json.get("current")?;
    let temp = cur.get("temperature_2m")?.as_f64()?.round() as i32;
    let code = cur.get("weather_code").and_then(|v| v.as_u64()).unwrap_or(0);
    let is_day = cur.get("is_day").and_then(|v| v.as_u64()).unwrap_or(1) != 0;

    let (desc, icon) = match code {
        0 => (
            "Clear sky",
            if is_day { "\u{F185}" } else { "\u{F186}" },
        ),
        1 | 2 => ("Partly cloudy", "\u{F0C2}"),
        3 => ("Overcast", "\u{F0C2}"),
        45 | 48 => ("Foggy", "\u{F0C2}"),
        51..=67 | 80..=82 => ("Rainy", "\u{F0E9}"),
        71..=77 | 85 | 86 => ("Snowy", "\u{F2DC}"),
        95..=99 => ("Thunderstorm", "\u{F0E7}"),
        _ => ("Partly cloudy", "\u{F0C2}"),
    };

    Some((temp, desc.to_string(), location.to_string(), icon))
}
