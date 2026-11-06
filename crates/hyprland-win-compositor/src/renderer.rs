//! Direct3D 11 Hardware Renderer, DirectComposition Click-Through Border/Corner Overlay,
//! and Zero-Copy Shared Window Texture Importer.
//!
//! - Binds `DwmGetDxSharedSurface` GPU `HANDLE`s directly as `ID3D11Texture2D` /
//!   `ID3D11ShaderResourceView` via `ID3D11Device::OpenSharedResource` (0 CPU copies).
//! - Captures `WS_EX_NOREDIRECTIONBITMAP` DirectComposition visual trees (`WindowsTerminal.exe`,
//!   `Antigravity.exe` / Electron / WinUI 3) into a GPU `ID3D11Texture2D` via
//!   `IDXGISurface1::GetDC` + `PrintWindow(PW_RENDERFULLCONTENT)`.
//! - Creates a full-screen click-through DirectComposition (`IDCompositionDevice` +
//!   `IDXGISwapChain1` with `DXGI_ALPHA_MODE_PREMULTIPLIED`) hardware overlay that renders
//!   Hyprland's signature SDF Rounded-Corner Animated Gradient Borders (`ps_border_overlay`)
//!   with 0.0ms input latency and automatic Direct Scanout bypass for fullscreen games.

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use windows::core::{s, w, Interface};
use windows::Win32::Foundation::{BOOL, COLORREF, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
    D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11BlendState, ID3D11Buffer, ID3D11Device, ID3D11DeviceContext,
    ID3D11PixelShader, ID3D11RasterizerState, ID3D11RenderTargetView, ID3D11SamplerState,
    ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader, D3D11_BIND_CONSTANT_BUFFER,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BLEND_DESC,
    D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD, D3D11_BUFFER_DESC,
    D3D11_COLOR_WRITE_ENABLE_ALL, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CULL_NONE,
    D3D11_FILL_SOLID, D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_RASTERIZER_DESC,
    D3D11_RENDER_TARGET_BLEND_DESC, D3D11_RESOURCE_MISC_GDI_COMPATIBLE, D3D11_SAMPLER_DESC,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_USAGE_DEFAULT,
    D3D11_VIEWPORT,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIAdapter, IDXGIDevice, IDXGIDevice1, IDXGIFactory2, IDXGISurface1, IDXGISwapChain1,
    DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetWindow, PeekMessageW, RegisterClassW,
    SetLayeredWindowAttributes, SetWindowPos, ShowWindow, TranslateMessage, GW_HWNDNEXT,
    GW_HWNDPREV, HWND_BOTTOM, LWA_ALPHA, MSG, PM_REMOVE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SW_SHOWNOACTIVATE, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP,
};

use hyprland_win_core::{HyprConfig, WorkspaceScene};

const SHADER_SOURCE: &str = include_str!("shaders.hlsl");
/// `PW_RENDERFULLCONTENT` (`0x00000002`) instructs `dwmcore.dll` to composite
/// DirectComposition (`WS_EX_NOREDIRECTIONBITMAP`) visual trees directly into the target surface.
const PW_RENDERFULLCONTENT: u32 = 0x0000_0002;

/// GPU Constant Buffer matching `cbuffer WindowUniforms : register(b0)` in `shaders.hlsl`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct WindowUniforms {
    pub rect_px: [f32; 4],        // (x, y, w, h)
    pub screen_info: [f32; 4],    // (screen_w, screen_h, rounding_px, border_size_px)
    pub border_color_a: [f32; 4], // Primary RGBA
    pub border_color_b: [f32; 4], // Secondary RGBA
    pub extra_params: [f32; 4],   // (angle_rad, opacity, is_active, 0.0)
}

pub struct D3d11CompositorRenderer {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub vertex_shader: ID3D11VertexShader,
    pub pixel_shader: ID3D11PixelShader,
    pub border_overlay_ps: ID3D11PixelShader,
    pub uniform_buffer: ID3D11Buffer,
    pub linear_sampler: ID3D11SamplerState,
    pub premul_blend: ID3D11BlendState,
    pub rasterizer_state: ID3D11RasterizerState,
}

