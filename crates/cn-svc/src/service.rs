//! Windows 服务外壳：手写 SCM FFI（不引 `windows-service` crate —— 它不在离线缓存里）。
//!
//! 关键点（旧方案在这里翻过车）：
//! - `SERVICE_STATUS_HANDLE` 是 64 位指针，必须用 `usize` 存；存成 u32 会截断句柄，
//!   导致 `SetServiceStatus` 永远失败、SCM 30 s 判超时（事件日志 7000/7009）。
//! - `START_PENDING` 期间必须递增 `dwCheckPoint` 并给 `dwWaitHint`，否则 SCM 会以为卡死。
//! - 服务模式**绝不写 stdout/stderr**（Session 0 没有控制台）。
//! - 手动执行 `--service` 时 `StartServiceCtrlDispatcherW` 返回 1063，
//!   此时退化成前台无界面进程，方便调试。

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use cn_core::logging;

use crate::ipc_server::{self, Ctx};
use crate::worker::{self, Deps};

/// SCM 回调签名固定、没有参数可传，所以先把 worker 依赖存起来，等 `service_main` 起来后取走。
static PENDING: OnceLock<Mutex<Option<Deps>>> = OnceLock::new();

fn take_deps() -> Option<Deps> {
    PENDING
        .get()
        .and_then(|m| m.lock().ok())
        .and_then(|mut g| g.take())
}

// 服务身份常量统一放在 cn-core::app，安装器/卸载器用同一份
pub use cn_core::app::SERVICE_NAME;

/// `--service` 入口：优先按服务跑；不是 SCM 拉起的就当前台程序跑
pub fn run_as_service(debug: bool, ipc: Ctx, deps: Deps) {
    crate::init_logging(false, debug);
    let _ = PENDING.set(Mutex::new(Some(deps)));
    ipc_server::spawn(Arc::new(ipc));

    if scm::try_run() {
        return;
    }
    // 手动执行 --service：退化成前台（有控制台时回声）
    logging::set_echo(true);
    logging::info("未由 SCM 启动：按前台模式运行（Ctrl+C 结束）");
    if let Some(d) = take_deps() {
        worker::run_foreground(d);
    }
}

/// `--console` 入口（开发/排障用，同时也开 IPC，面板可以连上来）
pub fn run_console(debug: bool, ipc: Ctx, deps: Deps) {
    crate::init_logging(true, debug);
    ipc_server::spawn(Arc::new(ipc));
    worker::run_foreground(deps);
}

mod scm {
    use super::*;

    #[repr(C)]
    struct ServiceStatus {
        service_type: u32,
        current_state: u32,
        controls_accepted: u32,
        win32_exit_code: u32,
        service_specific_exit_code: u32,
        checkpoint: u32,
        wait_hint: u32,
    }

    #[repr(C)]
    struct ServiceTableEntry {
        name: *const u16,
        proc: Option<unsafe extern "system" fn(u32, *mut *mut u16)>,
    }

    const SERVICE_WIN32_OWN_PROCESS: u32 = 0x0000_0010;
    const SERVICE_ACCEPT_STOP: u32 = 0x0000_0001;
    const SERVICE_ACCEPT_SHUTDOWN: u32 = 0x0000_0004;
    const SERVICE_START_PENDING: u32 = 0x0000_0002;
    const SERVICE_STOP_PENDING: u32 = 0x0000_0003;
    const SERVICE_RUNNING: u32 = 0x0000_0004;
    const SERVICE_STOPPED: u32 = 0x0000_0001;
    const SERVICE_CONTROL_STOP: u32 = 0x0000_0001;
    const SERVICE_CONTROL_SHUTDOWN: u32 = 0x0000_0005;
    const ERROR_FAILED_SERVICE_CONTROLLER_CONNECT: u32 = 1063;

    #[link(name = "advapi32")]
    extern "system" {
        fn StartServiceCtrlDispatcherW(table: *const ServiceTableEntry) -> i32;
        fn RegisterServiceCtrlHandlerExW(
            name: *const u16,
            handler: Option<unsafe extern "system" fn(u32, u32, *mut c_void, *mut c_void) -> u32>,
            context: *mut c_void,
        ) -> *mut c_void;
        fn SetServiceStatus(handle: *mut c_void, status: *const ServiceStatus) -> i32;
    }

