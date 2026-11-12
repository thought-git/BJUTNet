//! 服务控制（SCM FFI）：安装/启动/停止/删除/查询。
//!
//! 为什么手写 FFI 而不用 `sc.exe`：
//! 路径里有空格（`C:\Program Files\CampusNet\...`）时，`sc.exe` 的 `binPath= "..."` 引号转义
//! 极易出错；`CreateServiceW` 直接传宽字符串，零转义风险。
//!
//! 注意：这一组操作**几乎都需要管理员权限**（除 query_state）。

use std::ffi::c_void;

use crate::net;
use crate::util;

type ScHandle = *mut c_void;

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
struct ServiceDescriptionW {
    lp_description: *const u16,
}

#[repr(C)]
struct ScAction {
    action_type: u32,
    delay: u32,
}

#[repr(C)]
struct ServiceFailureActionsW {
    reset_period: u32,
    reboot_msg: *const u16,
    command: *const u16,
    actions_count: u32,
    actions: *mut ScAction,
}

#[repr(C)]
struct ServiceDelayedAutoStartInfo {
    delayed_autostart: i32,
}

#[repr(C)]
struct ServiceFailureActionsFlag {
    failure_actions_on_non_crash_failures: i32,
}

const SC_MANAGER_CONNECT: u32 = 0x0001;
const SC_MANAGER_CREATE_SERVICE: u32 = 0x0002;

const SERVICE_ALL_ACCESS: u32 = 0x000F_01FF;
const SERVICE_WIN32_OWN_PROCESS: u32 = 0x0000_0010;
const SERVICE_AUTO_START: u32 = 0x0000_0002;
const SERVICE_ERROR_NORMAL: u32 = 0x0000_0001;

const SERVICE_QUERY_STATUS: u32 = 0x0004;
const SERVICE_START: u32 = 0x0010;
const SERVICE_STOP: u32 = 0x0020;
const SERVICE_CONTROL_STOP: u32 = 0x0000_0001;
const DELETE: u32 = 0x0001_0000;

pub const STATE_STOPPED: u32 = 0x0000_0001;
pub const STATE_RUNNING: u32 = 0x0000_0004;

const SERVICE_CONFIG_DESCRIPTION: u32 = 1;
const SERVICE_CONFIG_FAILURE_ACTIONS: u32 = 2;
const SERVICE_CONFIG_DELAYED_AUTO_START_INFO: u32 = 3;
const SERVICE_CONFIG_FAILURE_ACTIONS_FLAG: u32 = 4;

const SC_ACTION_RESTART: u32 = 1;

#[link(name = "advapi32")]
extern "system" {
    fn OpenSCManagerW(machine: *const u16, database: *const u16, access: u32) -> ScHandle;
    fn CreateServiceW(
        scm: ScHandle,
        service_name: *const u16,
        display_name: *const u16,
        desired_access: u32,
        service_type: u32,
        start_type: u32,
        error_control: u32,
        bin_path: *const u16,
        load_order_group: *const u16,
        tag_id: *mut u32,
        dependencies: *const u16,
        start_name: *const u16,
        password: *const u16,
    ) -> ScHandle;
    fn OpenServiceW(scm: ScHandle, service_name: *const u16, access: u32) -> ScHandle;
    fn ChangeServiceConfig2W(service: ScHandle, info_level: u32, info: *const c_void) -> i32;
    fn StartServiceW(service: ScHandle, argc: u32, argv: *const *const u16) -> i32;
    fn ControlService(service: ScHandle, control: u32, status: *mut ServiceStatus) -> i32;
    fn QueryServiceStatus(service: ScHandle, status: *mut ServiceStatus) -> i32;
    fn DeleteService(service: ScHandle) -> i32;
    fn CloseServiceHandle(handle: ScHandle) -> i32;
}

fn describe_error(code: u32) -> &'static str {
    match code {
        5 => "（拒绝访问：需要管理员权限）",
        1060 => "（服务不存在）",
        1072 => "（服务已被标记为删除）",
        1073 => "（服务已存在）",
        1056 => "（服务已在运行）",
        1077 => "（上次启动失败，状态未更新）",
        _ => "",
    }
}

