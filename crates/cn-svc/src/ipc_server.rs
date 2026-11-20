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
    /// 当前配置/凭据（与 worker 共享）
    pub shared_cfg: Arc<Mutex<Config>>,
    pub shared_creds: Arc<Mutex<Option<secret::Credentials>>>,
    /// 最近一次计费查询结果
    pub billing: Arc<Mutex<Option<cn_core::billing::BillingData>>>,
    /// 等待验证码的计费会话
    pub billing_pending: crate::billing::Pending,
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
            // 每个连接单独一个线程，主循环立刻回去建下一个管道实例。
            // 否则一个连接没断开时，另一个请求会 "系统找不到指定的文件"（Win32 2）。
            let conn_ctx = ctx.clone();
            // HANDLE 是裸指针（不是 Send），转成整数带进线程再还原
            let raw = handle.0 as usize;
            std::thread::spawn(move || {
                let handle = win::Handle(raw as *mut core::ffi::c_void);
                serve_client(handle, &conn_ctx);
                win::disconnect(handle);
                win::close(handle);
                logging::debug("面板已断开");
            });
        }
    });
}

/// 生产管道只信任同目录下的 BJUTNet.exe（面板）；测试用自定义管道名时放行
fn client_trusted(path: Option<&std::path::Path>) -> bool {
    let test_pipe = std::env::var("CAMPUSNET_PIPE")
        .map(|v| {
            let v = v.trim().to_string();
            !v.is_empty() && v != cn_core::ipc::win::DEFAULT_PIPE
        })
        .unwrap_or(false);
    if test_pipe {
        return true;
    }
    let (Some(path), Ok(svc)) = (path, std::env::current_exe()) else {
        return false;
    };
    let (Some(dir), Some(svc_dir)) = (path.parent(), svc.parent()) else {
        return false;
    };
    dir == svc_dir
        && path
            .file_name()
            .map(|n| n.eq_ignore_ascii_case("BJUTNet.exe"))
            .unwrap_or(false)
}

/// 会改状态/凭据/配置的命令，只允许面板调用
fn sensitive(cmd: &str) -> bool {
    matches!(
        cmd,
        "set_credentials"
            | "set_options"
            | "reset_options"
            | "reconnect"
            | "pause"
            | "resume"
            | "verify_login"
            | "add_trusted_current"
            | "remove_trusted"
            | "stop_service"
            | "billing_refresh"
            | "billing_check_now"
            | "get_config"
    )
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
                let client_path = win::client_exe_path(handle);
                let trusted = client_trusted(client_path.as_deref());
                if sensitive(&req.cmd) && !trusted {
                    logging::warn(format!(
                        "拒绝 IPC 写命令 {}：调用方 {} 不是本目录面板",
                        req.cmd,
                        client_path
                            .as_deref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "<unknown>".into())
                    ));
                    Response::err("该操作只能从 BJUTNet 面板执行")
                } else {
                    dispatch(&req, ctx)
                }
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
            let st = ctx.status.lock().map(|g| g.clone()).unwrap_or_default();
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
        "get_config" => get_config(ctx),
        "reset_options" => reset_options(req, ctx),
        "billing_status" => billing_status(ctx),
        "billing_refresh" => billing_refresh(req, ctx),
        "billing_check_now" => billing_check_now(req, ctx),
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
    if let Some(v) = req.bool("billingAutoCheck") {
        cfg.billing_auto_check = v;
        changed.push("billingAutoCheck");
    }
    if let Some(v) = req.bool("billingHideAmount") {
        cfg.billing_hide_amount = v;
        changed.push("billingHideAmount");
    }
    if let Some(v) = req.bool("billingFlowAlert") {
        cfg.billing_flow_alert = v;
        changed.push("billingFlowAlert");
    }
    if let Some(v) = req.u64("billingFlowAlertMb") {
        cfg.billing_flow_alert_mb = v.min(10_000_000);
        changed.push("billingFlowAlertMb");
    }
    if let Some(v) = req.bool("billingFlowOnlyOffline") {
        cfg.billing_flow_only_offline = v;
        changed.push("billingFlowOnlyOffline");
    }
    if let Some(v) = req.bool("billingBalanceAlert") {
        cfg.billing_balance_alert = v;
        changed.push("billingBalanceAlert");
    }
    if let Some(v) = req.bool("billingBalanceOnlyOffline") {
        cfg.billing_balance_only_offline = v;
        changed.push("billingBalanceOnlyOffline");
    }
    if let Some(v) = req.bool("monthOff") {
        cfg.no_connect_until_epoch = if v {
            cn_core::config::next_month_start_epoch()
        } else {
            0
        };
        changed.push("monthOff");
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
    // 立刻更新共享快照：面板保存后会马上 get_config，
    // 不能等 worker 下一次 tick（tick 间隔可能 30~60s，用户会看到"配置恢复默认"）
    if let Ok(mut g) = ctx.shared_cfg.lock() {
        *g = cfg.clone();
    }
    logging::info(format!("面板已更新选项: {}", changed.join(", ")));
    let _ = ctx.cmd_tx.send(Command::ReloadConfig);

    Response::ok(serde_json::json!({ "changed": changed }))
}

