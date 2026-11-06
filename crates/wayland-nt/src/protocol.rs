//! Wayland binary wire protocol parser, marshaler, and standard interface definitions
//! adapted for Windows NT (`wayland-nt`).
//!
//! Wire format matches the upstream Freedesktop Wayland specification:
//! - 8-byte header: `[object_id: u32, (size_bytes << 16) | (opcode & 0xFFFF): u32]`
//! - All payload fields padded to 32-bit (4-byte) boundaries.
//! - File descriptors (`fd` in upstream XML) are transmitted on Windows NT via
//!   `DuplicateHandle` into the target process and serialized inline as a 64-bit
//!   NT `HANDLE` (`u64` / two 32-bit words).

use anyhow::{anyhow, bail, Result};
use bitflags::bitflags;
use serde::{Deserialize, Serialize};

/// Reserved object ID for `wl_display` on every Wayland connection.
pub const WL_DISPLAY_OBJECT_ID: u32 = 1;

/// Standard Wayland global interface names exposed by the Hyprland-Win compositor.
pub mod interfaces {
    pub const WL_DISPLAY: &str = "wl_display";
    pub const WL_REGISTRY: &str = "wl_registry";
    pub const WL_CALLBACK: &str = "wl_callback";
    pub const WL_COMPOSITOR: &str = "wl_compositor";
    pub const WL_SHM: &str = "wl_shm";
    pub const WL_SHM_POOL: &str = "wl_shm_pool";
    pub const WL_BUFFER: &str = "wl_buffer";
    pub const WL_SURFACE: &str = "wl_surface";
    pub const WL_REGION: &str = "wl_region";
    pub const WL_OUTPUT: &str = "wl_output";
    pub const WL_SEAT: &str = "wl_seat";
    pub const WL_POINTER: &str = "wl_pointer";
    pub const WL_KEYBOARD: &str = "wl_keyboard";
    pub const XDG_WM_BASE: &str = "xdg_wm_base";
    pub const XDG_SURFACE: &str = "xdg_surface";
    pub const XDG_TOPLEVEL: &str = "xdg_toplevel";
    pub const ZWLR_LAYER_SHELL_V1: &str = "zwlr_layer_shell_v1";
    pub const ZWLR_LAYER_SURFACE_V1: &str = "zwlr_layer_surface_v1";
    /// Windows NT zero-copy GPU texture sharing extension (equivalent to `zwp_linux_dmabuf_v1`).
    pub const WP_NT_DXGI_BUFFER_V1: &str = "wp_nt_dxgi_buffer_v1";
}

/// 8-byte Wayland message header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WlMessageHeader {
    pub sender_id: u32,
    pub opcode: u16,
    pub size: u16,
}

impl WlMessageHeader {
    pub const SIZE: usize = 8;

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < Self::SIZE {
            bail!("Wayland header requires at least 8 bytes, got {}", bytes.len());
        }
        let sender_id = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let word2 = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let opcode = (word2 & 0xFFFF) as u16;
        let size = (word2 >> 16) as u16;
        if (size as usize) < Self::SIZE || (size as usize) % 4 != 0 {
            bail!("Invalid Wayland packet size {} (must be >= 8 and 4-byte aligned)", size);
        }
        Ok(Self { sender_id, opcode, size })
    }

    pub fn to_bytes(self) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[0..4].copy_from_slice(&self.sender_id.to_le_bytes());
        let word2 = ((self.size as u32) << 16) | (self.opcode as u32);
        out[4..8].copy_from_slice(&word2.to_le_bytes());
        out
    }
}

/// Wayland 24.8 signed fixed-point number (`wl_fixed_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WlFixed(pub i32);

impl WlFixed {
    #[inline]
    pub fn from_f64(val: f64) -> Self {
        Self((val * 256.0).round() as i32)
    }

    #[inline]
    pub fn to_f64(self) -> f64 {
        (self.0 as f64) / 256.0
    }
}

/// Pixel formats for `wl_shm` and `wp_nt_dxgi_buffer_v1`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WlShmFormat {
    Argb8888 = 0,
    Xrgb8888 = 1,
    Abgr8888 = 0x34324241, // DXGI_FORMAT_R8G8B8A8_UNORM
    Xbgr8888 = 0x34324258,
    Argb2101010 = 0x30335241, // 10-bit HDR
}

/// `zwlr_layer_shell_v1` layer stacking order (used by `waybar`, `hyprpaper`, `rofi`, `mako`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LayerShellLayer {
    Background = 0,
    Bottom = 1,
    Top = 2,
    Overlay = 3,
}

bitflags! {
    /// Edge anchoring flags for `zwlr_layer_surface_v1` (e.g. `waybar` anchored `TOP | LEFT | RIGHT`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[derive(Serialize, Deserialize)]
    pub struct LayerSurfaceAnchor: u32 {
        const TOP = 1 << 0;
        const BOTTOM = 1 << 1;
        const LEFT = 1 << 2;
        const RIGHT = 1 << 3;
    }
}

/// `xdg_toplevel` state array values sent in `xdg_toplevel::configure`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum XdgToplevelState {
    Maximized = 1,
    Fullscreen = 2,
    Resizing = 3,
    Activated = 4,
    TiledLeft = 5,
    TiledRight = 6,
    TiledTop = 7,
    TiledBottom = 8,
}

