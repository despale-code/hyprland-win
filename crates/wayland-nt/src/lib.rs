//! `wayland-nt` — Native Windows NT port of `libwayland` (`wayland-server` & `wayland-client`).
//!
//! Maps the 4 core Linux Wayland OS primitives 1-to-1 onto Windows NT:
//! 1. `AF_UNIX` wire socket -> Windows Named Pipes (`\\.\pipe\wayland-0`)
//! 2. `epoll` event loop -> Windows I/O Completion Ports (IOCP via Tokio)
//! 3. `SCM_RIGHTS` file-descriptor passing -> `DuplicateHandle` across process boundaries
//! 4. `memfd` (`wl_shm`) & `dma-buf` -> NT Section Objects (`CreateFileMappingW`) & DXGI Shared Handles

pub mod handle_pass;
pub mod protocol;
pub mod server;
pub mod shm;

pub use handle_pass::{get_pipe_client_pid, import_handle_from_client};
pub use protocol::{
    interfaces, LayerShellLayer, LayerSurfaceAnchor, WlFixed, WlMessageBuilder, WlMessageHeader,
    WlMessageParser, WlShmFormat, XdgToplevelState, WL_DISPLAY_OBJECT_ID,
};
pub use server::{
    default_globals, DamageRect, SurfaceBuffer, SurfaceRole, WaylandCompositorEvent,
    WaylandNtServer, WlGlobal, WlSurfaceState,
};
pub use shm::NtShmPool;
