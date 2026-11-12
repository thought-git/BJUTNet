//! 主循环（M3 版本）：在 M2 的"判别→登录→自愈"之上，接上 IPC。
//!
//! 分工很明确：
//! - **worker 线程**是唯一操作状态机的人（登录、退避、暂停都只在这里发生）；
//! - **IPC 线程**只读状态快照、把写命令塞进 channel；
//! - 面板（GUI）可有可无：它关了，worker 照常跑 —— 这正是"配置一次，服务独立自启自登录"的前提。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use cn_core::config::{Config, LoadOutcome};
use cn_core::http::{self, Verdict};
use cn_core::ipc::protocol::{Status, PROTO};
use cn_core::logging;
use cn_core::net::{self, Adapter};
use cn_core::paths;
use cn_core::portal::{self, LoginOutcome, NetworkKind};
use cn_core::secret::{self, Credentials};
use cn_core::version;
use cn_core::wlan;

use crate::ipc_server::Command;
use crate::state::{Machine, State};

const HEARTBEAT: Duration = Duration::from_secs(300);
const DETECT_CACHE_TTL: Duration = Duration::from_secs(24 * 3600);

/// worker 需要的外部依赖（由 main / service 组装）
pub struct Deps {
    pub status: Arc<Mutex<Status>>,
    pub cmd_rx: Receiver<Command>,
    pub started: Instant,
    /// 停止信号（SCM 停止 / 面板「关闭服务」）
    pub stop: Arc<AtomicBool>,
}

pub fn run_foreground(deps: Deps) {
    run(deps);
}

pub fn run_service(deps: Deps) {
    run(deps);
}

fn run(deps: Deps) {
    let stop = deps.stop.clone();
    let mut rt = match Runtime::new(deps) {
        Ok(rt) => rt,
        Err(e) => {
            logging::err(format!("初始化失败，服务无法继续: {e}"));
            return;
        }
    };
    rt.banner();
    rt.publish_status();

    loop {
        if is_stopping(&stop) {
            logging::info("收到停止信号，退出主循环");
            break;
        }

        let before_wall = SystemTime::now();
        let before_tick = Instant::now();

        rt.tick();

        rt.wait_next(&stop);

        // 睡眠恢复检测：墙上时钟比单调时钟多走 >5 s → 机器睡过，立刻重试
        let wall = before_wall.elapsed().unwrap_or_default();
        let monotonic = before_tick.elapsed();
        if wall.as_secs() > monotonic.as_secs() + 5 {
            logging::warn(format!(
                "检测到系统睡眠/挂起（墙钟 {}s vs 单调 {}s），立即重新探测",
                wall.as_secs(),
                monotonic.as_secs()
            ));
            rt.machine.force_now();
        }
    }

    let mut st = rt.status.lock().map(|g| g.clone()).unwrap_or_default();
    st.state = "已停止".to_string();
    st.uptime_sec = rt.started.elapsed().as_secs();
    if let Ok(mut g) = rt.status.lock() {
        *g = st;
    }
    logging::info("主循环结束");
}

fn is_stopping(stop: &Arc<AtomicBool>) -> bool {
    stop.load(Ordering::Acquire)
}

#[derive(Debug, Clone)]
struct EnvSnap {
    ip: String,
    mac: String,
    gateway: String,
    ssid: String,
    prefix_len: u8,
    kind: NetworkKind,
}

impl Default for EnvSnap {
    fn default() -> Self {
        Self {
            ip: String::new(),
            mac: String::new(),
            gateway: String::new(),
            ssid: String::new(),
            prefix_len: 0,
            kind: NetworkKind::None,
        }
    }
}

struct Runtime {
    cfg: Config,
    cfg_outcome: LoadOutcome,
    cfg_mtime: Option<SystemTime>,
    creds: Option<Credentials>,
    creds_mtime: Option<SystemTime>,
    machine: Machine,
    env: EnvSnap,
    /// 最近一次网络判别的依据（给面板显示）
    last_evidence: String,
    last_verdict: Verdict,
    last_sig: Option<String>,
    detect_cache: Option<(String, NetworkKind, Instant)>,
    last_heartbeat: Instant,
    adapters_err_logged: bool,
    ticks: u64,
    // IPC
    status: Arc<Mutex<Status>>,
    cmd_rx: Receiver<Command>,
    started: Instant,
    stop: Arc<AtomicBool>,
}