/// Hardware-accelerated DirectComposition click-through overlay surface (`WS_EX_TRANSPARENT | WS_EX_NOREDIRECTIONBITMAP`).
pub struct DCompOverlaySurface {
    pub hwnd: HWND,
    pub width: i32,
    pub height: i32,
    pub swapchain: IDXGISwapChain1,
    pub rtv: ID3D11RenderTargetView,
    pub wallpaper_tex: ID3D11Texture2D,
    pub wallpaper_srv: ID3D11ShaderResourceView,
    pub last_wallpaper_update_id: AtomicU64,
    pub last_frame_sig: AtomicU64,
    pub _dcomp_device: IDCompositionDevice,
    pub _dcomp_target: IDCompositionTarget,
    pub _dcomp_visual: IDCompositionVisual,
}
unsafe impl Send for DCompOverlaySurface {}
unsafe impl Sync for DCompOverlaySurface {}

impl D3d11CompositorRenderer {
    /// Initialize the Direct3D 11 hardware device with `MaximumFrameLatency = 1`
    /// and compile the Hyprland-Win SDF Rounded Corner + Gradient Border HLSL shaders.
    pub fn new() -> Result<Self> {
        unsafe {
            let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
            let mut device: Option<ID3D11Device> = None;
            let mut context: Option<ID3D11DeviceContext> = None;

            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .context("D3D11CreateDevice failed on primary GPU")?;

            let device = device.context("D3D11Device is null")?;
            let context = context.context("D3D11DeviceContext is null")?;

            // Set DXGI MaximumFrameLatency = 1 for ultra-low compositor latency
            if let Ok(dxgi_dev) = device.cast::<IDXGIDevice1>() {
                let _ = dxgi_dev.SetMaximumFrameLatency(1);
            }

            let vs_blob = compile_hlsl(SHADER_SOURCE, "vs_main", "vs_5_0")?;
            let ps_blob = compile_hlsl(SHADER_SOURCE, "ps_main", "ps_5_0")?;
            let border_ps_blob = compile_hlsl(SHADER_SOURCE, "ps_border_overlay", "ps_5_0")?;

            let mut vertex_shader: Option<ID3D11VertexShader> = None;
            device
                .CreateVertexShader(&vs_blob, None, Some(&mut vertex_shader))
                .context("CreateVertexShader failed")?;

            let mut pixel_shader: Option<ID3D11PixelShader> = None;
            device
                .CreatePixelShader(&ps_blob, None, Some(&mut pixel_shader))
                .context("CreatePixelShader failed")?;

            let mut border_overlay_ps: Option<ID3D11PixelShader> = None;
            device
                .CreatePixelShader(&border_ps_blob, None, Some(&mut border_overlay_ps))
                .context("CreatePixelShader(ps_border_overlay) failed")?;

            let cb_desc = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of::<WindowUniforms>() as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
                StructureByteStride: 0,
            };
            let mut uniform_buffer: Option<ID3D11Buffer> = None;
            device
                .CreateBuffer(&cb_desc, None, Some(&mut uniform_buffer))
                .context("CreateBuffer for WindowUniforms failed")?;

            let samp_desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MipLODBias: 0.0,
                MaxAnisotropy: 1,
                ComparisonFunc: Default::default(),
                BorderColor: [0.0; 4],
                MinLOD: 0.0,
                MaxLOD: f32::MAX,
            };
            let mut linear_sampler: Option<ID3D11SamplerState> = None;
            device
                .CreateSamplerState(&samp_desc, Some(&mut linear_sampler))
                .context("CreateSamplerState failed")?;

            let mut blend_desc = D3D11_BLEND_DESC::default();
            blend_desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: BOOL(1),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            let mut premul_blend: Option<ID3D11BlendState> = None;
            device
                .CreateBlendState(&blend_desc, Some(&mut premul_blend))
                .context("CreateBlendState failed")?;

