//! IPC 服务端：命名管道 + 命令分发。
//!
//! 只做三件事：
//! 1. 读请求 → 分派：只读命令（status/get_logs/get_diag）当场处理；
//! 2. 写命令（重连/暂停/恢复/改配置）转给 worker 线程（它才是唯一操作状态机的人）；
//! 3. `set_credentials` **必须在服务进程里落盘** —— 这样 DPAPI 密文绑的是服务身份，
//!    以后服务（SYSTEM）重启也能自己解密，不需要面板再跑一次。
//!
//! 这条正是"配置一次，之后服务独立自启+自登录、不需要面板"的关键。

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cn_core::config::Config;
use cn_core::ipc::protocol::{Request, Response, Status, PROTO};
use cn_core::ipc::win;
use cn_core::{logging, paths, secret, version};

/// 面板 → worker 的指令
#[derive(Debug, Clone)]
pub enum Command {
    Reconnect,
    /// Some(分钟) = 暂停 N 分钟；None = 暂停直到手动恢复
    Pause(Option<u64>),
    Resume,
    ReloadCredentials,
    ReloadConfig,
    /// 立即验证登录：先注销当前会话，再立刻走一轮登录（面板「重连并验证」）
    VerifyLogin,
    /// 关闭服务（本次运行；服务启动类型仍是自动，下次开机会回来）
    StopService,
}

/// 服务端共享上下文
pub struct Ctx {
    pub status: Arc<Mutex<Status>>,
    pub cmd_tx: Sender<Command>,
    pub started: Instant,
}