impl Runtime {
    fn new(deps: Deps) -> std::io::Result<Self> {
        paths::ensure_dir(&paths::data_dir())?;
        paths::ensure_dir(&paths::logs_dir())?;

        let cfg_path = paths::config_path();
        let (cfg, outcome) = Config::load_or_create(&cfg_path);
        logging::set_level(cfg.level());
        let cfg_mtime = file_mtime(&cfg_path);

        let secret_path = paths::secret_path();
        let creds_mtime = file_mtime(&secret_path);
        let creds = match secret::load(&secret_path) {
            Ok(c) => c,
            Err(e) => {
                logging::warn(format!("凭据读取失败（{e}），按未配置处理"));
                None
            }
        };

        if outcome != LoadOutcome::Loaded {
            logging::warn(format!("配置状态: {}", outcome.text()));
        }

        let mut machine = Machine::new();
        if creds.is_none() {
            machine.set_unconfigured();
        }

        Ok(Self {
            cfg,
            cfg_outcome: outcome,
            cfg_mtime,
            creds,
            creds_mtime,
            machine,
            env: EnvSnap::default(),
            last_evidence: String::new(),
            last_verdict: Verdict::Offline,
            last_sig: None,
            detect_cache: None,
            last_heartbeat: Instant::now(),
            adapters_err_logged: false,
            ticks: 0,
            status: deps.status,
            cmd_rx: deps.cmd_rx,
            started: deps.started,
            stop: deps.stop,
        })
    }

    fn banner(&mut self) {
        logging::info("==================================================");
        logging::info(format!("BJUTNetSvc {} 启动", cn_core::version::full()));
        logging::info(format!(
            "身份: {}  elevated={}",
            std::env::var("USERNAME").unwrap_or_else(|_| "?".into()),
            net::is_elevated()
        ));
        logging::info(format!("安装目录: {}", paths::show(&paths::install_dir())));
        logging::info(format!("数据目录: {}", paths::show(&paths::data_dir())));
        logging::info(format!(
            "配置: {}  account={}  suffixFallback={}",
            self.cfg_outcome.text(),
            if self.cfg.account.is_empty() {
                "<未配置>".to_string()
            } else {
                secret::mask(&self.cfg.account)
            },
            self.cfg.account_suffix_fallback
        ));
        logging::info(format!(
            "门户: 宿舍={}  公共={}",
            self.cfg.dorm.base, self.cfg.public.base
        ));
        logging::info(format!(
            "节奏: 在线 {}s / 离线 {}s，探测超时 {}s，退避 {:?}",
            self.cfg.online_interval().as_secs(),
            self.cfg.offline_interval().as_secs(),
            self.cfg.probe_timeout().as_secs(),
            self.cfg.backoff
        ));
        match &self.creds {
            Some(c) => logging::info(format!(
                "凭据: 已就绪（{}），服务可独立自动登录（面板不必运行）",
                c.masked_account()
            )),
            None => logging::warn(format!(
                "凭据: 尚未配置（{} 不存在或解不开），等面板写入",
                paths::show(&paths::secret_path())
            )),
        }
        self.log_environment();
        logging::info("==================================================");
    }

    fn log_environment(&mut self) {
        let adapters = match net::list_adapters() {
            Ok(v) => v,
            Err(e) => {
                logging::err(format!("网卡枚举失败: {e}"));
                return;
            }
        };
        let active = net::active_adapter(&adapters);
        let ssid = wlan::current_ssid();
        match &active {
            Some(a) => {
                logging::info(format!("活跃出口: {}", a.summary()));
                if let (Some(ip), Some(gw)) = (a.ipv4_first(), a.gateway.parse().ok()) {
                    logging::info(format!(
                        "  子网: prefix={} 同子网={}",
                        a.prefix_len,
                        net::same_subnet(ip, gw, a.prefix_len)
                    ));
                }
            }
            None => logging::warn("活跃出口: 未找到（全部断开或只有虚拟网卡）"),
        }
        logging::info(format!(
            "SSID: {}",
            ssid.as_deref().unwrap_or("<未连接无线>")
        ));
    }

    fn tick(&mut self) {
        self.drain_commands();
        self.tick_inner();
        self.publish_status();
    }