    static STATUS_HANDLE: AtomicUsize = AtomicUsize::new(0);
    static STOP: AtomicBool = AtomicBool::new(false);
    static WORKER_DONE: AtomicBool = AtomicBool::new(false);

    fn wide(s: &str) -> Vec<u16> {
        cn_core::util::wide(s)
    }

    fn report(state: u32, controls: u32, checkpoint: u32, wait_hint: u32) {
        let handle = STATUS_HANDLE.load(Ordering::Relaxed) as *mut c_void;
        if handle.is_null() {
            return;
        }
        let status = ServiceStatus {
            service_type: SERVICE_WIN32_OWN_PROCESS,
            current_state: state,
            controls_accepted: controls,
            win32_exit_code: 0,
            service_specific_exit_code: 0,
            checkpoint,
            wait_hint,
        };
        unsafe {
            if SetServiceStatus(handle, &status) == 0 {
                // 不能写 stdout（服务模式），写文件日志
                cn_core::logging::warn("SetServiceStatus 失败（句柄或状态不被接受）");
            }
        }
    }

    unsafe extern "system" fn handler(
        control: u32,
        _event_type: u32,
        _event_data: *mut c_void,
        _context: *mut c_void,
    ) -> u32 {
        match control {
            SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
                cn_core::logging::info(format!("收到停止请求（control={control}）"));
                report(SERVICE_STOP_PENDING, 0, 1, 20_000);
                STOP.store(true, Ordering::Relaxed);
            }
            _ => {}
        }
        0
    }

    unsafe extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
        let name = wide(SERVICE_NAME);
        let handle = RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(handler), std::ptr::null_mut());
        STATUS_HANDLE.store(handle as usize, Ordering::Relaxed);
        if handle.is_null() {
            cn_core::logging::err("RegisterServiceCtrlHandlerExW 失败，服务无法上报状态");
            return;
        }

        // 启动中：给 SCM 明确的进度（checkpoint 递增 + wait hint）
        report(SERVICE_START_PENDING, 0, 1, 30_000);

        let deps = take_deps();
        // SCM 的"停止"和面板的"关闭服务"共用同一个 stop 标志
        let stop_flag = deps.as_ref().map(|d| d.stop.clone());
        std::thread::spawn(move || {
            match deps {
                Some(d) => worker::run_service(d),
                None => cn_core::logging::err("worker 依赖缺失，服务无法启动主循环"),
            }
            WORKER_DONE.store(true, Ordering::Relaxed);
        });

        report(
            SERVICE_RUNNING,
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN,
            0,
            0,
        );
        cn_core::logging::info("服务已进入 RUNNING 状态");

        // 等停止信号或 worker 自己退出
        loop {
            if STOP.load(Ordering::Relaxed) || WORKER_DONE.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }

        if let Some(f) = &stop_flag {
            f.store(true, Ordering::Release);
        }
        // 最多等 10 s 收尾
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !WORKER_DONE.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        report(SERVICE_STOPPED, 0, 0, 0);
        cn_core::logging::info("服务已停止");
    }

    /// true = 由 SCM 启动并已跑完；false = 不是 SCM 启动（调用方回退前台）
    pub fn try_run() -> bool {
        let name = wide(SERVICE_NAME);
        let table = [
            ServiceTableEntry {
                name: name.as_ptr(),
                proc: Some(service_main),
            },
            ServiceTableEntry {
                name: std::ptr::null(),
                proc: None,
            },
        ];
        let ok = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) };
        if ok == 0 {
            let err = cn_core::util::last_error();
            if err == ERROR_FAILED_SERVICE_CONTROLLER_CONNECT {
                return false;
            }
            cn_core::logging::err(format!("StartServiceCtrlDispatcherW 失败：错误码 {err}"));
            return false;
        }
        true
    }
}
