//! 单实例：重复双击时不再开第二个面板，而是把已有窗口拉到前台。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

const MUTEX: &str = "Local\\BJUTNet.Gui.SingleInstance";
const TITLE: &str = "BJUTNet 校园网自动登录";

#[link(name = "kernel32")]
extern "system" {
    fn CreateMutexW(
        attr: *mut core::ffi::c_void,
        owner: i32,
        name: *const u16,
    ) -> *mut core::ffi::c_void;
    fn GetLastError() -> u32;
}

#[link(name = "user32")]
extern "system" {
    fn FindWindowW(class: *const u16, title: *const u16) -> *mut core::ffi::c_void;
    fn ShowWindow(hwnd: *mut core::ffi::c_void, cmd: i32) -> i32;
    fn SetForegroundWindow(hwnd: *mut core::ffi::c_void) -> i32;
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// 已有实例时激活它并退出当前进程；否则持有互斥体直到进程结束。
pub fn ensure_single() {
    const ERROR_ALREADY_EXISTS: u32 = 183;
    unsafe {
        let name = wide(MUTEX);
        let handle = CreateMutexW(std::ptr::null_mut(), 0, name.as_ptr());
        if handle.is_null() {
            return; // 创建失败就不拦，宁可开两个也别打不开
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            activate_existing();
            // 不 CloseHandle 也无所谓：马上 exit，系统会回收
            std::process::exit(0);
        }
        // 成功创建：故意不关闭句柄，让它活到进程退出
        let _ = handle;
    }
}

fn activate_existing() {
    unsafe {
        let title = wide(TITLE);
        // 对方可能还在创建窗口，最多等 2 秒
        for _ in 0..20 {
            let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
            if !hwnd.is_null() {
                const SW_RESTORE: i32 = 9;
                ShowWindow(hwnd, SW_RESTORE);
                SetForegroundWindow(hwnd);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}