    fn tick_inner(&mut self) {
        self.ticks += 1;
        self.reload_config();
        self.reload_creds();
        let now = Instant::now();

        // ---- 1. 环境 ----
        let adapters = match net::list_adapters() {
            Ok(v) => {
                self.adapters_err_logged = false;
                v
            }
            Err(e) => {
                if !self.adapters_err_logged {
                    logging::err(format!("网卡枚举失败: {e}"));
                    self.adapters_err_logged = true;
                }
                Vec::new()
            }
        };
        let active = net::active_adapter(&adapters);
        let ssid = wlan::current_ssid();
        let sig = signature(active.as_ref(), ssid.as_deref());
        let network_changed = self.last_sig.as_deref().is_some_and(|s| s != sig);
        if network_changed {
            logging::info(format!(
                "网络变化: 出口={} SSID={}",
                active
                    .as_ref()
                    .map(|a| a.summary())
                    .unwrap_or_else(|| "<无>".to_string()),
                ssid.as_deref().unwrap_or("-")
            ));
            self.machine.force_now();
            self.detect_cache = None;
        }
        self.last_sig = Some(sig);
        self.env.ip = active
            .as_ref()
            .and_then(|a| a.ipv4.first().cloned())
            .unwrap_or_default();
        self.env.mac = active.as_ref().map(|a| a.mac.clone()).unwrap_or_default();
        self.env.gateway = active.as_ref().map(|a| a.gateway.clone()).unwrap_or_default();
        self.env.ssid = ssid.clone().unwrap_or_default();
        self.env.prefix_len = active.as_ref().map(|a| a.prefix_len).unwrap_or(0);

        // ---- 1.5 开机/断网初期：还没有 IPv4 时快速重试，别走"非校园网待机 60s" ----
        // 服务是普通自动启动，开机时网络栈可能还没就绪；这里每 2s 重试一次，
        // 一旦拿到 IP 就立刻判别并登录（"开机即登录"的关键）。
        if self.env.ip.is_empty() {
            // 没配凭据、或用户手动暂停时，别反复刷"等待网络"日志、也别清掉暂停
            if self.creds.is_none() || self.machine.is_paused(Instant::now()) {
                return;
            }
            let first = self.machine.state != State::Probing;
            self.machine.force_now();
            if first {
                logging::info("等待网络就绪（还没有 IPv4），每 2 秒重试（开机即登录）");
            }
            return;
        }

        // ---- 2. 出网判定 ----
        let timeout = self.cfg.probe_timeout();
        let targets = self.cfg.probe_targets();
        let refs: Vec<&str> = targets.iter().map(|s| s.as_str()).collect();
        let (verdict, reports) = http::internet_ok_quick(&refs, timeout);
        self.last_verdict = verdict;
        logging::debug(format!(
            "tick #{}: 出网={} 出口={} 状态={}",
            self.ticks,
            verdict.text(),
            if self.env.ip.is_empty() {
                "<无>".to_string()
            } else {
                format!("{} {}", self.env.gateway, self.env.ip)
            },
            self.machine.state.text()
        ));

        if verdict == Verdict::Online {
            let was_online = self.machine.state.is_online();
            self.machine.on_online();
            if !was_online {
                logging::info("出网正常（在线），无需登录");
            }
            // 在线时也要把"当前是什么网络"识别出来：
            // 之前只在离线时才判别，导致面板一直显示"非校园网"（用户实测反馈）。
            if self.machine.kind == NetworkKind::None || network_changed {
                let kind = self.detect_network(active.as_ref(), ssid.as_deref(), timeout);
                self.machine.on_kind(kind);
                self.env.kind = kind;
            }
            self.heartbeat(active.as_ref(), ssid.as_deref(), verdict);
            return;
        }

        if self.machine.state.is_online() {
            logging::warn("检测到掉线（generate_204 不通）");
        }
        self.machine.on_offline();
        for r in &reports {
            logging::debug(format!("  {}", r.text()));
        }

        // ---- 3. 凭据 ----
        let Some(creds) = self.creds.clone() else {
            if self.machine.state != State::Unconfigured {
                logging::warn("尚未配置学号/密码，进入未配置状态（等面板写入）");
            }
            self.machine.set_unconfigured();
            return;
        };

        // ---- 4. 暂停 / 到点判断 ----
        if self.machine.pause_expired(now) {
            logging::info(format!(
                "暂停结束（{} 分钟），恢复自动登录",
                self.cfg.pause_minutes
            ));
            self.machine.force_now();
        }
        if self.machine.is_paused(now) {
            logging::debug(format!("暂停中：{}", self.machine.summary()));
            return;
        }
        if !self.machine.due(now) {
            logging::debug(format!("未到重试时间：{}", self.machine.summary()));
            return;
        }

        // ---- 5. 判别网络 ----
        self.machine.on_probing();
        let kind = self.detect_network(active.as_ref(), ssid.as_deref(), timeout);
        self.machine.on_kind(kind);
        self.env.kind = kind;
        if kind == NetworkKind::None {
            logging::info("判定为非校园网，进入待机（60 s 后重探）");
            self.machine.on_not_campus();
            return;
        }

        // ---- 6. 登录 ----
        let (Some(ip), Some(mac)) = (
            active.as_ref().and_then(|a| a.ipv4.first().cloned()),
            active.as_ref().map(|a| a.mac.clone()).filter(|m| !m.is_empty()),
        ) else {
            logging::warn("没有可用的 IPv4/MAC，无法登录（等待 DHCP）");
            self.machine.force_now();
            return;
        };

        logging::info(format!(
            "开始登录: {}  账号={}  ip={}",
            kind.text(),
            creds.masked_account(),
            ip
        ));
        self.machine.on_authenticating();

        let started = Instant::now();
        let out: LoginOutcome = match kind {
            NetworkKind::Dorm => portal::dorm::login(&self.cfg, &creds, &ip, &mac, timeout),
            NetworkKind::Public => portal::public::login(&self.cfg, &creds, &ip, &mac, timeout),
            NetworkKind::None => unreachable!(),
        };
        let elapsed = started.elapsed().as_millis();
        self.handle_login_result(kind, out, elapsed, timeout);
    }

