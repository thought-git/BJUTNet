//! 提权 / 以用户身份启动 / 延迟删除 这类 shell 操作。

use std::path::Path;

use windows::core::{w, HSTRING};
use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_DELAY_UNTIL_REBOOT};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// `ShellExecuteW` 返回值 > 32 视为成功
fn se_ok(r: windows::Win32::Foundation::HINSTANCE) -> bool {
    r.0 as isize > 32
}

/// 以管理员身份重新启动自己（弹 UAC）。返回 `Ok(())` 表示已成功拉起新实例。
pub fn relaunch_as_admin(args: &[String]) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("取自身路径失败: {e}"))?;
    let params = args
        .iter()
        .map(|a| {
            if a.contains(' ') || a.is_empty() {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    let r = unsafe {
        ShellExecuteW(
            None,
            w!("runas"),
            &HSTRING::from(exe.as_os_str().to_string_lossy().to_string()),
            &HSTRING::from(params),
            None,
            SW_SHOWNORMAL,
        )
    };
    if se_ok(r) {
        Ok(())
    } else {
        Err(format!(
            "提权失败（UAC 被取消？返回值 {}）",
            r.0 as isize
        ))
    }
}

/// 以**当前用户身份**启动一个程序。
///
/// 安装器是提权运行的，直接 CreateProcess 出来的面板也会是管理员权限——那样面板写
/// `%ProgramData%` 之外的路径行为会和"普通双击"不一致，也没必要。用 explorer.exe 转发
/// 就能落到普通用户会话（经典做法）。
pub fn launch_as_invoker(exe: &Path) -> Result<(), String> {
    if !exe.is_file() {
        return Err(format!("找不到 {}", exe.display()));
    }
    std::process::Command::new("explorer.exe")
        .arg(exe)
        .spawn()
        .map_err(|e| format!("启动失败: {e}"))?;
    Ok(())
}

pub fn open_folder(path: &Path) {
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
}

/// 重启后删除（用于删掉正在运行的卸载器自身或其目录）
pub fn schedule_delete(path: &Path) -> Result<(), String> {
    let p = HSTRING::from(path.as_os_str().to_string_lossy().to_string());
    unsafe { MoveFileExW(&p, windows::core::PCWSTR::null(), MOVEFILE_DELAY_UNTIL_REBOOT) }
        .map_err(|e| format!("登记重启删除失败: {e}"))
}

/// 等某个进程退出（用于"父进程退出后子进程再删目录"）
pub fn wait_process_exit(pid: u32, timeout_ms: u64) -> bool {
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };
    unsafe {
        let h = OpenProcess(PROCESS_SYNCHRONIZE, false, pid);
        match h {
            Ok(handle) if !handle.is_invalid() => {
                let r = WaitForSingleObject(handle, timeout_ms as u32);
                let _ = windows::Win32::Foundation::CloseHandle(handle);
                r.0 == 0 // WAIT_OBJECT_0
            }
            _ => true, // 进程已经没了
        }
    }
}
