//! 服务侧的计费查询：复用校园网账号密码（DPAPI 已存），只读。
//!
//! 两个用途：
//! 1. 面板「计费」页刷新（IPC `billing_refresh`）；
//! 2. 开机后一次性自动查询，欠费则弹窗（用户需在面板里显式打开该开关）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cn_core::billing::{BillingClient, BillingData, BillingError};
use cn_core::config::Config;
use cn_core::ipc::protocol::Status;
use cn_core::secret::Credentials;
use cn_core::logging;

pub const TIMEOUT: Duration = Duration::from_secs(25);

/// 等待用户输入验证码的会话（一次性）
pub type Pending = Arc<Mutex<Option<BillingClient>>>;

/// 查询（面板刷新 / 开机自检共用）。
///
/// 验证码流程要跨两次 IPC：第一次返回 `NeedCaptcha` 并把会话留在 `pending`，
/// 用户输入后第二次带 `code` 复用同一个会话提交。
pub fn refresh(
    pending: &Pending,
    _cfg: &Config,
    creds: Option<&Credentials>,
    code: Option<&str>,
) -> Result<BillingData, BillingError> {
    let c = creds.ok_or_else(|| BillingError::Auth("尚未配置学号密码".into()))?;
    let account = c.account.split('@').next().unwrap_or(&c.account).to_string();

    if let Some(code) = code {
        let mut client = match pending.lock().ok().and_then(|mut g| g.take()) {
            Some(c) => c,
            None => {
                // 会话已过期：重新开一个，重新要验证码
                let mut fresh = BillingClient::new(TIMEOUT);
                let _ = fresh.begin();
                let image = fresh.captcha().unwrap_or_default();
                if let Ok(mut g) = pending.lock() {
                    *g = Some(fresh);
                }
                return Err(BillingError::NeedCaptcha { image });
            }
        };
        match client.login(&account, &c.password, Some(code)) {
            Ok(_) => client.snapshot(),
            Err(BillingError::NeedCaptcha { .. }) | Err(BillingError::Auth(_)) => {
                // 验证码错/会话失效：重新开一份干净的会话和图片，避免用户对着旧图反复输
                let (next, image) = fresh_session();
                if let Some(c2) = next {
                    if let Ok(mut g) = pending.lock() {
                        *g = Some(c2);
                    }
                }
                Err(BillingError::NeedCaptcha { image })
            }
            Err(e) => Err(e),
        }
    } else {
        let mut client = BillingClient::new(TIMEOUT);
        let already = client.begin()?;
        if already {
            return client.snapshot();
        }
        let _image = client.captcha().unwrap_or_default();
        match client.login(&account, &c.password, None) {
            Ok(true) => client.snapshot(),
            Ok(false) | Err(BillingError::NeedCaptcha { .. }) | Err(BillingError::Auth(_)) => {
                // 不带验证码的尝试失败后，旧图片/会话可能已失效：重新拿一份
                let (next, image) = fresh_session();
                if let Some(c2) = next {
                    if let Ok(mut g) = pending.lock() {
                        *g = Some(c2);
                    }
                }
                Err(BillingError::NeedCaptcha { image })
            }
            Err(e) => Err(e),
        }
    }
}

/// 开机后等网络/登录就绪：
/// - 余额为 0 → 弹窗（计费系统最低就是 0，0 即视为停机/欠费）
/// - 可选：仍无法出网 → 弹窗提醒
pub fn spawn_auto_check(
    status: Arc<Mutex<Status>>,
    cfg: Arc<Mutex<Config>>,
    creds: Arc<Mutex<Option<Credentials>>>,
    cache: Arc<Mutex<Option<BillingData>>>,
    pending: Pending,
    stop: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let started = Instant::now();
        loop {
            if stop.load(Ordering::Acquire) {
                return;
            }
            let (auto, only_offline) = {
                let c = cfg.lock().ok().map(|g| g.clone()).unwrap_or_default();
                (c.billing_auto_check, c.billing_popup_only_offline)
            };
            if !auto {
                return; // 主开关关掉就不做任何查询/提醒
            }
            let _ = only_offline;
            let st = status.lock().ok().map(|g| g.clone()).unwrap_or_default();
            // 已经配置且有网络结论（在线），或者等够 90 秒（用于"无法出网"判断）
            if st.configured && (st.online || started.elapsed() >= Duration::from_secs(90)) {
                break;
            }
            if started.elapsed() >= Duration::from_secs(240) {
                return;
            }
            std::thread::sleep(Duration::from_secs(2));
        }

        let st = status.lock().ok().map(|g| g.clone()).unwrap_or_default();
        let cfg_now = cfg.lock().ok().map(|g| g.clone()).unwrap_or_default();
        let creds_now = creds.lock().ok().and_then(|g| g.clone());

        if !cfg_now.billing_auto_check {
            return;
        }
        // 可选子条件：仅当无法出网时才弹（对下面两种提醒都生效）
        if cfg_now.billing_popup_only_offline && st.online {
            logging::info("当前可出网：按设置不弹窗（仅无法出网时提醒）");
            return;
        }
        let Some(creds_now) = creds_now else {
            return;
        };

        logging::info("开机自动查询计费信息…");
        let data = match refresh(&pending, &cfg_now, Some(&creds_now), None) {
            Ok(d) => {
                if let Ok(mut g) = cache.lock() {
                    *g = Some(d.clone());
                }
                logging::info(format!(
                    "余额 {:.2} 元 / 剩余流量 {:.0} MB / 套餐价 {}",
                    d.balance_yuan,
                    d.left_mb,
                    d.package_price_yuan
                        .map(|p| format!("¥{p:.2}"))
                        .unwrap_or_else(|| "未知".into())
                ));
                Some(d)
            }
            Err(BillingError::NeedCaptcha { .. }) => {
                logging::warn("自动查询需要验证码，已跳过（可在面板「计费」页手动查询）");
                None
            }
            Err(e) => {
                logging::warn(format!("自动查询失败: {}", e.text()));
                None
            }
        };
        let Some(d) = data else { return };

        // 取最新在线状态（查询期间可能已经掉线）
        let st = status.lock().ok().map(|g| g.clone()).unwrap_or_default();
        if cfg_now.billing_popup_only_offline && st.online {
            logging::info("当前已可出网，不弹窗");
            return;
        }

        let mut lines: Vec<String> = Vec::new();
        if d.flow_below(cfg_now.billing_flow_alert_mb) {
            lines.push(format!(
                "· 当月剩余流量 {:.0} MB（约 {:.2} GB），低于设定的 {} MB",
                d.left_mb,
                d.left_mb / 1024.0,
                cfg_now.billing_flow_alert_mb
            ));
        }
        if d.balance_below_package() {
            lines.push(format!(
                "· 余额 ¥{:.2}，低于当前套餐价 ¥{:.2}",
                d.balance_yuan,
                d.package_price_yuan.unwrap_or(0.0)
            ));
        }
        if lines.is_empty() {
            logging::info("余额与流量均未触发提醒阈值");
            return;
        }
        logging::warn(format!("触发提醒：{}", lines.join("；")));
        popup_alerts(&st, &lines);
    });
}