    fn handle_login_result(
        &mut self,
        kind: NetworkKind,
        out: LoginOutcome,
        elapsed: u128,
        timeout: Duration,
    ) {
        for a in &out.attempts {
            logging::debug(format!("  尝试: {a}"));
        }
        if out.ok {
            logging::info(format!("登录成功[{}]: {}", kind.text(), out.summary()));
            self.machine.on_login_ok(out.elapsed_ms.max(elapsed));

            let targets = self.cfg.probe_targets();
            let refs: Vec<&str> = targets.iter().map(|s| s.as_str()).collect();
            let (v, _) = http::internet_ok(&refs, timeout);
            self.last_verdict = v;
            if v == Verdict::Online {
                logging::info("复核出网: 已恢复");
            } else {
                logging::warn("门户返回成功，但出网仍不通（下一轮继续复核）");
            }
            return;
        }

        logging::warn(format!("登录失败[{}]: {}", kind.text(), out.summary()));
        if !out.raw_head.is_empty() {
            logging::debug(format!(
                "  响应: {}",
                out.raw_head.replace(['\r', '\n'], " ")
            ));
        }
        self.machine.on_login_fail(out.fault, &out.message, &self.cfg);

        if self.machine.state == State::Paused {
            logging::warn(format!(
                "连续 {} 次账号/密码类失败，暂停 {} 分钟。请检查学号/密码是否正确",
                self.cfg.max_password_fails, self.cfg.pause_minutes
            ));
        } else {
            let left = self
                .machine
                .next_try
                .saturating_duration_since(Instant::now());
            logging::info(format!("将在 {} 秒后重试", left.as_secs()));
        }
    }

