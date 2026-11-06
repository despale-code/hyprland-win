//! Windows NT cross-process `HANDLE` passing (replacement for Unix `SCM_RIGHTS`).
//!
//! In Linux Wayland, file descriptors (`memfd` for `wl_shm` and `dma-buf` for GPUs)
//! are passed out-of-band via `sendmsg(..., SCM_RIGHTS)`.
//!
//! On Windows NT, when a Wayland client connects over `\\.\pipe\wayland-0`, the
//! compositor retrieves the client's PID via `GetNamedPipeClientProcessId` and
//! calls `DuplicateHandle` to import the client's Section `HANDLE` (`wl_shm`) or
//! DXGI Shared Texture `HANDLE` (`wp_nt_dxgi_buffer_v1`) directly into the
//! compositor's handle table with zero kernel copies.

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE};
use windows::Win32::System::Pipes::GetNamedPipeClientProcessId;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE};

/// Query the client Process ID connected to the server end of a Windows Named Pipe.
pub fn get_pipe_client_pid(pipe_handle: HANDLE) -> Result<u32> {
    let mut client_pid: u32 = 0;
    unsafe {
        GetNamedPipeClientProcessId(pipe_handle, &mut client_pid)
            .context("GetNamedPipeClientProcessId failed")?;
    }
    Ok(client_pid)
}

/// Duplicate a `HANDLE` owned by `source_pid` into the current (compositor) process.
///
/// Works seamlessly for:
/// - NT Section Objects (`CreateFileMappingW`) used by `wl_shm_pool`
/// - NT Event Objects (`CreateEventW`) used by explicit sync timelines
/// - DXGI / D3DKMT Shared Resource Handles (`IDXGIResource::GetSharedHandle`)
pub fn import_handle_from_client(source_pid: u32, client_handle_raw: u64) -> Result<HANDLE> {
    unsafe {
        let source_proc = OpenProcess(PROCESS_DUP_HANDLE, false, source_pid)
            .with_context(|| format!("OpenProcess(PROCESS_DUP_HANDLE) failed for PID {source_pid}"))?;

        let mut duplicated = HANDLE::default();
        let current_proc = GetCurrentProcess();
        let src_handle = HANDLE(client_handle_raw as *mut core::ffi::c_void);

        let res = DuplicateHandle(
            source_proc,
            src_handle,
            current_proc,
            &mut duplicated,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        );

        let _ = CloseHandle(source_proc);
        res.context("DuplicateHandle from Wayland client into compositor failed")?;
        Ok(duplicated)
    }
}