fn err(what: &str) -> String {
    let code = util::last_error();
    format!("{what}失败：错误码 {code}{}", describe_error(code))
}

fn wait_state(handle: ScHandle, want: u32, timeout_ms: u64) -> bool {
    let start = std::time::Instant::now();
    loop {
        let mut st = unsafe { std::mem::zeroed::<ServiceStatus>() };
        if unsafe { QueryServiceStatus(handle, &mut st) } == 0 {
            return false;
        }
        if st.current_state == want {
            return true;
        }
        if start.elapsed().as_millis() > timeout_ms as u128 {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// 查询服务状态（只读，不需要管理员）。服务不存在返回 None。
pub fn service_state(name: &str) -> Option<u32> {
    let name_w = util::wide(name);
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return None;
        }
        let svc = OpenServiceW(scm, name_w.as_ptr(), SERVICE_QUERY_STATUS);
        let mut state = None;
        if !svc.is_null() {
            let mut st = std::mem::zeroed::<ServiceStatus>();
            if QueryServiceStatus(svc, &mut st) != 0 {
                state = Some(st.current_state);
            }
            let _ = CloseServiceHandle(svc);
        }
        let _ = CloseServiceHandle(scm);
        state
    }
}

pub fn service_exists(name: &str) -> bool {
    service_state(name).is_some()
}

/// 安装（或重装）服务：先停删同名旧服务，再创建并配置，最后启动。
///
/// `bin_path` 一般是 `"C:\Program Files\CampusNet\CampusNetSvc.exe" --service`（含引号）。
pub fn install(
    name: &str,
    display: &str,
    desc: &str,
    bin_path: &str,
    delayed_auto_start: bool,
) -> Result<(), String> {
    if !net::is_elevated() {
        return Err("需要管理员权限".into());
    }
    let name_w = util::wide(name);
    let display_w = util::wide(display);
    let desc_w = util::wide(desc);
    let bin_w = util::wide(bin_path);

    unsafe {
        let scm = OpenSCManagerW(
            std::ptr::null(),
            std::ptr::null(),
            SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE,
        );
        if scm.is_null() {
            return Err(err("打开服务管理器"));
        }

        // 已有同名服务：先停再删，保证 binPath 是最新的
        let existing = OpenServiceW(
            scm,
            name_w.as_ptr(),
            SERVICE_STOP | SERVICE_QUERY_STATUS | DELETE,
        );
        if !existing.is_null() {
            let mut st = std::mem::zeroed::<ServiceStatus>();
            let _ = ControlService(existing, SERVICE_CONTROL_STOP, &mut st);
            wait_state(existing, STATE_STOPPED, 15_000);
            let _ = DeleteService(existing);
            let _ = CloseServiceHandle(existing);
            std::thread::sleep(std::time::Duration::from_millis(600));
        }

        let svc = CreateServiceW(
            scm,
            name_w.as_ptr(),
            display_w.as_ptr(),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            bin_w.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(), // LocalSystem
            std::ptr::null(),
        );
        if svc.is_null() {
            let _ = CloseServiceHandle(scm);
            return Err(err("创建服务"));
        }

        let sd = ServiceDescriptionW {
            lp_description: desc_w.as_ptr(),
        };
        if ChangeServiceConfig2W(svc, SERVICE_CONFIG_DESCRIPTION, &sd as *const _ as *const c_void)
            == 0
        {
            let _ = CloseServiceHandle(svc);
            let _ = CloseServiceHandle(scm);
            return Err(err("设置服务描述"));
        }

        // 崩溃自动重启：5s / 15s / 60s
        let mut actions = vec![
            ScAction {
                action_type: SC_ACTION_RESTART,
                delay: 5_000,
            },
            ScAction {
                action_type: SC_ACTION_RESTART,
                delay: 15_000,
            },
            ScAction {
                action_type: SC_ACTION_RESTART,
                delay: 60_000,
            },
        ];
        let sfa = ServiceFailureActionsW {
            reset_period: 86_400,
            reboot_msg: std::ptr::null(),
            command: std::ptr::null(),
            actions_count: actions.len() as u32,
            actions: actions.as_mut_ptr(),
        };
        if ChangeServiceConfig2W(
            svc,
            SERVICE_CONFIG_FAILURE_ACTIONS,
            &sfa as *const _ as *const c_void,
        ) == 0
        {
            let _ = CloseServiceHandle(svc);
            let _ = CloseServiceHandle(scm);
            return Err(err("设置失败恢复策略"));
        }

        let flag = ServiceFailureActionsFlag {
            failure_actions_on_non_crash_failures: 1,
        };
        let _ = ChangeServiceConfig2W(
            svc,
            SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
            &flag as *const _ as *const c_void,
        );

        if delayed_auto_start {
            let delayed = ServiceDelayedAutoStartInfo {
                delayed_autostart: 1,
            };
            let _ = ChangeServiceConfig2W(
                svc,
                SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
                &delayed as *const _ as *const c_void,
            );
        }

        let _ = CloseServiceHandle(svc);
        let _ = CloseServiceHandle(scm);
    }
    Ok(())
}

/// 启动服务并等 RUNNING（最多 20 s）
pub fn start(name: &str) -> Result<(), String> {
    let name_w = util::wide(name);
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return Err(err("打开服务管理器"));
        }
        let svc = OpenServiceW(scm, name_w.as_ptr(), SERVICE_START | SERVICE_QUERY_STATUS);
        if svc.is_null() {
            let _ = CloseServiceHandle(scm);
            return Err(err("打开服务"));
        }
        if StartServiceW(svc, 0, std::ptr::null()) == 0 {
            let code = util::last_error();
            // 1056 已在运行 / 1077 状态未更新 —— 都继续查状态
            if code != 1056 && code != 1077 {
                let _ = CloseServiceHandle(svc);
                let _ = CloseServiceHandle(scm);
                return Err(format!("启动服务失败：错误码 {code}{}", describe_error(code)));
            }
        }
        let running = wait_state(svc, STATE_RUNNING, 20_000);
        let _ = CloseServiceHandle(svc);
        let _ = CloseServiceHandle(scm);
        if running {
            Ok(())
        } else {
            Err("服务已安装，但 20 秒内没有进入 RUNNING（查事件查看器 → 系统 → SCM）".into())
        }
    }
}