/// 新建一份"已打开登录页并取到验证码"的会话
fn fresh_session() -> (Option<BillingClient>, Vec<u8>) {
    let mut c = BillingClient::new(TIMEOUT);
    if c.begin().unwrap_or(false) {
        return (Some(c), Vec::new());
    }
    let image = c.captcha().unwrap_or_default();
    (Some(c), image)
}

/// 手动预览三种弹窗（`BJUTNetSvc.exe --demo-popups`）
pub fn demo_popups() {
    let st = Status::default();
    let base = BillingData {
        balance_yuan: 15.0,
        used_mb: 118000.0,
        left_mb: 512.0,
        used_time_min: 3600,
        package_price_yuan: Some(20.0),
        fetched_epoch: chrono::Local::now().timestamp(),
    };
    popup_alerts(
        &st,
        &[format!(
            "· 当月剩余流量 {:.0} MB（约 {:.2} GB），低于设定的 1024 MB",
            base.left_mb,
            base.left_mb / 1024.0
        )],
    );
    popup_alerts(
        &st,
        &[format!(
            "· 余额 ¥{:.2}，低于当前套餐价 ¥{:.2}",
            base.balance_yuan,
            base.package_price_yuan.unwrap_or(0.0)
        )],
    );
    let mut offline = st.clone();
    offline.online = false;
    offline.ip = "10.126.83.10".into();
    offline.iface = "WLAN".into();
    popup_alerts(
        &offline,
        &[
            "· 当月剩余流量 512 MB（约 0.50 GB），低于设定的 1024 MB".to_string(),
            "· 余额 ¥15.00，低于当前套餐价 ¥20.00".to_string(),
        ],
    );
}

/// 组合提醒弹窗（流量/余额），附当前出网与出口信息
fn popup_alerts(st: &Status, lines: &[String]) {
    let iface = if st.iface.is_empty() { "未知" } else { &st.iface };
    let ip = if st.ip.is_empty() { "无" } else { &st.ip };
    let mut text = String::from("BJUTNet 校园网提醒：\n\n");
    if !st.online {
        text.push_str("当前无法访问外网。\n\n");
    }
    text.push_str(&lines.join("\n"));
    text.push_str(&format!(
        "\n\n当前出口：{}   IP：{}\n请及时充值 / 关注流量使用。\n（本提醒由服务在开机时查询一次）",
        iface, ip
    ));
    message_box("BJUTNet 校园网提醒", &text);
}

/// 在交互式桌面弹一个提醒框（服务在 session 0，用 MB_SERVICE_NOTIFICATION）
fn message_box(caption: &str, text: &str) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(
            hwnd: *mut core::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            ty: u32,
        ) -> i32;
    }
    const MB_OK: u32 = 0x0;
    const MB_ICONWARNING: u32 = 0x30;
    const MB_SERVICE_NOTIFICATION: u32 = 0x0020_0000;
    const MB_SETFOREGROUND: u32 = 0x0001_0000;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcessId() -> u32;
        fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
    }
    // 只有 session 0 的服务才需要 MB_SERVICE_NOTIFICATION；
    // 普通进程（--demo-popups）用它会让窗口挂到 csrss 名下，关不掉。
    let mut sid: u32 = 0;
    let ok = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut sid) } != 0;
    let service_flag = if ok && sid == 0 {
        MB_SERVICE_NOTIFICATION | MB_SETFOREGROUND
    } else {
        0
    };

    let wide = |s: &str| -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let text_w = wide(text);
    let caption_w = wide(caption);
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text_w.as_ptr(),
            caption_w.as_ptr(),
            MB_OK | MB_ICONWARNING | service_flag,
        );
    }
}