/// 把"当前网络"加入用户标记的校园网列表
fn add_trusted_current(req: &Request, ctx: &Ctx) -> Response {
    let kind = match req
        .str("kind")
        .unwrap_or("dorm")
        .to_ascii_lowercase()
        .as_str()
    {
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
        ssid: if st.ssid.is_empty() {
            String::new()
        } else {
            st.ssid.clone()
        },
        gateway: st.gateway.clone(),
        subnet,
    };
    entry.name = format!(
        "{}（{}）",
        if entry.is_public() {
            "公共网络"
        } else {
            "宿舍网络"
        },
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

fn billing_status(ctx: &Ctx) -> Response {
    let cfg = ctx.shared_cfg.lock().map(|g| g.clone()).unwrap_or_default();
    let configured = ctx
        .shared_creds
        .lock()
        .map(|g| g.is_some())
        .unwrap_or(false);
    let data = ctx.billing.lock().ok().and_then(|g| g.clone());
    Response::ok(serde_json::json!({
        "configured": configured,
        "auto_check": cfg.billing_auto_check,
        "hide_amount": cfg.billing_hide_amount,
        "flow_alert": cfg.billing_flow_alert,
        "flow_alert_mb": cfg.billing_flow_alert_mb,
        "flow_only_offline": cfg.billing_flow_only_offline,
        "balance_alert": cfg.billing_balance_alert,
        "balance_only_offline": cfg.billing_balance_only_offline,
        "data": data,
    }))
}

fn billing_refresh(req: &Request, ctx: &Ctx) -> Response {
    let cfg = ctx.shared_cfg.lock().map(|g| g.clone()).unwrap_or_default();
    let creds = ctx.shared_creds.lock().ok().and_then(|g| g.clone());
    let code = req.str("code").filter(|s| !s.trim().is_empty());
    match crate::billing::refresh(&ctx.billing_pending, &cfg, creds.as_ref(), code) {
        Ok(d) => {
            if let Ok(mut g) = ctx.billing.lock() {
                *g = Some(d.clone());
            }
            Response::ok(serde_json::json!({
                "need_captcha": false,
                "data": d,
            }))
        }
        Err(cn_core::billing::BillingError::NeedCaptcha { image }) => {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&image);
            Response::ok(serde_json::json!({
                "need_captcha": true,
                "captcha_png": b64,
            }))
        }
        Err(e) => Response::err(e.text()),
    }
}

fn get_config(ctx: &Ctx) -> Response {
    let cfg = ctx.shared_cfg.lock().map(|g| g.clone()).unwrap_or_default();
    let now = chrono::Local::now().timestamp();
    Response::ok(serde_json::json!({
        "account": cfg.account,
        "checkIntervalSec": cfg.check_interval_sec,
        "offlineIntervalSec": cfg.offline_interval_sec,
        "probeTimeoutSec": cfg.probe_timeout_sec,
        "logLevel": cfg.log_level,
        "autoConnectWifi": cfg.auto_connect_wifi,
        "wifiProfile": cfg.wifi_profile,
        "accountSuffixFallback": cfg.account_suffix_fallback,
        "billingAutoCheck": cfg.billing_auto_check,
        "billingHideAmount": cfg.billing_hide_amount,
        "billingFlowAlert": cfg.billing_flow_alert,
        "billingFlowAlertMb": cfg.billing_flow_alert_mb,
        "billingFlowOnlyOffline": cfg.billing_flow_only_offline,
        "billingBalanceAlert": cfg.billing_balance_alert,
        "billingBalanceOnlyOffline": cfg.billing_balance_only_offline,
        "monthOff": cfg.no_connect_until_epoch > now,
        "monthOffUntilEpoch": cfg.no_connect_until_epoch,
    }))
}

fn reset_options(req: &Request, ctx: &Ctx) -> Response {
    let scope = req.str("scope").unwrap_or("").trim().to_string();
    let cfg_path = paths::config_path();
    let (mut cfg, _) = Config::load_or_create(&cfg_path);
    let d = Config::default();

    match scope.as_str() {
        "settings" => {
            // 只重置“运行参数”，不动账号/密码、标记网络、宿舍/公共门户地址
            cfg.check_interval_sec = d.check_interval_sec;
            cfg.offline_interval_sec = d.offline_interval_sec;
            cfg.probe_timeout_sec = d.probe_timeout_sec;
            cfg.log_level = d.log_level;
            cfg.auto_connect_wifi = d.auto_connect_wifi;
            cfg.wifi_profile = d.wifi_profile;
            cfg.account_suffix_fallback = d.account_suffix_fallback;
            cfg.probe_urls = d.probe_urls;
            cfg.backoff = d.backoff;
            cfg.max_password_fails = d.max_password_fails;
            cfg.pause_minutes = d.pause_minutes;
            cfg.no_connect_until_epoch = 0;
        }
        "billing" => {
            cfg.billing_auto_check = d.billing_auto_check;
            cfg.billing_hide_amount = d.billing_hide_amount;
            cfg.billing_flow_alert = d.billing_flow_alert;
            cfg.billing_flow_alert_mb = d.billing_flow_alert_mb;
            cfg.billing_flow_only_offline = d.billing_flow_only_offline;
            cfg.billing_balance_alert = d.billing_balance_alert;
            cfg.billing_balance_only_offline = d.billing_balance_only_offline;
        }
        _ => return Response::err("未知的重置范围（settings / billing）"),
    }

    if let Err(e) = cfg.save(&cfg_path) {
        logging::err(format!("恢复默认失败: {e}"));
        return Response::err(format!("恢复默认失败: {e}"));
    }
    if let Ok(mut g) = ctx.shared_cfg.lock() {
        *g = cfg.clone();
    }
    logging::info(format!("面板已恢复默认设置: {scope}"));
    let _ = ctx.cmd_tx.send(Command::ReloadConfig);
    Response::ok(serde_json::json!({ "scope": scope }))
}

fn billing_check_now(req: &Request, ctx: &Ctx) -> Response {
    let cfg = ctx.shared_cfg.lock().map(|g| g.clone()).unwrap_or_default();
    let creds = ctx.shared_creds.lock().ok().and_then(|g| g.clone());
    let code = req.str("code").filter(|s| !s.trim().is_empty());
    match crate::billing::refresh(&ctx.billing_pending, &cfg, creds.as_ref(), code) {
        Ok(d) => {
            if let Ok(mut g) = ctx.billing.lock() {
                *g = Some(d.clone());
            }
            let st = ctx.status.lock().map(|g| g.clone()).unwrap_or_default();
            let lines = crate::billing::alert_lines(&cfg, &d, st.online);
            if lines.is_empty() {
                return Response::ok(serde_json::json!({
                    "popped": false,
                    "data": d,
                    "reason": format!(
                        "条件未命中：余额 ¥{:.2}（套餐 ¥{}），剩余流量 {:.0} MB（阈值 {} MB）",
                        d.balance_yuan,
                        d.package_price_yuan.map(|p| format!("{p:.2}")).unwrap_or_else(|| "?".into()),
                        d.left_mb,
                        cfg.billing_flow_alert_mb
                    ),
                }));
            }
            let st2 = st.clone();
            let lines2 = lines.clone();
            std::thread::spawn(move || crate::billing::popup_alerts(&st2, &lines2));
            Response::ok(serde_json::json!({
                "popped": true,
                "lines": lines,
                "data": d,
            }))
        }
        Err(cn_core::billing::BillingError::NeedCaptcha { image }) => {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&image);
            Response::ok(serde_json::json!({
                "need_captcha": true,
                "captcha_png": b64,
            }))
        }
        Err(e) => Response::err(e.text()),
    }
}