pub fn spawn(ctx: Arc<Ctx>) {
    std::thread::spawn(move || {
        logging::info(format!("IPC 管道服务已启动: {}", win::pipe_path()));
        loop {
            let handle = match win::create_server_pipe() {
                Ok(h) => h,
                Err(e) => {
                    logging::err(format!("创建管道失败: {e}（2 秒后重试）"));
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
            };
            if let Err(e) = win::accept(handle) {
                logging::debug(format!("等待客户端失败: {e}"));
                win::close(handle);
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
            logging::debug("面板已连接");
            serve_client(handle, &ctx);
            win::disconnect(handle);
            win::close(handle);
            logging::debug("面板已断开");
        }
    });
}

fn serve_client(handle: win::Handle, ctx: &Ctx) {
    loop {
        let frame = match win::read_frame(handle) {
            Ok(Some(f)) => f,
            Ok(None) => return,
            Err(e) => {
                logging::debug(format!("读请求失败: {e}"));
                return;
            }
        };

        let resp = match serde_json::from_slice::<Request>(&frame) {
            Ok(req) => {
                logging::debug(format!("IPC 请求: {}", req.cmd));
                dispatch(&req, ctx)
            }
            Err(e) => Response::err(format!("请求解析失败: {e}")),
        };

        let bytes = match serde_json::to_vec(&resp) {
            Ok(b) => b,
            Err(e) => {
                logging::err(format!("响应序列化失败: {e}"));
                return;
            }
        };
        if let Err(e) = win::write_frame(handle, &bytes) {
            logging::debug(format!("写响应失败: {e}"));
            return;
        }
        win::flush(handle);
    }
}

fn dispatch(req: &Request, ctx: &Ctx) -> Response {
    match req.cmd.as_str() {
        "ping" => Response::ok(serde_json::json!({
            "proto": PROTO,
            "svc_version": version::full(),
        })),
        "status" => {
            let st = ctx
                .status
                .lock()
                .map(|g| g.clone())
                .unwrap_or_default();
            Response::ok(serde_json::to_value(st).unwrap_or(serde_json::Value::Null))
        }
        "set_credentials" => set_credentials(req, ctx),
        "set_options" => set_options(req, ctx),
        "reconnect" => {
            let _ = ctx.cmd_tx.send(Command::Reconnect);
            Response::ok(serde_json::json!({"msg": "已触发立即重连"}))
        }
        "pause" => {
            let forever = req.bool("forever").unwrap_or(false);
            let minutes = req.u64("minutes").unwrap_or(30);
            if forever || minutes == 0 {
                let _ = ctx.cmd_tx.send(Command::Pause(None));
                Response::ok(serde_json::json!({"msg": "已暂停，直到手动恢复"}))
            } else {
                let minutes = minutes.max(1);
                let _ = ctx.cmd_tx.send(Command::Pause(Some(minutes)));
                Response::ok(serde_json::json!({"msg": format!("已暂停 {minutes} 分钟")}))
            }
        }
        "stop_service" => {
            let _ = ctx.cmd_tx.send(Command::StopService);
            Response::ok(serde_json::json!({"msg": "服务正在关闭（本次运行）"}))
        }
        "verify_login" => {
            let _ = ctx.cmd_tx.send(Command::VerifyLogin);
            Response::ok(serde_json::json!({
                "msg": "已注销当前会话并开始重新登录，几秒后看状态/最近错误"
            }))
        }
        "resume" => {
            let _ = ctx.cmd_tx.send(Command::Resume);
            Response::ok(serde_json::json!({"msg": "已恢复自动登录"}))
        }
        "get_logs" => {
            let tail = req.u64("tail").unwrap_or(500).clamp(10, 5000) as usize;
            let lines = logging::tail(tail);
            Response::ok(serde_json::json!({ "lines": lines }))
        }
        "add_trusted_current" => add_trusted_current(req, ctx),
        "remove_trusted" => remove_trusted(req),
        "get_diag" => Response::ok(serde_json::json!({
            "text": crate::diag::text(),
            "uptime_sec": ctx.started.elapsed().as_secs(),
        })),
        other => Response::err(format!("未知命令: {other}")),
    }
}

fn set_credentials(req: &Request, ctx: &Ctx) -> Response {
    let account = req.str("account").unwrap_or("").trim().to_string();
    let password = req.str("password").unwrap_or("").to_string();
    if account.is_empty() {
        return Response::err("学号不能为空");
    }
    if password.is_empty() {
        return Response::err("密码不能为空");
    }

    let creds = secret::Credentials::new(account.clone(), password);
    let path = paths::secret_path();
    if let Err(e) = secret::save(&path, &creds) {
        logging::err(format!("保存凭据失败: {e}"));
        return Response::err(format!("保存凭据失败: {e}"));
    }
    logging::info(format!(
        "面板已保存凭据（{}），由服务身份加密落盘",
        creds.masked_account()
    ));

    // 账号名同步进 config（密码不落 config）
    let cfg_path = paths::config_path();
    let (mut cfg, _) = Config::load_or_create(&cfg_path);
    if cfg.account != account {
        cfg.account = account.clone();
        if let Err(e) = cfg.save(&cfg_path) {
            logging::warn(format!("写 config.json 失败（不影响登录）: {e}"));
        }
    }

    // 通知 worker 立刻重载并验证一次
    let _ = ctx.cmd_tx.send(Command::ReloadCredentials);

    Response::ok(serde_json::json!({
        "saved": true,
        "account": creds.masked_account(),
        "path": paths::show(&path),
    }))
}

fn set_options(req: &Request, ctx: &Ctx) -> Response {
    let cfg_path = paths::config_path();
    let (mut cfg, _) = Config::load_or_create(&cfg_path);
    let mut changed = Vec::new();

    if let Some(v) = req.u64("checkIntervalSec") {
        cfg.check_interval_sec = v.clamp(5, 3600);
        changed.push("checkIntervalSec");
    }
    if let Some(v) = req.u64("offlineIntervalSec") {
        cfg.offline_interval_sec = v.clamp(2, 3600);
        changed.push("offlineIntervalSec");
    }
    if let Some(v) = req.u64("probeTimeoutSec") {
        cfg.probe_timeout_sec = v.clamp(1, 30);
        changed.push("probeTimeoutSec");
    }
    if let Some(v) = req.bool("autoConnectWifi") {
        cfg.auto_connect_wifi = v;
        changed.push("autoConnectWifi");
    }
    if let Some(v) = req.str("wifiProfile") {
        if !v.trim().is_empty() {
            cfg.wifi_profile = v.trim().to_string();
            changed.push("wifiProfile");
        }
    }
    if let Some(v) = req.str("logLevel") {
        let up = v.trim().to_ascii_uppercase();
        if ["ERR", "WARN", "INFO", "DEBUG"].contains(&up.as_str()) {
            cfg.log_level = up;
            changed.push("logLevel");
        }
    }
    if let Some(v) = req.str("accountSuffixFallback") {
        cfg.account_suffix_fallback = v.trim().to_string();
        changed.push("accountSuffixFallback");
    }

    if changed.is_empty() {
        return Response::err("没有可保存的选项字段");
    }

    if let Err(e) = cfg.save(&cfg_path) {
        logging::err(format!("保存选项失败: {e}"));
        return Response::err(format!("保存选项失败: {e}"));
    }
    logging::info(format!("面板已更新选项: {}", changed.join(", ")));
    let _ = ctx.cmd_tx.send(Command::ReloadConfig);

    Response::ok(serde_json::json!({ "changed": changed }))
}

/// 把"当前网络"加入用户标记的校园网列表
fn add_trusted_current(req: &Request, ctx: &Ctx) -> Response {
    let kind = match req.str("kind").unwrap_or("dorm").to_ascii_lowercase().as_str() {
        "public" | "wifi" | "无线" => "public",
        _ => "dorm",
    };

    let st = ctx.status.lock().map(|g| g.clone()).unwrap_or_default();
    if st.ip.is_empty() && st.ssid.is_empty() && st.gateway.is_empty() {
        return Response::err("还没识别到当前网络（没有 IP/SSID/网关），稍等再试");
    }

    // 网段：ip + prefix → 10.126.64.0/19
    let subnet = if !st.ip.is_empty() && st.prefix_len > 0 {
        cn_core::net::subnet_of(&st.ip, st.prefix_len)
    } else {
        String::new()
    };

    let mut entry = cn_core::config::TrustedNet {
        kind: kind.to_string(),
        name: String::new(),
        ssid: if st.ssid.is_empty() { String::new() } else { st.ssid.clone() },
        gateway: st.gateway.clone(),
        subnet,
    };
    entry.name = format!(
        "{}（{}）",
        if entry.is_public() { "公共无线" } else { "宿舍有线" },
        if !entry.ssid.is_empty() {
            format!("SSID={}", entry.ssid)
        } else if !entry.gateway.is_empty() {
            format!("网关={}", entry.gateway)
        } else {
            format!("网段={}", entry.subnet)
        }
    );

    let cfg_path = paths::config_path();
    let (mut cfg, _) = Config::load_or_create(&cfg_path);

    // 去重：已经有一条能匹配同一网络就不再加
    if let Some(existing) = cfg.trusted_match(Some(&entry.ssid), Some(&st.ip), &entry.gateway) {
        return Response::ok(serde_json::json!({
            "added": false,
            "entry": existing,
            "msg": "当前网络已经在标记列表里了",
        }));
    }

    cfg.trusted_networks.push(entry.clone());
    if let Err(e) = cfg.save(&cfg_path) {
        return Response::err(format!("保存配置失败: {e}"));
    }
    logging::info(format!("面板标记当前网络为校园网: {}", entry.describe()));
    let _ = ctx.cmd_tx.send(Command::ReloadConfig);
    Response::ok(serde_json::json!({ "added": true, "entry": entry }))
}

/// 删除第 index 条标记
fn remove_trusted(req: &Request) -> Response {
    let Some(idx) = req.u64("index") else {
        return Response::err("缺少 index");
    };
    let cfg_path = paths::config_path();
    let (mut cfg, _) = Config::load_or_create(&cfg_path);
    let i = idx as usize;
    if i >= cfg.trusted_networks.len() {
        return Response::err("编号超出范围");
    }
    let removed = cfg.trusted_networks.remove(i);
    if let Err(e) = cfg.save(&cfg_path) {
        return Response::err(format!("保存配置失败: {e}"));
    }
    logging::info(format!("面板移除了校园网标记: {}", removed.describe()));
    // 注意：这里没有 ctx，但保存后 worker 的 mtime 轮询会自动重载；为了让 UI 立刻刷新，
    // 调用方（面板）可以紧接着点「立即重连」或等下一轮 200ms 热重载。
    Response::ok(serde_json::json!({ "removed": removed }))
}
