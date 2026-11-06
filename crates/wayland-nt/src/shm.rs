//! Windows NT implementation of `wl_shm_pool` using NT Section Objects
//! (`CreateFileMappingW` / `MapViewOfFile` / `UnmapViewOfFile`).
//!
//! Replaces Linux `memfd_create` + `mmap` so standard CPU-rendered Wayland clients
//! (`waybar`, `rofi-wayland`, `wofi`, `mako`, `foot`) can share pixel buffers
//! with the compositor with zero memory copies.

use anyhow::{bail, Context, Result};
use std::ptr::NonNull;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_ALL_ACCESS,
    MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};

/// A memory-mapped shared memory pool (`wl_shm_pool`) backed by an NT Section Object.
pub struct NtShmPool {
    section_handle: HANDLE,
    view_base: NonNull<u8>,
    size: usize,
}

unsafe impl Send for NtShmPool {}
unsafe impl Sync for NtShmPool {}

impl NtShmPool {
    /// Allocate a new anonymous pagefile-backed shared memory pool of `size` bytes.
    pub fn create_anonymous(size: usize) -> Result<Self> {
        if size == 0 {
            bail!("wl_shm_pool size must be > 0");
        }
        unsafe {
            let size_high = ((size as u64) >> 32) as u32;
            let size_low = (size as u64 & 0xFFFF_FFFF) as u32;

            let section = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                size_high,
                size_low,
                PCWSTR::null(),
            )
            .context("CreateFileMappingW failed for wl_shm_pool")?;

            Self::from_section_handle(section, size)
        }
    }

    /// Map an existing NT Section `HANDLE` (imported from a Wayland client via `DuplicateHandle`).
    pub fn from_section_handle(section_handle: HANDLE, size: usize) -> Result<Self> {
        if size == 0 {
            bail!("wl_shm_pool size must be > 0");
        }
        unsafe {
            let view = MapViewOfFile(section_handle, FILE_MAP_ALL_ACCESS, 0, 0, size);
            let Some(ptr) = NonNull::new(view.Value as *mut u8) else {
                let _ = CloseHandle(section_handle);
                bail!("MapViewOfFile failed for wl_shm_pool (size={size})");
            };
            Ok(Self {
                section_handle,
                view_base: ptr,
                size,
            })
        }
    }

    #[inline]
    pub fn raw_handle(&self) -> u64 {
        self.section_handle.0 as u64
    }

    #[inline]
    pub fn size(&self) -> usize {
        self.size
    }

    /// Borrow a sub-slice of the shared memory pool for reading a `wl_buffer`.
    pub fn slice(&self, offset: usize, len: usize) -> Result<&[u8]> {
        if offset.saturating_add(len) > self.size {
            bail!(
                "wl_buffer slice out of bounds (offset={}, len={}, pool_size={})",
                offset,
                len,
                self.size
            );
        }
        unsafe { Ok(std::slice::from_raw_parts(self.view_base.as_ptr().add(offset), len)) }
    }

    /// Borrow a mutable sub-slice of the shared memory pool.
    pub fn slice_mut(&mut self, offset: usize, len: usize) -> Result<&mut [u8]> {
        if offset.saturating_add(len) > self.size {
            bail!(
                "wl_buffer mutable slice out of bounds (offset={}, len={}, pool_size={})",
                offset,
                len,
                self.size
            );
        }
        unsafe { Ok(std::slice::from_raw_parts_mut(self.view_base.as_ptr().add(offset), len)) }
    }
}

impl Drop for NtShmPool {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.view_base.as_ptr() as *mut core::ffi::c_void,
            });
            let _ = CloseHandle(self.section_handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_write_nt_shm_pool() {
        let mut pool = NtShmPool::create_anonymous(4096).unwrap();
        let buf = pool.slice_mut(0, 16).unwrap();
        buf[0..4].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(&pool.slice(0, 4).unwrap()[..], &[0xDE, 0xAD, 0xBE, 0xEF]);
    }
}
