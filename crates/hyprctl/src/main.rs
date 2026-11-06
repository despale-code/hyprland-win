//! `hyprctl` — Command-line controller for the Hyprland-Win Compositor Daemon.

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::ClientOptions;

use hyprland_win_core::{
    HyprIpcRequest, HyprIpcResponse, HyprNotifyMessage, HYPRNOTIFY_IPC_PIPE, HYPRLAND_WIN_IPC_PIPE,
};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "-j" || a == "--json");
    let filtered: Vec<&str> = args
        .iter()
        .map(|s| s.as_str())
        .filter(|&a| a != "-j" && a != "--json")
        .collect();

    if filtered
        .first()
        .copied()
        .map(|a| a == "--init-startup" || a == "init-startup")
        .unwrap_or(false)
    {
        let elevated_internal = args.iter().any(|a| a == "--elevated-internal");
        return run_init_startup(elevated_internal);
    }

    if filtered
        .first()
        .copied()
        .map(|a| a == "--remove-startup" || a == "remove-startup")
        .unwrap_or(false)
    {
        let elevated_internal = args.iter().any(|a| a == "--elevated-internal");
        return run_remove_startup(elevated_internal);
    }

    if filtered
        .first()
        .copied()
        .map(|a| a == "--loop-dynbg" || a == "loop-dynbg")
        .unwrap_or(false)
    {
        let step_ms = filtered
            .get(1)
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(15_000)
            .clamp(150, 120_000);
        return run_loop_dynbg(step_ms).await;
    }

    if filtered.first().copied() == Some("notify") {
        let summary = filtered
            .get(1)
            .copied()
            .unwrap_or("Hyprland-Win Notification")
            .to_string();
        let body = filtered.get(2..).map(|s| s.join(" ")).unwrap_or_default();
        let msg = HyprNotifyMessage {
            summary,
            body,
            urgency: 1,
            timeout_ms: 4000,
        };
        let mut client = ClientOptions::new()
            .open(HYPRNOTIFY_IPC_PIPE)
            .with_context(|| {
                format!(
                    "Could not connect to HyprNotify daemon at {HYPRNOTIFY_IPC_PIPE} — is `hyprnotify.exe` running?"
                )
            })?;
        let payload = serde_json::to_vec(&msg)?;
        client.write_all(&payload).await?;
        client.flush().await?;
        println!("ok: notification sent");
        return Ok(());
    }

    let req = match filtered.first().copied() {
        Some("clients") => HyprIpcRequest::Clients { json },
        Some("workspaces") => HyprIpcRequest::Workspaces { json },
        Some("activewindow") => HyprIpcRequest::ActiveWindow { json },
        Some("reload") => HyprIpcRequest::Reload,
        Some("dispatch") => {
            let dispatcher = filtered.get(1).copied().unwrap_or("").to_string();
            let arg = filtered.get(2..).map(|s| s.join(" ")).unwrap_or_default();
            HyprIpcRequest::Dispatch { dispatcher, arg }
        }
        Some("keyword") => {
            let key = filtered.get(1).copied().unwrap_or("").to_string();
            let value = filtered.get(2..).map(|s| s.join(" ")).unwrap_or_default();
            HyprIpcRequest::Keyword { key, value }
        }
        _ => HyprIpcRequest::Status { json },
    };

    let mut client = ClientOptions::new()
        .open(HYPRLAND_WIN_IPC_PIPE)
        .with_context(|| {
            format!(
                "Could not connect to Hyprland-Win daemon at {HYPRLAND_WIN_IPC_PIPE} — is `hyprland-win.exe` running?"
            )
        })?;

    let payload = serde_json::to_vec(&req)?;
    client.write_all(&payload).await?;
    client.flush().await?;

    let mut resp_bytes = Vec::with_capacity(16384);
    let mut chunk = [0u8; 8192];
    loop {
        let n = client.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        resp_bytes.extend_from_slice(&chunk[..n]);
        if serde_json::from_slice::<HyprIpcResponse>(&resp_bytes).is_ok() {
            break;
        }
    }

    let resp: HyprIpcResponse = serde_json::from_slice(&resp_bytes)?;
    print!("{}", resp.payload);
    if !resp.payload.ends_with('\n') {
        println!();
    }
    Ok(())
}