            let rast_desc = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                ..Default::default()
            };
            let mut rasterizer_state: Option<ID3D11RasterizerState> = None;
            device
                .CreateRasterizerState(&rast_desc, Some(&mut rasterizer_state))
                .context("CreateRasterizerState failed")?;

            Ok(Self {
                device,
                context,
                vertex_shader: vertex_shader.unwrap(),
                pixel_shader: pixel_shader.unwrap(),
                border_overlay_ps: border_overlay_ps.unwrap(),
                uniform_buffer: uniform_buffer.unwrap(),
                linear_sampler: linear_sampler.unwrap(),
                premul_blend: premul_blend.unwrap(),
                rasterizer_state: rasterizer_state.unwrap(),
            })
        }
    }

    /// Create a full-screen click-through DirectComposition (`DXGI_ALPHA_MODE_PREMULTIPLIED`)
    /// hardware overlay window for rendering Hyprland SDF rounded gradient borders.
    pub fn create_dcomp_overlay(&self, width: i32, height: i32) -> Result<DCompOverlaySurface> {
        unsafe {
            let hinstance = GetModuleHandleW(None)?;
            let class_name = w!("HyprlandWinDCompOverlayClass");

            let wc = WNDCLASSW {
                lpfnWndProc: Some(overlay_wnd_proc),
                hInstance: hinstance.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);

            // Non-topmost click-through overlay: positioned strictly in front of `hyprpaper`
            // and behind every application window, popup, menu, floating window, and bar!
            let ex_style = WS_EX_TRANSPARENT
                | WS_EX_LAYERED
                | WS_EX_TOOLWINDOW
                | WS_EX_NOACTIVATE
                | WS_EX_NOREDIRECTIONBITMAP;

            let hwnd = CreateWindowExW(
                ex_style,
                class_name,
                w!("Hyprland-Win Border Overlay"),
                WS_POPUP,
                0,
                0,
                width,
                height,
                HWND::default(),
                None,
                hinstance,
                None,
            )
            .context("CreateWindowExW for DirectComposition overlay failed")?;

            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = SetWindowPos(
                hwnd,
                HWND_BOTTOM,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );

            let dxgi_dev: IDXGIDevice = self.device.cast()?;
            let adapter: IDXGIAdapter = dxgi_dev.GetAdapter()?;
            let factory2: IDXGIFactory2 = adapter.GetParent()?;

            let swap_desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width.max(64) as u32,
                Height: height.max(64) as u32,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: BOOL(0),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                Flags: 0,
            };

            let swapchain: IDXGISwapChain1 =
                factory2.CreateSwapChainForComposition(&self.device, &swap_desc, None)?;

            let backbuf: ID3D11Texture2D = swapchain.GetBuffer(0)?;
            let mut rtv: Option<ID3D11RenderTargetView> = None;
            self.device
                .CreateRenderTargetView(&backbuf, None, Some(&mut rtv))
                .context("CreateRenderTargetView on DComp swapchain failed")?;

            let dcomp_device: IDCompositionDevice = DCompositionCreateDevice(&dxgi_dev)
                .context("DCompositionCreateDevice failed")?;
            let dcomp_target = dcomp_device
                .CreateTargetForHwnd(hwnd, true)
                .context("CreateTargetForHwnd failed")?;
            let dcomp_visual = dcomp_device
                .CreateVisual()
                .context("CreateVisual failed")?;

            dcomp_visual.SetContent(&swapchain)?;
            dcomp_target.SetRoot(&dcomp_visual)?;
            dcomp_device.Commit()?;

            let wp_desc = D3D11_TEXTURE2D_DESC {
                Width: width.max(64) as u32,
                Height: height.max(64) as u32,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: D3D11_RESOURCE_MISC_GDI_COMPATIBLE.0 as u32,
            };
            let mut wp_tex: Option<ID3D11Texture2D> = None;
            self.device
                .CreateTexture2D(&wp_desc, None, Some(&mut wp_tex))
                .context("CreateTexture2D for wallpaper corner mask failed")?;
            let wp_tex = wp_tex.unwrap();
            let mut wp_srv: Option<ID3D11ShaderResourceView> = None;
            self.device
                .CreateShaderResourceView(&wp_tex, None, Some(&mut wp_srv))
                .context("CreateShaderResourceView for wallpaper corner mask failed")?;

            Ok(DCompOverlaySurface {
                hwnd,
                width,
                height,
                swapchain,
                rtv: rtv.unwrap(),
                wallpaper_tex: wp_tex,
                wallpaper_srv: wp_srv.unwrap(),
                last_wallpaper_update_id: AtomicU64::new(0),
                last_frame_sig: AtomicU64::new(0),
                _dcomp_device: dcomp_device,
                _dcomp_target: dcomp_target,
                _dcomp_visual: dcomp_visual,
            })
        }
    }

    /// Render a frame of SDF rounded gradient borders onto the DirectComposition click-through
    /// overlay swapchain. Automatically clears to 100% transparent when any window on the active
    /// workspace is in fullscreen / Direct Scanout mode.
    pub fn render_border_overlay_frame(
        &self,
        overlay: &DCompOverlaySurface,
        scene: &WorkspaceScene,
        cfg: &HyprConfig,
        border_angle_rad: f32,
        apply_tiling: bool,
    ) {
        unsafe {
            // Pump any pending Win32 messages on the overlay HWND
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, overlay.hwnd, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            // Position border overlay immediately ABOVE the topmost active-workspace tiled window
            // (and BELOW all floating windows, popup/context menus, `hyprbar`, and `hyprlauncher`),
            // so tiled windows receive true 22px anti-aliased rounded corners while floating windows
            // and popup menus are never covered!
            let paper_cls: Vec<u16> = "HyprPaperBackdropClass\0".encode_utf16().collect();
            let paper_hwnd = windows::Win32::UI::WindowsAndMessaging::FindWindowW(
                windows::core::PCWSTR(paper_cls.as_ptr()),
                windows::core::PCWSTR::null(),
            )
            .unwrap_or_default();

            let active_ws = scene.active_workspace;
            let mut top_tiled_hwnd = HWND::default();
            if !paper_hwnd.0.is_null() {
                // Walk forward from `paper_hwnd` (`GW_HWNDPREV`) to find the highest active-workspace tiled window
                let mut cur = GetWindow(paper_hwnd, GW_HWNDPREV).unwrap_or_default();
                let mut steps = 0;
                while !cur.0.is_null() && steps < 512 {
                    if let Some(c) = scene.clients.get(&(cur.0 as u64)) {
                        if c.workspace_id == active_ws
                            && !c.floating
                            && !c.fullscreen
                            && c.current_rect.x < 10_000.0
                        {
                            top_tiled_hwnd = cur;
                        }
                    }
                    cur = GetWindow(cur, GW_HWNDPREV).unwrap_or_default();
                    steps += 1;
                }

                if !top_tiled_hwnd.0.is_null() {
                    let prev_above_tiled =
                        GetWindow(top_tiled_hwnd, GW_HWNDPREV).unwrap_or_default();
                    if !prev_above_tiled.0.is_null() && prev_above_tiled != overlay.hwnd {
                        let _ = SetWindowPos(
                            overlay.hwnd,
                            prev_above_tiled,
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        );
                    }
                } else {
                    let prev_above_paper = GetWindow(paper_hwnd, GW_HWNDPREV).unwrap_or_default();
                    if !prev_above_paper.0.is_null() && prev_above_paper != overlay.hwnd {
                        let _ = SetWindowPos(
                            overlay.hwnd,
                            prev_above_paper,
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        );
                    }
                }
            }

            let mut paper_update_id: u64 = 0;
            if !paper_hwnd.0.is_null() {
                if let Ok(shared) = dwm_hollow::query_window_shared_texture(paper_hwnd) {
                    paper_update_id = shared.win32k_update_id.max(1);
                } else {
                    paper_update_id = 1;
                }
                if overlay.last_wallpaper_update_id.load(Ordering::Relaxed) != paper_update_id {
                    // Must unbind `wallpaper_srv` from PS slot 0 before calling `IDXGISurface1::GetDC`!
                    self.context.PSSetShaderResources(0, Some(&[None]));
                    self.context.OMSetRenderTargets(None, None);
                    if let Ok(surface1) = overlay.wallpaper_tex.cast::<IDXGISurface1>() {
                        if let Ok(hdc) = surface1.GetDC(BOOL(0)) {
                            let ok = PrintWindow(
                                paper_hwnd,
                                hdc,
                                PRINT_WINDOW_FLAGS(0),
                            )
                            .as_bool();
                            if !ok {
                                let _ = PrintWindow(
                                    paper_hwnd,
                                    hdc,
                                    PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT),
                                );
                            }
                            let _ = surface1.ReleaseDC(None);
                            overlay
                                .last_wallpaper_update_id
                                .store(paper_update_id, Ordering::Relaxed);
                        }
                    }
                }
            }

            let has_fullscreen = scene
                .clients
                .values()
                .any(|c| c.workspace_id == active_ws && c.fullscreen);

            let fg_id =
                windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow().0 as u64;
            let any_scene_focused = scene
                .clients
                .values()
                .any(|c| c.workspace_id == active_ws && c.focused);

            // Compute a deterministic 64-bit signature of the border state so we only flip the
            // DirectComposition swapchain when geometry, focus, or gradient state actually changes!
            let mut hasher = DefaultHasher::new();
            apply_tiling.hash(&mut hasher);
            has_fullscreen.hash(&mut hasher);
            cfg.border_size.hash(&mut hasher);
            (cfg.rounding.to_bits()).hash(&mut hasher);
            for c in &cfg.active_border_rgba {
                c.to_bits().hash(&mut hasher);
            }
            for c in &cfg.active_border_rgba_secondary {
                c.to_bits().hash(&mut hasher);
            }
            for c in &cfg.inactive_border_rgba {
                c.to_bits().hash(&mut hasher);
            }
            (cfg.border_angle_deg.to_bits()).hash(&mut hasher);
            active_ws.hash(&mut hasher);
            paper_update_id.hash(&mut hasher);
            let has_gradient = cfg.active_border_rgba != cfg.active_border_rgba_secondary;
            if has_gradient {
                ((border_angle_rad * 60.0) as i32).hash(&mut hasher);
            }

            let mut sorted_ids: Vec<u64> = scene.clients.keys().copied().collect();
            sorted_ids.sort_unstable();
            for &id in &sorted_ids {
                if let Some(client) = scene.clients.get(&id) {
                    let is_visible_in_transition = (client.workspace_id == active_ws
                        && client.current_rect.x < 10_000.0)
                        || (client.anim_progress < 1.0
                            && client.current_rect.x + client.current_rect.w > 0.0
                            && client.current_rect.x < overlay.width as f32);
                    if !is_visible_in_transition {
                        continue;
                    }
                    let is_client_active = if any_scene_focused {
                        client.focused
                    } else {
                        client.id == fg_id
                    };
                    id.hash(&mut hasher);
                    is_client_active.hash(&mut hasher);
                    client.floating.hash(&mut hasher);
                    (client.current_rect.x.round() as i32).hash(&mut hasher);
                    (client.current_rect.y.round() as i32).hash(&mut hasher);
                    (client.current_rect.w.round() as i32).hash(&mut hasher);
                    (client.current_rect.h.round() as i32).hash(&mut hasher);
                }
            }
            let frame_sig = hasher.finish().max(1);
            if overlay.last_frame_sig.load(Ordering::Relaxed) == frame_sig {
                return;
            }
            overlay.last_frame_sig.store(frame_sig, Ordering::Relaxed);

            // Acquire the current flip-model backbuffer (Buffer 0) and bind its RTV before clearing
            let frame_rtv = if let Ok(backbuf) = overlay.swapchain.GetBuffer::<ID3D11Texture2D>(0) {
                let mut new_rtv: Option<ID3D11RenderTargetView> = None;
                if self
                    .device
                    .CreateRenderTargetView(&backbuf, None, Some(&mut new_rtv))
                    .is_ok()
                {
                    new_rtv.unwrap_or_else(|| overlay.rtv.clone())
                } else {
                    overlay.rtv.clone()
                }
            } else {
                overlay.rtv.clone()
            };

            self.context
                .OMSetRenderTargets(Some(&[Some(frame_rtv.clone())]), None);
            let clear_transparent = [0.0_f32, 0.0, 0.0, 0.0];
            self.context
                .ClearRenderTargetView(&frame_rtv, &clear_transparent);

            if !apply_tiling || has_fullscreen || cfg.border_size <= 0 {
                let _ = overlay.swapchain.Present(0, DXGI_PRESENT(0));
                return;
            }

            let vp = D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: overlay.width as f32,
                Height: overlay.height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            self.context.RSSetViewports(Some(&[vp]));
            self.context.RSSetState(&self.rasterizer_state);
            self.context
                .OMSetBlendState(&self.premul_blend, Some(&[0.0; 4]), 0xFFFF_FFFF);
            self.context
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            self.context.VSSetShader(&self.vertex_shader, None);
            self.context.PSSetShader(&self.border_overlay_ps, None);
            self.context
                .VSSetConstantBuffers(0, Some(&[Some(self.uniform_buffer.clone())]));
            self.context
                .PSSetConstantBuffers(0, Some(&[Some(self.uniform_buffer.clone())]));
            self.context
                .PSSetShaderResources(0, Some(&[Some(overlay.wallpaper_srv.clone())]));
            self.context
                .PSSetSamplers(0, Some(&[Some(self.linear_sampler.clone())]));

            let rounding = cfg.rounding.max(0.0);
            let border_w = (cfg.border_size.max(1)) as f32;

            // Draw inactive window borders first, then active window border on top
            for pass_active in [false, true] {
                for &id in &sorted_ids {
                    let Some(client) = scene.clients.get(&id) else {
                        continue;
                    };
                    let is_visible_in_transition = (client.workspace_id == active_ws
                        && client.current_rect.x < 10_000.0)
                        || (client.anim_progress < 1.0
                            && client.current_rect.x + client.current_rect.w > 0.0
                            && client.current_rect.x < overlay.width as f32);
                    let is_client_active = if any_scene_focused {
                        client.focused
                    } else {
                        client.id == fg_id
                    };
                    if !is_visible_in_transition || is_client_active != pass_active {
                        continue;
                    }

                    let is_borderless_client = client.class_name.to_ascii_lowercase().contains("alacritty")
                        || client.title.to_ascii_lowercase().contains("alacritty");
                    let (col_a, col_b, angle, opacity, is_act) = if is_borderless_client {
                        (
                            [0.0, 0.0, 0.0, 0.0],
                            [0.0, 0.0, 0.0, 0.0],
                            0.0_f32,
                            0.0_f32,
                            0.0_f32,
                        )
                    } else if is_client_active {
                        (
                            cfg.active_border_rgba,
                            cfg.active_border_rgba_secondary,
                            border_angle_rad,
                            1.0_f32,
                            1.0_f32,
                        )
                    } else {
                        (
                            cfg.inactive_border_rgba,
                            cfg.inactive_border_rgba,
                            0.0_f32,
                            0.85_f32,
                            0.0_f32,
                        )
                    };
                    let is_tiled_flag = if client.floating { 0.0_f32 } else { 1.0_f32 };

                    let uniforms = WindowUniforms {
                        rect_px: [
                            client.current_rect.x,
                            client.current_rect.y,
                            client.current_rect.w,
                            client.current_rect.h,
                        ],
                        screen_info: [
                            overlay.width as f32,
                            overlay.height as f32,
                            rounding,
                            border_w,
                        ],
                        border_color_a: col_a,
                        border_color_b: col_b,
                        extra_params: [angle, opacity, is_act, is_tiled_flag],
                    };

                    self.context.UpdateSubresource(
                        &self.uniform_buffer,
                        0,
                        None,
                        &uniforms as *const WindowUniforms as *const _,
                        0,
                        0,
                    );
                    self.context.Draw(4, 0);
                }
            }

            self.context.PSSetShaderResources(0, Some(&[None]));
            let _ = overlay.swapchain.Present(0, DXGI_PRESENT(0));
        }
    }

    /// Zero-copy import of a `DwmGetDxSharedSurface` GPU `HANDLE` into a Direct3D 11
    /// `ID3D11ShaderResourceView` ready for sampling in `ps_main`.
    pub fn open_shared_window_srv(
        &self,
        shared_handle: u64,
    ) -> Result<(ID3D11Texture2D, ID3D11ShaderResourceView)> {
        unsafe {
            let h = HANDLE(shared_handle as *mut core::ffi::c_void);
            let mut tex: Option<ID3D11Texture2D> = None;
            self.device
                .OpenSharedResource(h, &mut tex)
                .with_context(|| format!("OpenSharedResource failed for handle 0x{shared_handle:X}"))?;
            let tex = tex.context("OpenSharedResource returned null texture")?;

            let mut srv: Option<ID3D11ShaderResourceView> = None;
            self.device
                .CreateShaderResourceView(&tex, None, Some(&mut srv))
                .context("CreateShaderResourceView on shared DWM texture failed")?;

            Ok((tex, srv.unwrap()))
        }
    }

    /// Capture a `WS_EX_NOREDIRECTIONBITMAP` DirectComposition visual tree (such as
    /// `WindowsTerminal.exe` or `Antigravity.exe`) directly into a GPU `ID3D11Texture2D`
    /// via `IDXGISurface1::GetDC` + `PrintWindow(PW_RENDERFULLCONTENT)`.
    pub fn capture_dcomp_window_srv(
        &self,
        hwnd: u64,
        width: u32,
        height: u32,
    ) -> Result<(ID3D11Texture2D, ID3D11ShaderResourceView)> {
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width.max(64),
                Height: height.max(64),
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: D3D11_RESOURCE_MISC_GDI_COMPATIBLE.0 as u32,
            };

            let mut tex: Option<ID3D11Texture2D> = None;
            self.device
                .CreateTexture2D(&desc, None, Some(&mut tex))
                .context("CreateTexture2D for DirectComposition capture failed")?;
            let tex = tex.unwrap();

            let surface1: IDXGISurface1 = tex.cast()?;
            let hdc = surface1.GetDC(BOOL(0))?;
            let _ = PrintWindow(
                HWND(hwnd as *mut core::ffi::c_void),
                hdc,
                PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT),
            );
            let _ = surface1.ReleaseDC(None);

            let mut srv: Option<ID3D11ShaderResourceView> = None;
            self.device
                .CreateShaderResourceView(&tex, None, Some(&mut srv))
                .context("CreateShaderResourceView on DComp texture failed")?;

            Ok((tex, srv.unwrap()))
        }
    }
}