/// Builder for serializing 32-bit aligned Wayland wire messages.
#[derive(Debug, Clone)]
pub struct WlMessageBuilder {
    sender_id: u32,
    opcode: u16,
    payload: Vec<u8>,
}

impl WlMessageBuilder {
    pub fn new(sender_id: u32, opcode: u16) -> Self {
        Self {
            sender_id,
            opcode,
            payload: Vec::with_capacity(32),
        }
    }

    pub fn put_u32(&mut self, v: u32) -> &mut Self {
        self.payload.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn put_i32(&mut self, v: i32) -> &mut Self {
        self.payload.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn put_u64_handle(&mut self, handle: u64) -> &mut Self {
        self.payload.extend_from_slice(&handle.to_le_bytes());
        self
    }

    pub fn put_fixed(&mut self, v: WlFixed) -> &mut Self {
        self.put_i32(v.0)
    }

    pub fn put_string(&mut self, s: &str) -> &mut Self {
        let bytes = s.as_bytes();
        let len_with_nul = (bytes.len() + 1) as u32;
        self.put_u32(len_with_nul);
        self.payload.extend_from_slice(bytes);
        self.payload.push(0); // NUL terminator
        // Pad to 4-byte boundary
        while self.payload.len() % 4 != 0 {
            self.payload.push(0);
        }
        self
    }

    pub fn put_array(&mut self, data: &[u8]) -> &mut Self {
        self.put_u32(data.len() as u32);
        self.payload.extend_from_slice(data);
        while self.payload.len() % 4 != 0 {
            self.payload.push(0);
        }
        self
    }

    pub fn finish(self) -> Vec<u8> {
        let total_size = (WlMessageHeader::SIZE + self.payload.len()) as u16;
        let header = WlMessageHeader {
            sender_id: self.sender_id,
            opcode: self.opcode,
            size: total_size,
        };
        let mut packet = Vec::with_capacity(total_size as usize);
        packet.extend_from_slice(&header.to_bytes());
        packet.extend_from_slice(&self.payload);
        packet
    }
}

/// Zero-copy cursor for reading fields out of a Wayland message payload.
pub struct WlMessageParser<'a> {
    buf: &'a [u8],
    offset: usize,
}

impl<'a> WlMessageParser<'a> {
    pub fn new(payload: &'a [u8]) -> Self {
        Self { buf: payload, offset: 0 }
    }

    pub fn read_u32(&mut self) -> Result<u32> {
        if self.offset + 4 > self.buf.len() {
            bail!("Wayland payload underflow reading u32 at offset {}", self.offset);
        }
        let v = u32::from_le_bytes([
            self.buf[self.offset],
            self.buf[self.offset + 1],
            self.buf[self.offset + 2],
            self.buf[self.offset + 3],
        ]);
        self.offset += 4;
        Ok(v)
    }

    pub fn read_i32(&mut self) -> Result<i32> {
        Ok(self.read_u32()? as i32)
    }

    pub fn read_u64_handle(&mut self) -> Result<u64> {
        if self.offset + 8 > self.buf.len() {
            bail!("Wayland payload underflow reading u64 handle at offset {}", self.offset);
        }
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&self.buf[self.offset..self.offset + 8]);
        self.offset += 8;
        Ok(u64::from_le_bytes(arr))
    }

    pub fn read_fixed(&mut self) -> Result<WlFixed> {
        Ok(WlFixed(self.read_i32()?))
    }

    pub fn read_string(&mut self) -> Result<String> {
        let len = self.read_u32()? as usize;
        if len == 0 {
            return Ok(String::new());
        }
        let aligned = (len + 3) & !3;
        if self.offset + aligned > self.buf.len() {
            bail!("Wayland string payload underflow (len={}, aligned={})", len, aligned);
        }
        let str_bytes = &self.buf[self.offset..self.offset + len - 1];
        self.offset += aligned;
        String::from_utf8(str_bytes.to_vec()).map_err(|e| anyhow!("Invalid UTF-8 in Wayland string: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_wayland_wire_message() {
        let mut b = WlMessageBuilder::new(1, 0);
        b.put_u32(42)
            .put_string("xdg_wm_base")
            .put_u64_handle(0x1234_5678_9ABC_DEF0);
        let bytes = b.finish();

        let hdr = WlMessageHeader::from_bytes(&bytes).unwrap();
        assert_eq!(hdr.sender_id, 1);
        assert_eq!(hdr.opcode, 0);
        assert_eq!(hdr.size as usize, bytes.len());

        let mut p = WlMessageParser::new(&bytes[WlMessageHeader::SIZE..]);
        assert_eq!(p.read_u32().unwrap(), 42);
        assert_eq!(p.read_string().unwrap(), "xdg_wm_base");
        assert_eq!(p.read_u64_handle().unwrap(), 0x1234_5678_9ABC_DEF0);
    }

    #[test]
    fn layer_surface_anchor_bitflags_and_serde() {
        let anchor = LayerSurfaceAnchor::TOP | LayerSurfaceAnchor::LEFT;
        assert!(anchor.intersects(LayerSurfaceAnchor::TOP));
        assert!(!anchor.intersects(LayerSurfaceAnchor::BOTTOM));
        assert_eq!(anchor.bits(), 0b0101);

        let json = serde_json::to_string(&anchor).expect("serialize anchor");
        let deserialized: LayerSurfaceAnchor = serde_json::from_str(&json).expect("deserialize anchor");
        assert_eq!(anchor, deserialized);
    }
}