/// 停止服务并等 STOPPED
pub fn stop(name: &str) -> Result<(), String> {
    let name_w = util::wide(name);
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return Err(err("打开服务管理器"));
        }
        let svc = OpenServiceW(scm, name_w.as_ptr(), SERVICE_STOP | SERVICE_QUERY_STATUS);
        if svc.is_null() {
            let _ = CloseServiceHandle(scm);
            return Err(err("打开服务"));
        }
        let mut st = std::mem::zeroed::<ServiceStatus>();
        if ControlService(svc, SERVICE_CONTROL_STOP, &mut st) == 0 {
            let code = util::last_error();
            // 1062 = 服务未启动
            if code != 1062 {
                let _ = CloseServiceHandle(svc);
                let _ = CloseServiceHandle(scm);
                return Err(format!("停止服务失败：错误码 {code}{}", describe_error(code)));
            }
        }
        wait_state(svc, STATE_STOPPED, 20_000);
        let _ = CloseServiceHandle(svc);
        let _ = CloseServiceHandle(scm);
    }
    Ok(())
}

/// 删除服务（调用前应先 stop）
pub fn delete(name: &str) -> Result<(), String> {
    let name_w = util::wide(name);
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return Err(err("打开服务管理器"));
        }
        let svc = OpenServiceW(scm, name_w.as_ptr(), DELETE | SERVICE_QUERY_STATUS);
        let mut result = Ok(());
        if svc.is_null() {
            let code = util::last_error();
            if code != 1060 {
                result = Err(err("打开服务"));
            }
        } else {
            if DeleteService(svc) == 0 {
                let code = util::last_error();
                if code != 1072 {
                    result = Err(format!("删除服务失败：错误码 {code}{}", describe_error(code)));
                }
            }
            let _ = CloseServiceHandle(svc);
        }
        let _ = CloseServiceHandle(scm);
        result
    }
}

/// 停止 + 删除（不存在也算成功）
pub fn remove(name: &str) -> Result<(), String> {
    if service_state(name).is_none() {
        return Ok(());
    }
    let _ = stop(name);
    delete(name)
}
