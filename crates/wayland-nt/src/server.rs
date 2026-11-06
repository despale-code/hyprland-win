//! Asynchronous Windows Named Pipe Wayland Display Server (`\\.\pipe\wayland-0`).
//!
//! Manages Wayland client connections, the global registry (`wl_compositor`,
//! `wl_shm`, `xdg_wm_base`, `zwlr_layer_shell_v1`, `wp_nt_dxgi_buffer_v1`,
//! `wl_seat`, `wl_output`), surface state commits, and damage tracking.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{PipeMode, ServerOptions};
use tokio::sync::{mpsc, RwLock};

use crate::protocol::{
    interfaces, LayerShellLayer, LayerSurfaceAnchor, WlMessageBuilder, WlMessageHeader,
    WlMessageParser, WlShmFormat, WL_DISPLAY_OBJECT_ID,
};

/// Damage rectangle reported by `wl_surface::damage` or `wl_surface::damage_buffer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DamageRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Backing pixel buffer attached to a `wl_surface`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SurfaceBuffer {
    /// CPU shared memory buffer (`wl_shm` via NT `CreateFileMappingW`).
    Shm {
        pool_id: u32,
        offset: u32,
        width: u32,
        height: u32,
        stride: u32,
        format: WlShmFormat,
    },
    /// Zero-copy GPU texture (`wp_nt_dxgi_buffer_v1` or `Win32Wayland` `DwmGetDxSharedSurface`).
    DxgiShared {
        shared_handle: u64,
        width: u32,
        height: u32,
        format: u32,
    },
}

/// Role assigned to a `wl_surface` (`xdg_toplevel` window vs `zwlr_layer_shell_v1` panel/wallpaper).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SurfaceRole {
    None,
    XdgToplevel {
        title: String,
        app_id: String,
        fullscreen: bool,
        maximized: bool,
        floating: bool,
    },
    LayerSurface {
        namespace: String,
        layer: LayerShellLayer,
        anchor: LayerSurfaceAnchor,
        exclusive_zone: i32,
        desired_width: u32,
        desired_height: u32,
    },
}

/// Committed state of a Wayland surface inside the compositor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WlSurfaceState {
    pub client_id: u64,
    pub surface_id: u32,
    pub role: SurfaceRole,
    pub buffer: Option<SurfaceBuffer>,
    pub damage: Vec<DamageRect>,
    pub buffer_scale: i32,
    pub frame_callback_id: Option<u32>,
}

/// High-level event emitted by the Wayland server into the Hyprland-Win compositor loop.
#[derive(Debug, Clone)]
pub enum WaylandCompositorEvent {
    ClientConnected { client_id: u64 },
    ClientDisconnected { client_id: u64 },
    SurfaceCommitted(WlSurfaceState),
    SurfaceDestroyed { client_id: u64, surface_id: u32 },
}

/// Global object advertised via `wl_registry::global`.
#[derive(Debug, Clone)]
pub struct WlGlobal {
    pub name: u32,
    pub interface: &'static str,
    pub version: u32,
}

/// Default list of Wayland globals advertised to every connecting client.
pub fn default_globals() -> Vec<WlGlobal> {
    vec![
        WlGlobal { name: 1, interface: interfaces::WL_COMPOSITOR, version: 6 },
        WlGlobal { name: 2, interface: interfaces::WL_SHM, version: 1 },
        WlGlobal { name: 3, interface: interfaces::WL_OUTPUT, version: 4 },
        WlGlobal { name: 4, interface: interfaces::WL_SEAT, version: 9 },
        WlGlobal { name: 5, interface: interfaces::XDG_WM_BASE, version: 6 },
        WlGlobal { name: 6, interface: interfaces::ZWLR_LAYER_SHELL_V1, version: 4 },
        WlGlobal { name: 7, interface: interfaces::WP_NT_DXGI_BUFFER_V1, version: 1 },
    ]
}

/// Windows Named Pipe Wayland Display Server (`\\.\pipe\wayland-0`).
pub struct WaylandNtServer {
    pipe_name: String,
    event_tx: mpsc::UnboundedSender<WaylandCompositorEvent>,
    surfaces: Arc<RwLock<HashMap<(u64, u32), WlSurfaceState>>>,
}

