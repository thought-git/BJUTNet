//! 命名管道的 Win32 薄封装（服务端建管道、客户端连管道、收发帧）。
//!
//! 安全描述符用 SDDL 一行搞定，比手工拼 SECURITY_DESCRIPTOR/ACL 稳得多：
//!
//! ```text
//! D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;BU)
//!    └ SYSTEM 全权  └ 管理员全权   └ 普通用户读写
//! ```
//!
//! 为什么要显式写：服务以 LocalSystem 建管道时，**默认 DACL 普通用户根本连不上**，
//! 面板就会永远"连不上服务"。反过来也不能设成 Everyone 全权。

use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FlushFileBuffers, ReadFile, WriteFile, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_SHARE_MODE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW, PIPE_READMODE_BYTE,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

pub use windows::Win32::Foundation::HANDLE as Handle;

/// 默认管道名（GUI 与服务必须一致）
pub const DEFAULT_PIPE: &str = "CampusNet.v1";

/// 实际管道名：环境变量 `CAMPUSNET_PIPE` 可覆盖（测试/多实例隔离用）
pub fn pipe_name() -> String {
    std::env::var("CAMPUSNET_PIPE")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_PIPE.to_string())
}

/// 管道完整路径：`\\.\pipe\<name>`
pub fn pipe_path() -> String {
    format!(r"\\.\pipe\{}", pipe_name())
}

const BUFFER: u32 = 64 * 1024;
const MAX_FRAME: usize = 8 * 1024 * 1024;
const ERROR_PIPE_BUSY: u32 = 231;
const ERROR_PIPE_CONNECTED: u32 = 535;

fn win_code(e: &windows::core::Error) -> u32 {
    (e.code().0 as u32) & 0xFFFF
}

fn io_err(op: &str) -> std::io::Error {
    let code = std::io::Error::last_os_error();
    std::io::Error::new(
        code.kind(),
        format!("{op}失败: {code}（Win32 {}）", code.raw_os_error().unwrap_or(0)),
    )
}

// ---------------------------------------------------------------------------
// 服务端
// ---------------------------------------------------------------------------

/// 建一个服务端管道实例（带显式 DACL）。调用方负责 `close`。
pub fn create_server_pipe() -> std::io::Result<HANDLE> {
    // 1) SDDL → SECURITY_DESCRIPTOR
    let mut psd = windows::Win32::Security::PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            w!("D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;BU)"),
            SDDL_REVISION_1,
            &mut psd,
            None,
        )
        .map_err(|e| std::io::Error::other(format!("SDDL 解析失败: {e}")))?;
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: psd.0,
        bInheritHandle: windows::core::BOOL(0),
    };

    let name = crate::util::wide(&pipe_path());
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            BUFFER,
            BUFFER,
            0,
            Some(&sa as *const _),
        )
    };

    // 管道对象已经复制了安全描述符，可以释放了
    unsafe {
        let _ = LocalFree(Some(HLOCAL(psd.0)));
    }

    if handle.is_invalid() {
        return Err(io_err("CreateNamedPipeW"));
    }
    Ok(handle)
}

/// 等一个客户端连上来（客户端在 ConnectNamedPipe 之前就连上会返回 535，当作成功）
pub fn accept(handle: HANDLE) -> std::io::Result<()> {
    match unsafe { ConnectNamedPipe(handle, None) } {
        Ok(()) => Ok(()),
        Err(e) if win_code(&e) == ERROR_PIPE_CONNECTED => Ok(()),
        Err(e) => Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("ConnectNamedPipe 失败（Win32 {}）", win_code(&e)),
        )),
    }
}

pub fn disconnect(handle: HANDLE) {
    unsafe {
        let _ = DisconnectNamedPipe(handle);
    }
}

pub fn flush(handle: HANDLE) {
    unsafe {
        let _ = FlushFileBuffers(handle);
    }
}

pub fn close(handle: HANDLE) {
    unsafe {
        let _ = CloseHandle(handle);
    }
}

// ---------------------------------------------------------------------------
// 客户端
// ---------------------------------------------------------------------------

/// 连服务；`timeout` 内含"服务还没起来/管道忙"的重试
pub fn connect(timeout: Duration) -> std::io::Result<HANDLE> {
    let deadline = Instant::now() + timeout;
    let access = (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0;

    loop {
        let full = pipe_path();
        let name = crate::util::wide(&full);
        let r = unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                access,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        };
        match r {
            Ok(h) if !h.is_invalid() => return Ok(h),
            _ => {
                let code = std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(0) as u32;
                if code != ERROR_PIPE_BUSY {
                    // 最常见的是 2（服务没运行）
                    return Err(io_err("连接服务管道"));
                }
                let remain = deadline.saturating_duration_since(Instant::now());
                if remain.is_zero() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "连接服务超时（管道忙）",
                    ));
                }
                unsafe {
                    let _ = WaitNamedPipeW(PCWSTR(name.as_ptr()), remain.as_millis() as u32);
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "连接服务超时",
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// 帧读写（收发两端共用）
// ---------------------------------------------------------------------------

fn read_exact(h: HANDLE, buf: &mut [u8]) -> std::io::Result<()> {
    let mut off = 0usize;
    while off < buf.len() {
        let mut got: u32 = 0;
        match unsafe { ReadFile(h, Some(&mut buf[off..]), Some(&mut got), None) } {
            Ok(()) => {
                if got == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "对端已关闭",
                    ));
                }
                off += got as usize;
            }
            Err(e) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("ReadFile 失败（Win32 {}）", win_code(&e)),
                ))
            }
        }
    }
    Ok(())
}

fn write_all(h: HANDLE, buf: &[u8]) -> std::io::Result<()> {
    let mut off = 0usize;
    while off < buf.len() {
        let mut wrote: u32 = 0;
        match unsafe { WriteFile(h, Some(&buf[off..]), Some(&mut wrote), None) } {
            Ok(()) => {
                if wrote == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "WriteFile 写了 0 字节",
                    ));
                }
                off += wrote as usize;
            }
            Err(e) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    format!("WriteFile 失败（Win32 {}）", win_code(&e)),
                ))
            }
        }
    }
    Ok(())
}

pub fn write_frame(h: HANDLE, payload: &[u8]) -> std::io::Result<()> {
    if payload.len() > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "帧太大",
        ));
    }
    let len = (payload.len() as u32).to_le_bytes();
    write_all(h, &len)?;
    write_all(h, payload)
}

/// 读一帧；对端正常关闭返回 `Ok(None)`
pub fn read_frame(h: HANDLE) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match read_exact(h, &mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("帧长度异常: {len}"),
        ));
    }
    let mut buf = vec![0u8; len];
    read_exact(h, &mut buf)?;
    Ok(Some(buf))
}