    fn detect_network(
        &mut self,
        active: Option<&Adapter>,
        ssid: Option<&str>,
        timeout: Duration,
    ) -> NetworkKind {
        let key = format!(
            "{}|{}|{}",
            active.map(|a| a.gateway.clone()).unwrap_or_default(),
            active.map(|a| a.mac.clone()).unwrap_or_default(),
            ssid.unwrap_or("")
        );
        if let Some((k, kind, at)) = &self.detect_cache {
            if k == &key && at.elapsed() < DETECT_CACHE_TTL {
                logging::debug(format!("判别缓存命中: {} → {}", key, kind.text()));
                return *kind;
            }
        }

        let ipv4 = active.and_then(|a| a.ipv4.first().cloned());
        let gw = active.map(|a| a.gateway.clone()).unwrap_or_default();
        let (kind, ev) = portal::detect(&self.cfg, ssid, ipv4.as_deref(), &gw, timeout);
        self.last_evidence = ev.text();
        if ev.trusted {
            logging::info(format!("网络判别: {} ← 用户手动标记（{}）", kind.text(), ev.trusted_desc));
        } else {
            logging::info(format!("网络判别: {} ← {}", kind.text(), ev.text()));
        }
        for note in &ev.notes {
            logging::debug(format!("  判别备注: {note}"));
        }
        self.detect_cache = Some((key, kind, Instant::now()));
        kind
    }

    fn heartbeat(&mut self, active: Option<&Adapter>, ssid: Option<&str>, verdict: Verdict) {
        if self.last_heartbeat.elapsed() < HEARTBEAT {
            return;
        }
        self.last_heartbeat = Instant::now();
        logging::info(format!(
            "心跳: 出网={} 出口={} SSID={} {}",
            verdict.text(),
            active
                .map(|a| a.summary())
                .unwrap_or_else(|| "<无活跃网卡>".into()),
            ssid.unwrap_or("-"),
            self.machine.summary()
        ));
    }

    // ------------------------------------------------------------------
    // IPC 相关
    // ------------------------------------------------------------------

    /// 取面板发来的命令并执行（只在 worker 线程里调用，避免状态机被并发改）
    fn drain_commands(&mut self) {
        loop {
            let cmd = match self.cmd_rx.try_recv() {
                Ok(c) => c,
                Err(_) => break,
            };
            match cmd {
                Command::Reconnect => {
                    logging::info("面板指令: 立即重连");
                    self.machine.force_now();
                }
                Command::Pause(Some(minutes)) => {
                    logging::info(format!("面板指令: 暂停 {minutes} 分钟"));
                    self.machine.pause(minutes);
                }
                Command::Pause(None) => {
                    logging::info("面板指令: 暂停直到手动恢复");
                    self.machine.pause_until_resume();
                }
                Command::StopService => {
                    logging::info("面板指令: 关闭服务（本次运行）");
                    self.stop.store(true, Ordering::Release);
                }
                Command::Resume => {
                    logging::info("面板指令: 恢复自动登录");
                    self.machine.force_now();
                }
                Command::ReloadCredentials => {
                    logging::info("面板指令: 凭据已更新，立即重载");
                    self.creds_mtime = None;
                    self.reload_creds();
                }
                Command::VerifyLogin => {
                    logging::info("面板指令: 重连并验证（先注销当前会话）");
                    if let Some(creds) = self.creds.clone() {
                        let (ip, mac) = (self.env.ip.clone(), self.env.mac.clone());
                        if !ip.is_empty() && !mac.is_empty() {
                            match self.machine.kind {
                                NetworkKind::Dorm => {
                                    let out = portal::dorm::logout(
                                        &self.cfg,
                                        &creds,
                                        &ip,
                                        &mac,
                                        self.cfg.probe_timeout(),
                                    );
                                    if out.ok {
                                        logging::info("已注销当前会话，马上重新登录");
                                    } else {
                                        logging::warn(format!(
                                            "注销失败（仍会尝试登录）: {}",
                                            out.message
                                        ));
                                    }
                                }
                                NetworkKind::Public => {
                                    logging::warn("公共无线没有注销接口，本次只做重连探测")
                                }
                                NetworkKind::None => {}
                            }
                        }
                    }
                    self.detect_cache = None;
                    self.machine.kind = NetworkKind::None;
                    self.machine.force_now();
                }
                Command::ReloadConfig => {
                    logging::info("面板指令: 配置已更新，立即重载");
                    self.cfg_mtime = None;
                    self.reload_config();
                    // 用户可能改了"标记的校园网"：清掉缓存和旧结论，下一轮重新判别
                    self.detect_cache = None;
                    self.machine.kind = NetworkKind::None;
                    self.machine.force_now();
                }
            }
        }
    }