impl WaylandNtServer {
    pub fn new(
        display_name: &str,
        event_tx: mpsc::UnboundedSender<WaylandCompositorEvent>,
    ) -> Self {
        let pipe_name = if display_name.starts_with(r"\\.\pipe\") {
            display_name.to_string()
        } else {
            format!(r"\\.\pipe\{display_name}")
        };
        Self {
            pipe_name,
            event_tx,
            surfaces: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }

    /// Spawn the asynchronous Wayland Named Pipe listener loop.
    pub async fn run(&self) -> Result<()> {
        tracing::info!("Wayland-NT Display Server listening on {}", self.pipe_name);
        let mut next_client_id: u64 = 1;
        let mut is_first = true;

        loop {
            let server = ServerOptions::new()
                .first_pipe_instance(is_first)
                .pipe_mode(PipeMode::Byte)
                .in_buffer_size(65536)
                .out_buffer_size(65536)
                .create(&self.pipe_name)?;
            is_first = false;

            server.connect().await?;
            let client_id = next_client_id;
            next_client_id += 1;

            let tx = self.event_tx.clone();
            let surfaces = Arc::clone(&self.surfaces);

            tokio::spawn(async move {
                let _ = tx.send(WaylandCompositorEvent::ClientConnected { client_id });
                if let Err(e) = handle_client_connection(client_id, server, tx.clone(), surfaces).await {
                    tracing::debug!("Wayland client {} disconnected: {}", client_id, e);
                }
                let _ = tx.send(WaylandCompositorEvent::ClientDisconnected { client_id });
            });
        }
    }
}

async fn handle_client_connection(
    client_id: u64,
    mut pipe: tokio::net::windows::named_pipe::NamedPipeServer,
    event_tx: mpsc::UnboundedSender<WaylandCompositorEvent>,
    surfaces: Arc<RwLock<HashMap<(u64, u32), WlSurfaceState>>>,
) -> Result<()> {
    let mut object_interfaces: HashMap<u32, String> = HashMap::new();
    object_interfaces.insert(WL_DISPLAY_OBJECT_ID, interfaces::WL_DISPLAY.to_string());

    let mut header_buf = [0u8; WlMessageHeader::SIZE];

    loop {
        pipe.read_exact(&mut header_buf).await?;
        let hdr = WlMessageHeader::from_bytes(&header_buf)?;
        let payload_len = (hdr.size as usize).saturating_sub(WlMessageHeader::SIZE);
        let mut payload = vec![0u8; payload_len];
        if payload_len > 0 {
            pipe.read_exact(&mut payload).await?;
        }

        let iface = object_interfaces
            .get(&hdr.sender_id)
            .map(|s| s.as_str())
            .unwrap_or("");

        match (iface, hdr.opcode) {
            // wl_display::sync(callback: new_id)
            (interfaces::WL_DISPLAY, 0) => {
                let mut p = WlMessageParser::new(&payload);
                let callback_id = p.read_u32()?;
                // Immediately reply with wl_callback::done(serial=1) + wl_display::delete_id(callback_id)
                let mut done = WlMessageBuilder::new(callback_id, 0);
                done.put_u32(1);
                pipe.write_all(&done.finish()).await?;

                let mut del = WlMessageBuilder::new(WL_DISPLAY_OBJECT_ID, 1);
                del.put_u32(callback_id);
                pipe.write_all(&del.finish()).await?;
            }
            // wl_display::get_registry(registry: new_id)
            (interfaces::WL_DISPLAY, 1) => {
                let mut p = WlMessageParser::new(&payload);
                let registry_id = p.read_u32()?;
                object_interfaces.insert(registry_id, interfaces::WL_REGISTRY.to_string());

                // Emit wl_registry::global(name, interface, version) for all supported globals
                for g in default_globals() {
                    let mut evt = WlMessageBuilder::new(registry_id, 0);
                    evt.put_u32(g.name).put_string(g.interface).put_u32(g.version);
                    pipe.write_all(&evt.finish()).await?;
                }
            }
            // wl_registry::bind(name: uint, interface: string, version: uint, id: new_id)
            (interfaces::WL_REGISTRY, 0) => {
                let mut p = WlMessageParser::new(&payload);
                let _name = p.read_u32()?;
                let bound_iface = p.read_string()?;
                let _version = p.read_u32()?;
                let new_id = p.read_u32()?;
                object_interfaces.insert(new_id, bound_iface);
            }
            // wl_compositor::create_surface(id: new_id)
            (interfaces::WL_COMPOSITOR, 0) => {
                let mut p = WlMessageParser::new(&payload);
                let surface_id = p.read_u32()?;
                object_interfaces.insert(surface_id, interfaces::WL_SURFACE.to_string());
                surfaces.write().await.insert(
                    (client_id, surface_id),
                    WlSurfaceState {
                        client_id,
                        surface_id,
                        role: SurfaceRole::None,
                        buffer: None,
                        damage: Vec::new(),
                        buffer_scale: 1,
                        frame_callback_id: None,
                    },
                );
            }
            // wl_surface::commit()
            (interfaces::WL_SURFACE, 6) => {
                if let Some(state) = surfaces.read().await.get(&(client_id, hdr.sender_id)).cloned() {
                    let _ = event_tx.send(WaylandCompositorEvent::SurfaceCommitted(state));
                }
            }
            _ => {
                tracing::trace!(
                    "Wayland client {} msg: obj={} ({}) opcode={} len={}",
                    client_id,
                    hdr.sender_id,
                    iface,
                    hdr.opcode,
                    payload_len
                );
            }
        }
    }
}