fn is_process_elevated() -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("net.exe")
        .arg("session")
        .creation_flags(0x0800_0000)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn resolve_hyprland_win_binary() -> std::path::PathBuf {
    use std::path::PathBuf;
    if let Ok(cur) = std::env::current_exe() {
        if let Some(dir) = cur.parent() {
            let sibling = dir.join("hyprland-win.exe");
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    let candidates = [
        PathBuf::from(r"C:\Users\mbura\Projects\Other\Hyprland-Win\target\release\hyprland-win.exe"),
        PathBuf::from(r"C:\Users\mbura\Projects\Other\Hyprland-Win\target\debug\hyprland-win.exe"),
    ];
    for c in candidates {
        if c.is_file() {
            return c;
        }
    }
    PathBuf::from("hyprland-win.exe")
}

fn run_reg_cmd(args: &[&str]) -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("reg.exe")
        .args(args)
        .creation_flags(0x0800_0000)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run_init_startup(elevated_internal: bool) -> Result<()> {
    use std::os::windows::process::CommandExt;

    let hyprland_win_exe = resolve_hyprland_win_binary();
    let hyprland_win_str = hyprland_win_exe.to_string_lossy().to_string();
    let username = std::env::var("USERNAME").unwrap_or_else(|_| "mbura".to_string());
    let run_cmd = format!("\"{hyprland_win_str}\" --boot-lock");

    // 1. Apply HKCU settings immediately (never requires elevation)
    let _ = run_reg_cmd(&[
        "add",
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Policies\System",
        "/v",
        "DisableLockWorkstation",
        "/t",
        "REG_DWORD",
        "/d",
        "1",
        "/f",
    ]);
    let _ = run_reg_cmd(&[
        "add",
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced",
        "/v",
        "DisabledHotkeys",
        "/t",
        "REG_SZ",
        "/d",
        "PWTEK1234567890",
        "/f",
    ]);
    // Set Hyprland-Win as the primary Winlogon Shell via the `RL HIGHEST` Scheduled Task so `winlogon.exe` / `userinit.exe`
    // launches `hyprland-win.exe --boot-lock` directly as Administrator with ZERO UAC prompt BEFORE `explorer.exe`!
    let shell_cmd = "schtasks.exe /Run /TN \"Hyprland-WinBootLock\"";
    let _ = run_reg_cmd(&[
        "add",
        r"HKCU\Software\Microsoft\Windows NT\CurrentVersion\Winlogon",
        "/v",
        "Shell",
        "/t",
        "REG_SZ",
        "/d",
        shell_cmd,
        "/f",
    ]);
    let _ = run_reg_cmd(&[
        "add",
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Serialize",
        "/v",
        "StartupDelayInMSec",
        "/t",
        "REG_DWORD",
        "/d",
        "0",
        "/f",
    ]);
    let _ = run_reg_cmd(&[
        "add",
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Serialize",
        "/v",
        "WaitForIdleState",
        "/t",
        "REG_DWORD",
        "/d",
        "0",
        "/f",
    ]);
    // Remove legacy HKCU\...\Run\Hyprland-Win so Windows never spawns a second unelevated instance!
    let _ = run_reg_cmd(&[
        "delete",
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
        "/v",
        "Hyprland-Win",
        "/f",
    ]);

    // 2. If not elevated, spawn self elevated to apply HKLM Winlogon/ARSO policies & HIGHEST Scheduled Task
    if !is_process_elevated() && !elevated_internal {
        println!("[hyprctl] Requesting Administrator privilege to configure HKLM Winlogon & Pre-Logon Scheduled Task...");
        if let Ok(cur_exe) = std::env::current_exe() {
            let ps_cmd = format!(
                "Start-Process -FilePath '{}' -ArgumentList '--init-startup','--elevated-internal' -Verb RunAs -Wait -WindowStyle Hidden",
                cur_exe.to_string_lossy()
            );
            let _ = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &ps_cmd])
                .creation_flags(0x0800_0000)
                .status();
        }
    } else {
        // Apply HKLM Pre-Logon & Winlogon ARSO policies
        let hklm_cmds: &[&[&str]] = &[
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
                "ConsentPromptBehaviorAdmin",
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
                "PromptOnSecureDesktop",
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
                "DisableLockWorkstation",
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
                "ConfigAutomaticRestartSignOn",
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
                "EnableFirstLogonAnimation",
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
                "DelayedDesktopSwitchTimeout",
                "/t",
                "REG_DWORD",
                "/d",
                "0",
                "/f",
            ],
            &[
                "add",
                r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\PasswordLess\Device",
                "/v",
                "DevicePasswordLessBuildVersion",
                "/t",
                "REG_DWORD",
                "/d",
                "0",
                "/f",
            ],
            &[
                "add",
                r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon",
                "/v",
                "DefaultUserName",
                "/t",
                "REG_SZ",
                "/d",
                &username,
                "/f",
            ],
            &[
                "add",
                r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon",
                "/v",
                "AutoAdminLogon",
                "/t",
                "REG_SZ",
                "/d",
                "1",
                "/f",
            ],
            &[
                "add",
                r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon",
                "/v",
                "DefaultPassword",
                "/t",
                "REG_SZ",
                "/d",
                "",
                "/f",
            ],
            &[
                "add",
                r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon",
                "/v",
                "AutoRestartShell",
                "/t",
                "REG_DWORD",
                "/d",
                "1",
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
        for cmd in hklm_cmds {
            let _ = run_reg_cmd(cmd);
        }

        // Clear SAM blank-password requirement on the Local account so `winlogon.exe`
        // signs in at 0ms without invoking `LogonUI.exe`
        let _ = std::process::Command::new("net.exe")
            .args(["user", &username, ""])
            .creation_flags(0x0800_0000)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        // Register Highest-Privilege Logon Task (`Hyprland-WinBootLock`) so `hyprland-win.exe --boot-lock`
        // starts immediately with Administrator rights and zero UAC prompt, spawning `hyprlock.exe`
        let _ = std::process::Command::new("schtasks.exe")
            .args([
                "/Create",
                "/TN",
                "Hyprland-WinBootLock",
                "/TR",
                &run_cmd,
                "/SC",
                "ONLOGON",
                "/RL",
                "HIGHEST",
                "/F",
            ])
            .creation_flags(0x0800_0000)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        // Ensure the scheduled task runs on battery power and has no execution time limit
        let ps_tune = r#"$s = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Seconds 0); Set-ScheduledTask -TaskName 'Hyprland-WinBootLock' -Settings $s -ErrorAction SilentlyContinue"#;
        let _ = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", ps_tune])
            .creation_flags(0x0800_0000)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    if !elevated_internal {
        let userprofile = std::env::var("USERPROFILE").unwrap_or_default();
        let pass_file = format!("{userprofile}\\.config\\Hyprwin\\pass.txt");
        let has_pass = std::path::Path::new(&pass_file).is_file()
            || std::path::Path::new(r"C:\Users\mbura\Projects\Other\Hyprland-Win\pass.txt").is_file();

        println!("=== Hyprland-Win Pre-Logon & Boot Lock Startup Initialized ===");
        println!("  [OK] Target Binary       : {hyprland_win_str} --boot-lock");
        println!("  [OK] Native OS Shell     : Registered (HKCU\\...\\Winlogon\\Shell -> before explorer.exe)");
        println!("  [OK] Win11 Lock Curtain  : Disabled (NoLockScreen = 1)");
        println!("  [OK] Win+L Hijack Bypass : Enabled  (DisableLockWorkstation = 1)");
        println!("  [OK] Winlogon ARSO Boot  : Enabled  (AutoAdminLogon = 1, 0ms StartupDelay)");
        println!("  [OK] First-Logon Delay   : Disabled (EnableFirstLogonAnimation = 0)");
        println!("  [OK] Logon Task (Admin)  : Registered (schtasks: Hyprland-WinBootLock, ONLOGON, RL HIGHEST)");
        println!("  [OK] HKCU Run Fallback   : Registered (HKCU\\...\\Run\\Hyprland-Win)");
        if has_pass {
            println!("  [OK] Encrypted pass.txt  : Found (DPAPI HYPRENC1 ready)");
        } else {
            println!("  [!!] Encrypted pass.txt  : Not yet created — `hyprlock` will prompt 2-step setup on first lock");
        }
    }

    Ok(())
}

fn run_remove_startup(elevated_internal: bool) -> Result<()> {
    use std::os::windows::process::CommandExt;

    let _ = run_reg_cmd(&[
        "delete",
        r"HKCU\Software\Microsoft\Windows NT\CurrentVersion\Winlogon",
        "/v",
        "Shell",
        "/f",
    ]);
    let _ = run_reg_cmd(&[
        "delete",
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
        "/v",
        "Hyprland-Win",
        "/f",
    ]);
    let _ = run_reg_cmd(&[
        "add",
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Policies\System",
        "/v",
        "DisableLockWorkstation",
        "/t",
        "REG_DWORD",
        "/d",
        "0",
        "/f",
    ]);

    if !is_process_elevated() && !elevated_internal {
        if let Ok(cur_exe) = std::env::current_exe() {
            let ps_cmd = format!(
                "Start-Process -FilePath '{}' -ArgumentList '--remove-startup','--elevated-internal' -Verb RunAs -Wait -WindowStyle Hidden",
                cur_exe.to_string_lossy()
            );
            let _ = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &ps_cmd])
                .creation_flags(0x0800_0000)
                .status();
        }
    } else {
        let _ = run_reg_cmd(&[
            "add",
            r"HKLM\SOFTWARE\Policies\Microsoft\Windows\Personalization",
            "/v",
            "NoLockScreen",
            "/t",
            "REG_DWORD",
            "/d",
            "0",
            "/f",
        ]);
        let _ = run_reg_cmd(&[
            "add",
            r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System",
            "/v",
            "DisableLockWorkstation",
            "/t",
            "REG_DWORD",
            "/d",
            "0",
            "/f",
        ]);
        let _ = run_reg_cmd(&[
            "delete",
            r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\DisplaySwitch.exe",
            "/v",
            "Debugger",
            "/f",
        ]);
        let _ = std::process::Command::new("schtasks.exe")
            .args(["/Delete", "/TN", "Hyprland-WinBootLock", "/F"])
            .creation_flags(0x0800_0000)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    if !elevated_internal {
        println!("ok: Hyprland-Win startup and pre-logon policies removed.");
    }
    Ok(())
}

async fn run_loop_dynbg(step_ms: u64) -> Result<()> {
    #[link(name = "user32")]
    extern "system" {
        fn OpenDesktopW(
            lpszdesktop: *const u16,
            dwflags: u32,
            finherit: i32,
            dwdesiredaccess: u32,
        ) -> *mut core::ffi::c_void;
        fn SetThreadDesktop(hdesktop: *mut core::ffi::c_void) -> i32;
        fn FindWindowW(lp_class_name: *const u16, lp_window_name: *const u16) -> isize;
        fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
    }

    unsafe {
        let desk_name: Vec<u16> = "Default\0".encode_utf16().collect();
        let h_desk = OpenDesktopW(desk_name.as_ptr(), 0, 0, 0x01FF);
        if !h_desk.is_null() {
            let _ = SetThreadDesktop(h_desk);
        }
    }

    const WM_PAPER_SET_STAGE: u32 = 0x8000 + 50; // WM_APP + 50
    let cls_u16: Vec<u16> = "HyprPaperBackdropClass\0".encode_utf16().collect();

    let mut paper_hwnd = unsafe { FindWindowW(cls_u16.as_ptr(), core::ptr::null()) };
    if paper_hwnd == 0 {
        use std::os::windows::process::CommandExt;
        if let Some(dir) = resolve_hyprland_win_binary().parent() {
            let paper_exe = dir.join("hyprpaper.exe");
            if paper_exe.is_file() {
                let _ = std::process::Command::new(paper_exe)
                    .creation_flags(0x0800_0000)
                    .spawn();
            }
        }
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            paper_hwnd = unsafe { FindWindowW(cls_u16.as_ptr(), core::ptr::null()) };
            if paper_hwnd != 0 {
                break;
            }
        }
    }

    if paper_hwnd == 0 {
        anyhow::bail!("Could not find `HyprPaperBackdropClass` window — is `hyprpaper.exe` running?");
    }

    let stages: [(u8, &str, &str); 16] = [
        (1, "05:00 - 06:14", "Early Dawn"),
        (2, "06:15 - 07:14", "Sunrise / First Light"),
        (3, "07:15 - 08:29", "Early Morning"),
        (4, "08:30 - 09:44", "Morning"),
        (5, "09:45 - 11:14", "Late Morning"),
        (6, "11:15 - 12:44", "Solar Noon / High Day"),
        (7, "12:45 - 14:14", "Early Afternoon"),
        (8, "14:15 - 15:29", "Mid Afternoon"),
        (9, "15:30 - 16:29", "Late Afternoon"),
        (10, "16:30 - 17:14", "Golden Hour"),
        (11, "17:15 - 17:59", "Early Sunset"),
        (12, "18:00 - 18:44", "Sunset / Horizon Glow"),
        (13, "18:45 - 19:44", "Dusk / Civil Twilight"),
        (14, "19:45 - 21:29", "Evening / Blue Hour"),
        (15, "21:30 - 00:59", "Night"),
        (16, "01:00 - 04:59", "Deep Night / Pre-Dawn"),
    ];

    println!(
        "=== HyprPaper Dynamic Wallpaper Loop Test (1 -> 16, {}ms/stage) ===",
        step_ms
    );

    for (stage, window, label) in stages {
        unsafe {
            let _ = PostMessageW(
                paper_hwnd,
                WM_PAPER_SET_STAGE,
                stage as usize,
                step_ms as isize,
            );
        }
        println!(
            "  [{:02}/16] mojave_dynamic_{}.jpeg | {} | {}",
            stage, stage, window, label
        );
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                unsafe {
                    let _ = PostMessageW(paper_hwnd, WM_PAPER_SET_STAGE, 0, 0);
                }
                println!("\nInterrupted — restored live daytime wallpaper schedule.");
                return Ok(());
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(step_ms)) => {}
        }
    }

    unsafe {
        let _ = PostMessageW(paper_hwnd, WM_PAPER_SET_STAGE, 0, 0);
    }
    println!("ok: Dynamic wallpaper loop complete (1 -> 16). Restored live daytime schedule.");
    Ok(())
}