    /// 把当前状态写进共享快照，供 IPC 读取
    fn publish_status(&self) {
        let now = Instant::now();
        let m = &self.machine;
        let account = self
            .creds
            .as_ref()
            .map(|c| c.masked_account())
            .or_else(|| {
                if self.cfg.account.is_empty() {
                    None
                } else {
                    Some(secret::mask(&self.cfg.account))
                }
            })
            .unwrap_or_default();

        let retry_in_sec = if m.state == State::Online {
            0
        } else {
            m.next_try.saturating_duration_since(now).as_secs() as i64
        };
        let paused_until_epoch = match (m.state, m.paused_until) {
            (State::Paused, Some(t)) => {
                chrono::Local::now().timestamp()
                    + t.saturating_duration_since(now).as_secs() as i64
            }
            _ => 0,
        };

        let status = Status {
            svc_version: version::full(),
            proto: PROTO,
            state: m.state.text().to_string(),
            net_kind: m.kind.text().to_string(),
            net_evidence: self.last_evidence.clone(),
            prefix_len: self.env.prefix_len,
            trusted: self.cfg.trusted_networks.clone(),
            ssid: self.env.ssid.clone(),
            ip: self.env.ip.clone(),
            mac: self.env.mac.clone(),
            gateway: self.env.gateway.clone(),
            online: self.last_verdict == Verdict::Online,
            configured: self.creds.is_some(),
            account,
            last_ok_epoch: m.last_ok_epoch,
            last_ok_elapsed_ms: m.last_ok_elapsed_ms.unwrap_or(0) as u64,
            last_err: m.last_err.clone(),
            paused_until_epoch,
            paused_forever: m.paused_forever,
            retry_in_sec,
            ticks: self.ticks,
            uptime_sec: self.started.elapsed().as_secs(),
            fail_streak: m.fail_streak,
            pw_fail_streak: m.pw_fail_streak,
        };

        if let Ok(mut g) = self.status.lock() {
            *g = status;
        }
    }

    /// 睡到下一次动作；每 200 ms 醒一次：响应停止 + 面板指令 + 配置/凭据热更新
    fn wait_next(&mut self, stop: &Arc<AtomicBool>) {
        let deadline = Instant::now() + self.machine.interval(&self.cfg, Instant::now());
        loop {
            if is_stopping(stop) {
                return;
            }
            let Some(remain) = deadline.checked_duration_since(Instant::now()) else {
                return;
            };
            std::thread::sleep(remain.min(Duration::from_millis(200)));
            self.drain_commands();
            self.reload_config();
            self.reload_creds();
            self.publish_status();
        }
    }

    fn reload_config(&mut self) {
        let path = paths::config_path();
        let mtime = file_mtime(&path);
        if mtime == self.cfg_mtime {
            return;
        }
        self.cfg_mtime = mtime;
        let (cfg, outcome) = Config::load_or_create(&path);
        if outcome == LoadOutcome::Loaded || outcome == LoadOutcome::Recovered {
            logging::set_level(cfg.level());
            logging::info(format!("配置已重新加载（{}）", outcome.text()));
            if self.machine.backoff_idx >= cfg.backoff.len().max(1) {
                self.machine.backoff_idx = 0;
            }
            self.cfg = cfg;
        }
    }

    fn reload_creds(&mut self) {
        let path = paths::secret_path();
        let mtime = file_mtime(&path);
        if mtime == self.creds_mtime {
            return;
        }
        self.creds_mtime = mtime;
        match secret::load(&path) {
            Ok(Some(c)) => {
                logging::info(format!("凭据已更新（账号 {}）", c.masked_account()));
                self.creds = Some(c);
                if !self.machine.paused_forever {
                    self.machine.force_now();
                }
            }
            Ok(None) => {
                if self.creds.is_some() {
                    logging::warn("凭据文件已被删除，回到未配置状态");
                }
                self.creds = None;
                self.machine.set_unconfigured();
            }
            Err(e) => {
                logging::warn(format!("凭据读取失败（{e}），按未配置处理"));
                self.creds = None;
                self.machine.set_unconfigured();
            }
        }
    }
}

fn signature(active: Option<&Adapter>, ssid: Option<&str>) -> String {
    match active {
        Some(a) => format!(
            "{}|{}|{}|{}",
            a.if_index,
            a.ipv4.first().cloned().unwrap_or_default(),
            a.gateway,
            ssid.unwrap_or("")
        ),
        None => format!("<none>|{}", ssid.unwrap_or("")),
    }
}

fn file_mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}
