//! BJUTNet 服务主程序。
//!
//! 用法：
//!   BJUTNetSvc.exe --service     由 SCM 启动（手动执行会退化成前台模式，方便自检）
//!   BJUTNetSvc.exe --console     前台运行 + 控制台日志（开发/排障用）
//!   BJUTNetSvc.exe --probe       一次性诊断：网卡 / 出口 / SSID / 出网 / 门户判别 / loadConfig
//!   BJUTNetSvc.exe --login-once  判别 + 真实登录一次（排障用，失败会打印门户原始响应）
//!   BJUTNetSvc.exe --logout      注销（宿舍门户；参数见方案 §9.5）
//!   BJUTNetSvc.exe --set-credentials <学号>  写入凭据（密码从 stdin 读一行）
//!   BJUTNetSvc.exe --install     注册为 Windows 服务（管理员）
//!   BJUTNetSvc.exe --uninstall   停止并删除服务（管理员）
//!   BJUTNetSvc.exe --version
//!
//! 通用参数：
//!   --data <dir>   覆盖数据目录（默认 %ProgramData%\BJUTNet），开发时指向可写目录
//!   --debug        打开 DEBUG 日志

mod diag;
mod install;
mod ipc_server;
mod once;
mod probe;
mod service;
mod state;
mod worker;

use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

use cn_core::ipc::protocol::Status;
use cn_core::logging::{self, Level};
use cn_core::{paths, version};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |f: &str| args.iter().any(|a| a.eq_ignore_ascii_case(f));

    if let Some(dir) = arg_value(&args, "--data") {
        std::env::set_var(paths::ENV_DATA_DIR, dir);
    }

    if has("--version") || has("-v") {
        println!("BJUTNetSvc {}", version::full());
        return;
    }

    if has("--probe") {
        probe::run();
        return;
    }

    if has("--login-once") {
        std::process::exit(once::login_once());
    }
    if has("--logout") {
        std::process::exit(once::logout());
    }
    if let Some(account) = arg_value(&args, "--set-credentials") {
        std::process::exit(once::set_credentials(&account));
    }

    // 通用 RPC：给自动化测试/排障用，参数是一整条 Request JSON
    if let Some(raw) = arg_value(&args, "--rpc") {
        // `--rpc -` 从 stdin 读整条 JSON（避免密码出现在命令行/进程列表里）
        let raw = if raw.trim() == "-" {
            use std::io::Read;
            let mut buf = String::new();
            let _ = std::io::stdin().read_to_string(&mut buf);
            buf
        } else {
            raw
        };
        std::process::exit(rpc(&raw));
    }

    if has("--ping") {
        std::process::exit(ipc_selftest("ping"));
    }
    if has("--status") {
        std::process::exit(ipc_selftest("status"));
    }
    if has("--logs") {
        std::process::exit(ipc_selftest("get_logs"));
    }

    if has("--install") {
        std::process::exit(install::install());
    }
    if has("--uninstall") {
        std::process::exit(install::uninstall());
    }

    let debug = has("--debug");

    // IPC：面板（GUI）通过命名管道读状态、发指令；面板不运行也完全不影响服务
    let status = Arc::new(Mutex::new(Status::default()));
    let (cmd_tx, cmd_rx) = mpsc::channel::<ipc_server::Command>();
    let started = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let ipc_ctx = ipc_server::Ctx {
        status: status.clone(),
        cmd_tx,
        started,
    };
    let deps = worker::Deps {
        status,
        cmd_rx,
        started,
        stop,
    };

    if has("--service") {
        service::run_as_service(debug, ipc_ctx, deps);
    } else {
        // 不带参数直接双击也按 --console 跑（比什么都不发生更容易发现）
        service::run_console(debug, ipc_ctx, deps);
    }
}

fn arg_value(args: &[String], key: &str) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a.eq_ignore_ascii_case(key) {
            return it.next().cloned();
        }
        if let Some(rest) = a.strip_prefix(&format!("{key}=")) {
            return Some(rest.to_string());
        }
    }
    None
}

/// 发一条任意 IPC 请求（JSON），把响应原样打成一行 JSON
fn rpc(raw: &str) -> i32 {
    let req = match serde_json::from_str::<cn_core::ipc::protocol::Request>(raw) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("请求 JSON 解析失败: {e}");
            return 3;
        }
    };
    // 诊断要做门户探测，默认 3 秒不够
    let timeout = if req.cmd == "get_diag" {
        std::time::Duration::from_secs(30)
    } else {
        std::time::Duration::from_secs(3)
    };
    match cn_core::ipc::call_timeout(&req, timeout) {
        Ok(resp) => {
            let out = serde_json::json!({
                "ok": resp.ok,
                "error": resp.error,
                "data": resp.data,
            });
            println!("{}", serde_json::to_string(&out).unwrap_or_default());
            if resp.ok { 0 } else { 2 }
        }
        Err(e) => {
            eprintln!("连不上服务: {e}");
            1
        }
    }
}

/// IPC 自检：连正在运行的服务（--console 或已安装的服务），发一条命令看响应。
/// 面板还没写之前，用这个验证管道通不通。
fn ipc_selftest(cmd: &str) -> i32 {
    let req = cn_core::ipc::protocol::Request::new(cmd);
    match cn_core::ipc::call(&req) {
        Ok(resp) if resp.ok => {
            println!("✓ {cmd} 成功");
            match cmd {
                "status" => println!("{}", serde_json::to_string_pretty(&resp.data).unwrap_or_default()),
                "get_logs" => {
                    if let Some(lines) = resp.data.get("lines").and_then(|v| v.as_array()) {
                        for l in lines.iter().rev().take(15).rev() {
                            println!("{}", l.as_str().unwrap_or(""));
                        }
                    }
                }
                _ => println!("{}", resp.data),
            }
            0
        }
        Ok(resp) => {
            eprintln!("✗ {cmd} 被服务拒绝: {}", resp.error);
            2
        }
        Err(e) => {
            eprintln!("✗ 连不上服务: {e}");
            eprintln!("  提示：先把服务跑起来（BJUTNetSvc.exe --console），或检查服务是否已安装");
            1
        }
    }
}

/// 日志初始化：失败也不能让程序起不来
pub fn init_logging(echo: bool, debug: bool) {
    let level = if debug { Level::Debug } else { Level::Info };
    match logging::init(&paths::logs_dir(), echo, level) {
        Ok(p) => {
            if echo {
                println!("日志: {}", p.display());
            }
            if debug {
                // --debug 优先于 config.json 里的 logLevel
                logging::force_debug();
            }
        }
        Err(e) => eprintln!("日志初始化失败（继续运行，仅控制台输出）: {e}"),
    }
}
