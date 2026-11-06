# hyprland-win — Wayland-NT & Hyprland Compositor for Windows (Hollowed DWM)

**hyprland-win** decouples Windows' `uDWM.dll` / `explorer.exe` shell and brings the **Wayland wire protocol (`wayland-nt`)** and **Hyprland's dynamic tiling, bezier animation, and SDF/Dual-Kawase shader pipeline** natively to Windows (Windows 7 NT 6.1 through Windows 11 24H2/26300).

---

## Architecture Overview

```text
┌────────────────────────────────────────────────────────────────────────────┐
│                   HYPRLAND & WAYLAND ECOSYSTEM                             │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌────────────────┐  │
│  │  hyprctl CLI │  │ Wayland Apps │  │  hyprland.conf│ │ Win32 / DX12   │  │
│  │ (Named Pipe) │  │ (waybar/rofi)│  │ (Live Reload)│  │ Games & Apps   │  │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘  └────────┬───────┘  │
├─────────┼─────────────────┼─────────────────┼───────────────────┼──────────┤
│         ▼                 ▼                 ▼                   ▼          │
│  ┌───────────────────┐  ┌─────────────────────────┐  ┌──────────────────┐  │
│  │ hyprland-win-core │  │       wayland-nt        │  │    dwm-hollow    │  │
│  │ - Dwindle/        │  │ - \\.\pipe\wayland-0    │  │ - user32!DwmGet  │  │
│  │   Master BSP      │  │ - DuplicateHandle       │  │   DxSharedSurface│  │
│  │ - Bezier          │  │ - CreateFileMapping     │  │ - DWMWA_CLOAK &  │  │
│  │   Animations      │  │ - wp_nt_dxgi_buffer_v1  │  │   uDWM Stripper  │  │
│  │ - IPC Server      │  │ - xdg_shell / layer     │  │ - Direct Scanout │  │
│  └──────┬────────────┘  └────────────┬────────────┘  └─────────┬────────┘  │
│         └────────────────────────────┼─────────────────────────┘           │
│                                      ▼                                     │
│                  ┌──────────────────────────────────┐                      │
│                  │     hyprland-win-compositor      │                      │
│                  │ - Direct3D 11 Hardware Device    │                      │
│                  │ - OpenSharedResource (Zero-Copy) │                      │
│                  │ - HLSL SDF Rounded Corners +     │                      │
│                  │   Rotated Gradient Borders       │                      │
│                  └──────────────────────────────────┘                      │
└────────────────────────────────────────────────────────────────────────────┘
```

---

## Workspace Crates

| Crate | Path | Description |
| :--- | :--- | :--- |
| **`wayland-nt`** | [`crates/wayland-nt`](crates/wayland-nt) | Native Windows NT port of `libwayland`: 8-byte wire framing, `wl_display`, `wl_registry`, `wl_compositor`, `wl_shm` (`CreateFileMappingW`), `DuplicateHandle` (`SCM_RIGHTS` equivalent), `xdg_wm_base`, `zwlr_layer_shell_v1`, and `wp_nt_dxgi_buffer_v1` over `\\.\pipe\wayland-0`. |
| **`dwm-hollow`** | [`crates/dwm-hollow`](crates/dwm-hollow) | Resolves `user32.dll!DwmGetDxSharedSurface` for zero-copy GPU texture handles, strips stock `uDWM.dll` Mica/Acrylic/shadows/borders/transitions, cloaks inactive workspaces (`DWMWA_CLOAK`), and detects fullscreen games (`cs2.exe`, `project8.exe` / Deadlock, `Client-Win64-Shipping.exe` / Wuthering Waves) for hardware **Independent Flip Direct Scanout**. |
| **`hyprland-win-core`** | [`crates/hyprland-win-core`](crates/hyprland-win-core) | Parses standard [`config/hyprland.conf`](config/hyprland.conf), solves parametric Cubic Bezier curves (`bezier = myBezier, 0.05, 0.9, 0.1, 1.05`), computes **Dwindle BSP** and **Master-Stack** layouts across Workspaces `1..=10`, and implements the `hyprctl` IPC protocol (`\\.\pipe\hyprland-win-ipc`). |
| **`hyprland-win-compositor`** | [`crates/hyprland-win-compositor`](crates/hyprland-win-compositor) | Direct3D 11 hardware compositor (`hyprland-win.exe`). Imports `DwmGetDxSharedSurface` handles via `ID3D11Device::OpenSharedResource` with zero CPU copies and compiles [`shaders.hlsl`](crates/hyprland-win-compositor/src/shaders.hlsl) (Signed-Distance-Field rounded corners + angled dual-stop gradient borders). |
| **`hyprctl`** | [`crates/hyprctl`](crates/hyprctl) | CLI controller (`hyprctl.exe`) supporting `clients [-j]`, `workspaces [-j]`, `activewindow [-j]`, `dispatch <cmd> <arg>`, `keyword <key> <val>`, `reload`, and `status`. |
| **`hyprbar`** | [`crates/hyprbar`](crates/hyprbar) | Top status bar and Dynamic Island control center (`hyprbar.exe`). |
| **`hyprlauncher`** | [`crates/hyprlauncher`](crates/hyprlauncher) | Application launcher and clipboard manager (`hyprlauncher.exe`). |
| **`hyprpaper`** | [`crates/hyprpaper`](crates/hyprpaper) | Dynamic & static wallpaper daemon (`hyprpaper.exe`). |
| **`hyprnotify`** | [`crates/hyprnotify`](crates/hyprnotify) | Notification daemon synced with Windows Action Center (`hyprnotify.exe`). |
| **`hyprlock`** | [`crates/hyprlock`](crates/hyprlock) | Lockscreen overlay (`hyprlock.exe`). |
| **`hyprshot`** | [`crates/hyprshot`](crates/hyprshot) | Region and output screenshot utility (`hyprshot.exe`). |

---

## Quick Start (Run in Windows Terminal)

### Automated Setup & Integration (`setup.ps1`)
Run [`setup.ps1`](setup.ps1) in PowerShell to automatically install dependencies, seed `~/.config/hyprwin`, build all release binaries, and register the Windows logon task:
```powershell
powershell -ExecutionPolicy Bypass -File .\setup.ps1
```

### Automated Update (`update.ps1`)
Run [`update.ps1`](update.ps1) at any time to pull the latest changes from GitHub, rebuild in `--release` mode without linker locks, and hot-restart the compositor and companion daemons:
```powershell
powershell -ExecutionPolicy Bypass -File .\update.ps1
```

### 1. Probe Zero-Copy DWM Shared GPU Textures on Your Desktop
```powershell
cd hyprland-win
.\target\release\hyprland-win.exe --probe
```

### 2. Start the `hyprland-win` Compositor Daemon
```powershell
.\target\release\hyprland-win.exe
```

### 3. Control via `hyprctl`
In a second terminal:
```powershell
.\target\release\hyprctl.exe status
.\target\release\hyprctl.exe clients
.\target\release\hyprctl.exe workspaces
.\target\release\hyprctl.exe dispatch tiling
.\target\release\hyprctl.exe dispatch workspace 2
.\target\release\hyprctl.exe keyword general:layout master
.\target\release\hyprctl.exe reload
```

## License

Licensed under the [MIT License](LICENSE).