fn compile_hlsl(source: &str, entry: &str, target: &str) -> Result<Vec<u8>> {
    unsafe {
        let entry_c = std::ffi::CString::new(entry)?;
        let target_c = std::ffi::CString::new(target)?;
        let mut code_blob = None;
        let mut err_blob = None;

        let hr = D3DCompile(
            source.as_ptr() as *const _,
            source.len(),
            s!("shaders.hlsl"),
            None,
            None,
            windows::core::PCSTR(entry_c.as_ptr() as *const u8),
            windows::core::PCSTR(target_c.as_ptr() as *const u8),
            0,
            0,
            &mut code_blob,
            Some(&mut err_blob),
        );

        if hr.is_err() {
            let msg = if let Some(err) = err_blob {
                let slice = std::slice::from_raw_parts(
                    err.GetBufferPointer() as *const u8,
                    err.GetBufferSize(),
                );
                String::from_utf8_lossy(slice).to_string()
            } else {
                format!("HRESULT {hr:?}")
            };
            anyhow::bail!("HLSL compile error ({entry}): {msg}");
        }

        let blob = code_blob.context("D3DCompile returned null blob")?;
        let slice = std::slice::from_raw_parts(
            blob.GetBufferPointer() as *const u8,
            blob.GetBufferSize(),
        );
        Ok(slice.to_vec())
    }
}

unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
