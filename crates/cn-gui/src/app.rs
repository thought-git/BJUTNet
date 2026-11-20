//! 面板主体：状态 / 设置 / 日志 / 关于 四页。

use std::time::{Duration, Instant};

use eframe::egui;
use serde_json::json;

use cn_core::config::Config;
use cn_core::ipc::protocol::Status;
use cn_core::paths;

use crate::bridge::{Bridge, UiEvent};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Status,
    Settings,
    Billing,
    Logs,
    About,
}

/// 计费页展示用的视图数据（从 IPC JSON 解析）
#[derive(Clone)]
struct BillingView {
    balance_yuan: f64,
    used_mb: f64,
    left_mb: f64,
    used_time_min: u64,
    package_price_yuan: Option<f64>,
    fetched_epoch: i64,
}

impl BillingView {
    fn from_json(v: &serde_json::Value) -> Option<Self> {
        Some(Self {
            balance_yuan: v.get("balance_yuan")?.as_f64()?,
            used_mb: v.get("used_mb")?.as_f64()?,
            left_mb: v.get("left_mb")?.as_f64()?,
            used_time_min: v.get("used_time_min").and_then(|x| x.as_u64()).unwrap_or(0),
            package_price_yuan: v.get("package_price_yuan").and_then(|x| x.as_f64()),
            fetched_epoch: v.get("fetched_epoch").and_then(|x| x.as_i64()).unwrap_or(0),
        })
    }
    fn total_mb(&self) -> f64 {
        (self.used_mb + self.left_mb).max(0.0)
    }
    fn arrears(&self) -> bool {
        self.balance_yuan <= 0.005
    }

    fn below_package(&self) -> bool {
        self.package_price_yuan
            .map(|p| self.balance_yuan < p)
            .unwrap_or(false)
    }
}

pub struct App {
    bridge: Bridge,
    tab: Tab,

    status: Option<Status>,
    status_err: String,

    // 设置页输入
    cfg_loaded: bool,
    account: String,
    password: String,
    password_visible: bool,
    check_interval: u64,
    offline_interval: u64,
    probe_timeout: u64,
    log_level: String,
    auto_wifi: bool,
    wifi_profile: String,

    // 日志页
    log_lines: Vec<String>,
    auto_log: bool,
    last_log_at: Instant,

    // 计费页
    billing_loaded: bool,
    billing_loading: bool,
    billing_need_captcha: bool,
    billing_captcha: Option<egui::TextureHandle>,
    billing_captcha_img: Option<egui::ColorImage>,
    billing_code: String,
    billing_captcha_cmd: String,
    billing_error: String,
    billing_data: Option<BillingView>,
    billing_hide: bool,
    billing_auto: bool,
    billing_flow_only_offline: bool,
    billing_flow_alert: bool,
    billing_flow_alert_mb: u64,
    billing_balance_alert: bool,
    billing_balance_only_offline: bool,
    billing_confirm_auto: bool,
    confirm_reset_settings: bool,
    confirm_reset_billing: bool,
    billing_unsupported: bool,
    /// 服务端版本与面板不一致（常见于只换了面板/只换了服务）
    svc_version_mismatch: bool,
    // 本月停用
    month_off: bool,
    month_off_until_epoch: i64,
    cfg_req_at: Option<Instant>,

    // 关于页
    diag_text: Option<String>,

    // 提示
    toast: Option<(String, Instant, bool)>,
    /// 「关闭服务」的二次确认
    confirm_stop: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.all_styles_mut(|s| {
            s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        });

        Self {
            bridge: Bridge::new(cc.egui_ctx.clone()),
            tab: Tab::Status,
            status: None,
            status_err: String::new(),
            cfg_loaded: false,
            account: String::new(),
            password: String::new(),
            password_visible: false,
            check_interval: 30,
            offline_interval: 5,
            probe_timeout: 3,
            log_level: "INFO".to_string(),
            auto_wifi: false,
            wifi_profile: "bjut_wifi".to_string(),
            log_lines: Vec::new(),
            auto_log: true,
            last_log_at: Instant::now() - Duration::from_secs(5),
            billing_loaded: false,
            billing_loading: false,
            billing_need_captcha: false,
            billing_captcha: None,
            billing_captcha_img: None,
            billing_code: String::new(),
            billing_captcha_cmd: "billing_refresh".into(),
            billing_error: String::new(),
            billing_data: None,
            billing_hide: false,
            billing_auto: false,
            billing_flow_only_offline: false,
            billing_flow_alert: true,
            billing_flow_alert_mb: 512,
            billing_balance_alert: true,
            billing_balance_only_offline: false,
            billing_confirm_auto: false,
            confirm_reset_settings: false,
            confirm_reset_billing: false,
            billing_unsupported: false,
            svc_version_mismatch: false,
            month_off: false,
            month_off_until_epoch: 0,
            cfg_req_at: None,
            diag_text: None,
            toast: None,
            confirm_stop: false,
        }
    }

    fn toast(&mut self, msg: impl Into<String>, is_err: bool) {
        self.toast = Some((msg.into(), Instant::now(), is_err));
    }

    /// 首次连上服务后，从 config.json 预填设置页（只用读，不写）
    fn ensure_cfg_loaded(&mut self) {
        if self.cfg_loaded {
            return;
        }
        let now = Instant::now();
        if let Some(at) = self.cfg_req_at {
            if now.duration_since(at) < Duration::from_secs(2) {
                return;
            }
        }
        self.cfg_req_at = Some(now);
        // 配置以服务为准（服务写的是 SYSTEM 权限的文件，面板直接读文件可能失败）
        self.bridge.send("get_config", json!({}), "config");
    }

    /// 老服务没有 get_config 时，退回读本地配置文件（只显示，不一定能保存新字段）
    fn apply_config_file(&mut self) -> bool {
        let Some(cfg) = Config::load_existing(&paths::config_path()) else {
            return false;
        };
        self.account = cfg.account.clone();
        self.check_interval = cfg.check_interval_sec;
        self.offline_interval = cfg.offline_interval_sec;
        self.probe_timeout = cfg.probe_timeout_sec;
        self.log_level = cfg.log_level.clone();
        self.auto_wifi = cfg.auto_connect_wifi;
        self.wifi_profile = cfg.wifi_profile.clone();
        self.billing_auto = cfg.billing_auto_check;
        self.billing_hide = cfg.billing_hide_amount;
        self.billing_flow_alert = cfg.billing_flow_alert;
        self.billing_flow_alert_mb = cfg.billing_flow_alert_mb;
        self.billing_flow_only_offline = cfg.billing_flow_only_offline;
        self.billing_balance_alert = cfg.billing_balance_alert;
        self.billing_balance_only_offline = cfg.billing_balance_only_offline;
        self.month_off = cfg.no_connect_until_epoch > chrono::Local::now().timestamp();
        self.month_off_until_epoch = cfg.no_connect_until_epoch;
        self.cfg_loaded = true;
        true
    }

    fn drain_events(&mut self) {
        while let Ok(ev) = self.bridge.rx.try_recv() {
            match ev {
                UiEvent::Status(Ok(st)) => {
                    self.svc_version_mismatch = st.svc_version != cn_core::version::full();
                    self.status = Some(st);
                    self.status_err.clear();
                    self.ensure_cfg_loaded();
                }
                UiEvent::Status(Err(e)) => {
                    self.status = None;
                    self.status_err = e;
                }
                UiEvent::Action(Ok(resp), label) => {
                    if !resp.ok {
                        if label.starts_with("billing") {
                            self.billing_loading = false;
                            self.billing_error = resp.error.clone();
                            self.billing_loaded = false;
                        }
                        self.toast(format!("{label} 失败: {}", resp.error), true);
                    } else {
                        match label {
                            "config" => {
                                let d = &resp.data;
                                let s = |k: &str| {
                                    d.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string()
                                };
                                let n = |k: &str| d.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
                                let b =
                                    |k: &str| d.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
                                self.account = s("account");
                                if n("checkIntervalSec") > 0 {
                                    self.check_interval = n("checkIntervalSec");
                                }
                                if n("offlineIntervalSec") > 0 {
                                    self.offline_interval = n("offlineIntervalSec");
                                }
                                if n("probeTimeoutSec") > 0 {
                                    self.probe_timeout = n("probeTimeoutSec");
                                }
                                if !s("logLevel").is_empty() {
                                    self.log_level = s("logLevel");
                                }
                                self.auto_wifi = b("autoConnectWifi");
                                if !s("wifiProfile").is_empty() {
                                    self.wifi_profile = s("wifiProfile");
                                }
                                self.billing_auto = b("billingAutoCheck");
                                self.billing_hide = b("billingHideAmount");
                                self.billing_flow_alert = b("billingFlowAlert");
                                self.billing_flow_alert_mb = n("billingFlowAlertMb");
                                self.billing_flow_only_offline = b("billingFlowOnlyOffline");
                                self.billing_balance_alert = b("billingBalanceAlert");
                                self.billing_balance_only_offline = b("billingBalanceOnlyOffline");
                                self.month_off = b("monthOff");
                                self.month_off_until_epoch = d
                                    .get("monthOffUntilEpoch")
                                    .and_then(|v| v.as_i64())
                                    .unwrap_or(0);
                                self.cfg_loaded = true;
                            }
                            "reset_settings" | "reset_billing" => {
                                self.confirm_reset_settings = false;
                                self.confirm_reset_billing = false;
                                self.cfg_loaded = false;
                                self.billing_loaded = false;
                                self.billing_unsupported = false;
                                self.toast("已恢复默认设置", false);
                            }
                            "options" => {
                                // 保存后重新从服务读一遍，避免界面和服务不一致
                                self.cfg_loaded = false;
                                self.toast("已保存", false);
                            }
                            "logs" => {
                                if let Some(arr) = resp.data.get("lines").and_then(|v| v.as_array())
                                {
                                    self.log_lines = arr
                                        .iter()
                                        .map(|v| v.as_str().unwrap_or("").to_string())
                                        .collect();
                                    self.last_log_at = Instant::now();
                                }
                            }
                            "diag" => {
                                if let Some(t) = resp.data.get("text").and_then(|v| v.as_str()) {
                                    self.diag_text = Some(t.to_string());
                                }
                            }
                            "trusted" => {
                                let added = resp
                                    .data
                                    .get("added")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                let msg = resp.data.get("msg").and_then(|v| v.as_str()).unwrap_or(
                                    if added {
                                        "已加入标记列表，服务会立刻按校园网重新判别"
                                    } else {
                                        "这张网络已经在标记列表里了"
                                    },
                                );
                                self.toast(msg.to_string(), false);
                            }
                            "verify_login" => {
                                self.toast("已注销并开始重新登录，几秒后看状态/最近错误", false);
                            }
                            "trusted-remove" => {
                                self.toast("已移除该标记", false);
                            }
                            "credentials" => {
                                self.password.clear();
                                self.password_visible = false;
                                self.cfg_loaded = false; // 重新拉一次配置
                                let online =
                                    self.status.as_ref().map(|s| s.online).unwrap_or(false);
                                if online {
                                    self.toast(
                                        "已保存。当前在线，密码要等掉线才会验证；想立刻确认请点「重连并验证」",
                                        false,
                                    );
                                } else {
                                    self.toast("已保存，服务正在用新密码登录，稍等看状态页", false);
                                }
                            }
                            "billing_status" | "billing_refresh" | "billing_check" => {
                                self.billing_loading = false;
                                if let Some(v) =
                                    resp.data.get("hide_amount").and_then(|v| v.as_bool())
                                {
                                    self.billing_hide = v;
                                }
                                if let Some(v) =
                                    resp.data.get("auto_check").and_then(|v| v.as_bool())
                                {
                                    self.billing_auto = v;
                                }
                                if let Some(v) =
                                    resp.data.get("flow_alert").and_then(|v| v.as_bool())
                                {
                                    self.billing_flow_alert = v;
                                }
                                if let Some(v) =
                                    resp.data.get("flow_alert_mb").and_then(|v| v.as_u64())
                                {
                                    self.billing_flow_alert_mb = v;
                                }
                                if let Some(v) =
                                    resp.data.get("flow_only_offline").and_then(|v| v.as_bool())
                                {
                                    self.billing_flow_only_offline = v;
                                }
                                if let Some(v) =
                                    resp.data.get("balance_alert").and_then(|v| v.as_bool())
                                {
                                    self.billing_balance_alert = v;
                                }
                                if let Some(v) = resp
                                    .data
                                    .get("balance_only_offline")
                                    .and_then(|v| v.as_bool())
                                {
                                    self.billing_balance_only_offline = v;
                                }
                                let need = resp
                                    .data
                                    .get("need_captcha")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                self.billing_need_captcha = need;
                                if need {
                                    self.billing_captcha = None;
                                    self.billing_captcha_img = None;
                                    if let Some(b64) =
                                        resp.data.get("captcha_png").and_then(|v| v.as_str())
                                    {
                                        use base64::Engine;
                                        if let Ok(bytes) =
                                            base64::engine::general_purpose::STANDARD.decode(b64)
                                        {
                                            if let Ok(img) = image::load_from_memory(&bytes) {
                                                let rgba = img.to_rgba8();
                                                let (w, h) = rgba.dimensions();
                                                self.billing_captcha_img =
                                                    Some(egui::ColorImage::from_rgba_unmultiplied(
                                                        [w as usize, h as usize],
                                                        rgba.as_raw(),
                                                    ));
                                            }
                                        }
                                    }
                                    self.billing_captcha_cmd = label.to_string();
                                    self.billing_code.clear();
                                    self.toast("需要验证码：按图片输入后提交", false);
                                } else {
                                    self.billing_captcha = None;
                                    self.billing_captcha_img = None;
                                    if let Some(v) = resp.data.get("data") {
                                        if !v.is_null() {
                                            self.billing_data = BillingView::from_json(v);
                                            self.billing_error.clear();
                                        }
                                    }
                                    if label == "billing_check" {
                                        if resp
                                            .data
                                            .get("popped")
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(false)
                                        {
                                            self.toast("条件命中，已弹出提醒", false);
                                        } else if let Some(reason) =
                                            resp.data.get("reason").and_then(|v| v.as_str())
                                        {
                                            self.toast(reason.to_string(), false);
                                        }
                                    } else if resp.data.get("data").is_some() {
                                        self.toast("计费信息已更新", false);
                                    }
                                }
                            }
                            "reconnect" => self.toast("已触发立即重连", false),
                            "pause" => self.toast("已暂停自动登录", false),
                            "stop_service" => {
                                self.toast("服务正在关闭（本次运行），面板即将断开", false)
                            }
                            other => {
                                self.toast(format!("{other} 已执行"), false);
                            }
                        }
                    }
                }
                UiEvent::Action(Err(e), label) => {
                    if label.starts_with("billing") {
                        self.billing_loading = false;
                        if e.contains("未知命令") {
                            self.billing_unsupported = true;
                            self.billing_loaded = true;
                            self.billing_error =
                                "服务端版本过旧，不支持计费功能；请安装新版服务。".into();
                        } else {
                            self.billing_error = e.clone();
                            self.billing_loaded = false;
                        }
                    }
                    if label == "config" {
                        if e.contains("未知命令") {
                            if self.apply_config_file() {
                                self.toast(
                                    "服务端版本过旧（没有 get_config），已从本地配置读取；请安装新版服务",
                                    true,
                                );
                            } else {
                                self.cfg_loaded = true;
                                self.toast("服务端版本过旧，无法读取配置；请安装新版服务", true);
                            }
                            // 不再每 2 秒重试
                            self.cfg_req_at = Some(Instant::now() + Duration::from_secs(3600));
                        } else {
                            // 服务没起来/管道断了：2 秒后再试，不弹错误（状态栏已提示未连接）
                            self.cfg_req_at = Some(Instant::now());
                        }
                    } else if label == "options" && e.contains("没有可保存的选项字段") {
                        self.toast(
                            "服务端版本过旧，该设置需要新版服务才能保存；请安装 1.3.0 服务",
                            true,
                        );
                    } else {
                        self.toast(format!("{label} 失败: {e}"), true);
                    }
                }
            }
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let (color, text) = match &self.status {
            Some(st) => {
                let (r, g, b) = st.state_color();
                (
                    egui::Color32::from_rgb(r, g, b),
                    if st.state.contains("无网络") {
                        format!(
                            "{} · {}",
                            st.state,
                            if st.online {
                                "出网正常"
                            } else {
                                "出网异常"
                            }
                        )
                    } else {
                        format!(
                            "{} · {} · {}",
                            st.state,
                            st.net_kind,
                            if st.online {
                                "出网正常"
                            } else {
                                "出网异常"
                            }
                        )
                    },
                )
            }
            None => (
                egui::Color32::from_rgb(120, 120, 120),
                "未连接服务（服务可能没运行）".to_string(),
            ),
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("●").color(color).size(18.0));
            ui.label(egui::RichText::new(text).size(15.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("服务 {}", cn_core::version::full()))
                        .weak()
                        .small(),
                );
            });
        });
        if let Some(st) = &self.status {
            if !st.configured {
                ui.colored_label(
                    egui::Color32::from_rgb(200, 140, 0),
                    "⚠ 还没有配置学号/密码：请到「设置」页填写保存，之后就可以关掉面板了",
                );
            }
        } else {
            ui.colored_label(
                egui::Color32::from_rgb(200, 60, 60),
                format!("{}", self.status_err),
            );
            ui.label(
                egui::RichText::new(
                    "先启动服务：BJUTNetSvc.exe --console（调试）或安装成服务（开机自启）",
                )
                .weak()
                .small(),
            );
        }
    }

    fn tabs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for (tab, name) in [
                (Tab::Status, "状态"),
                (Tab::Settings, "设置"),
                (Tab::Billing, "计费"),
                (Tab::Logs, "日志"),
                (Tab::About, "关于"),
            ] {
                if ui.selectable_label(self.tab == tab, name).clicked() {
                    self.tab = tab;
                }
            }
        });
    }

    fn status_tab(&mut self, ui: &mut egui::Ui) {
        let Some(st) = self.status.clone() else {
            ui.label("服务未运行，暂时没有状态。");
            return;
        };

        egui::Grid::new("status_grid")
            .num_columns(2)
            .spacing([24.0, 6.0])
            .show(ui, |ui| {
                let mut row = |k: &str, v: String| {
                    ui.label(egui::RichText::new(k).weak());
                    ui.label(v);
                    ui.end_row();
                };
                row("状态", st.state.clone());
                row(
                    "网络",
                    if st.state.contains("无网络") {
                        "无网络".into()
                    } else {
                        st.net_kind.clone()
                    },
                );
                // 网络类型（协议）与接入介质（有线/无线）是两回事：
                // 宿舍网、公共网都可能走有线或无线，这里单独显示介质。
                let medium = if st.wireless || !st.ssid.is_empty() {
                    "无线"
                } else {
                    "有线"
                };
                row(
                    "接入",
                    match (st.ssid.is_empty(), st.iface.is_empty()) {
                        (false, false) => format!("{medium}（{}，SSID {}）", st.iface, st.ssid),
                        (false, true) => format!("{medium}（SSID {}）", st.ssid),
                        (true, false) => format!("{medium}（{}）", st.iface),
                        (true, true) => "-".into(),
                    },
                );
                row(
                    "判别依据",
                    if st.net_evidence.is_empty() {
                        "-".into()
                    } else {
                        st.net_evidence.clone()
                    },
                );
                row(
                    "SSID",
                    if st.ssid.is_empty() {
                        "-".into()
                    } else {
                        st.ssid.clone()
                    },
                );
                row(
                    "本机 IP",
                    if st.ip.is_empty() {
                        "-".into()
                    } else {
                        st.ip.clone()
                    },
                );
                row(
                    "网关",
                    if st.gateway.is_empty() {
                        "-".into()
                    } else {
                        st.gateway.clone()
                    },
                );
                row(
                    "MAC",
                    if st.mac.is_empty() {
                        "-".into()
                    } else {
                        st.mac.clone()
                    },
                );
                row(
                    "账号",
                    if st.account.is_empty() {
                        "<未配置>".into()
                    } else {
                        st.account.clone()
                    },
                );
                row(
                    "最近成功",
                    if st.last_ok_epoch > 0 {
                        chrono::DateTime::from_timestamp(st.last_ok_epoch, 0)
                            .map(|t| {
                                t.with_timezone(&chrono::Local)
                                    .format("%Y-%m-%d %H:%M:%S")
                                    .to_string()
                            })
                            .unwrap_or_else(|| "-".into())
                    } else {
                        "-".into()
                    },
                );
                row("运行时长", format_duration(st.uptime_sec));
                if st.month_off {
                    row(
                        "本月停用",
                        chrono::DateTime::from_timestamp(st.month_off_until_epoch, 0)
                            .map(|d| {
                                format!(
                                    "已停用自动连接（到 {}）",
                                    d.with_timezone(&chrono::Local).format("%Y-%m-%d")
                                )
                            })
                            .unwrap_or_else(|| "已停用自动连接".into()),
                    );
                }
                row(
                    "暂停",
                    if st.paused_forever {
                        "手动暂停（直到点恢复）".into()
                    } else if st.paused_until_epoch > 0 {
                        chrono::DateTime::from_timestamp(st.paused_until_epoch, 0)
                            .map(|t| {
                                format!("到 {}", t.with_timezone(&chrono::Local).format("%H:%M:%S"))
                            })
                            .unwrap_or_else(|| "-".into())
                    } else {
                        "-".into()
                    },
                );
                row(
                    "下次重试",
                    if st.online {
                        "-".into()
                    } else if st.retry_in_sec > 0 {
                        format!("{} 秒后", st.retry_in_sec)
                    } else {
                        "马上".into()
                    },
                );
                if !st.last_err.is_empty() {
                    row("最近错误", st.last_err.clone());
                }
            });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("立即重连").clicked() {
                self.bridge.send("reconnect", json!({}), "reconnect");
            }
            if ui.button("重连并验证").on_hover_text(
                "先注销当前门户会话，再立刻重新登录一次\n用来验证面板里保存的密码到底对不对（会让网络断几秒）",
            ).clicked() {
                self.bridge.send("verify_login", json!({}), "verify_login");
            }
            if ui.button("恢复自动登录").clicked() {
                self.bridge.send("resume", json!({}), "resume");
            }
        });
        ui.horizontal(|ui| {
            if ui.button("暂停 30 分钟").clicked() {
                self.bridge.send("pause", json!({"minutes": 30}), "pause");
            }
            if ui.button("暂停 2 小时").clicked() {
                self.bridge.send("pause", json!({"minutes": 120}), "pause");
            }
            if ui
                .button("暂停直到恢复")
                .on_hover_text("不会自动恢复：点「恢复自动登录」或重启服务才解除")
                .clicked()
            {
                self.bridge.send("pause", json!({"forever": true}), "pause");
            }
            ui.separator();
            if ui
                .button("关闭服务")
                .on_hover_text(
                    "结束服务进程（本次运行）。服务启动类型仍是「自动」，下次开机会自己起来",
                )
                .clicked()
            {
                self.confirm_stop = true;
            }
        });
        if self.confirm_stop {
            ui.horizontal(|ui| {
                ui.colored_label(
                    egui::Color32::from_rgb(200, 60, 60),
                    "确定关闭服务？关闭后不会自动登录，直到下次开机/手动启动。",
                );
                if ui.button("确定关闭").clicked() {
                    self.confirm_stop = false;
                    self.bridge.send("stop_service", json!({}), "stop_service");
                }
                if ui.button("取消").clicked() {
                    self.confirm_stop = false;
                }
            });
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("识别不准？把当前网络标记为校园网：");
            if ui
                .button("宿舍网")
                .on_hover_text("有线 / 无线都适用：按门户协议区分，不看接入介质")
                .clicked()
            {
                self.bridge
                    .send("add_trusted_current", json!({"kind": "dorm"}), "trusted");
            }
            if ui
                .button("公共网")
                .on_hover_text("有线 / 无线都适用：按门户协议区分，不看接入介质")
                .clicked()
            {
                self.bridge
                    .send("add_trusted_current", json!({"kind": "public"}), "trusted");
            }
        });
        if let Some(st) = &self.status {
            if !st.trusted.is_empty() {
                ui.label(
                    egui::RichText::new(format!(
                        "已标记 {} 张网络（设置页可管理）",
                        st.trusted.len()
                    ))
                    .weak()
                    .small(),
                );
            }
        }
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(
                "提示：面板只是查看/修改用。关闭本窗口不会影响服务——服务已配置好后会自己开机自启、自己检测登录。",
            )
            .weak()
            .small(),
        );
    }

    fn settings_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("账号");
        egui::Grid::new("cred_grid")
            .num_columns(2)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                ui.label("学号");
                ui.add(
                    egui::TextEdit::singleline(&mut self.account)
                        .hint_text("如 20250000")
                        .desired_width(240.0),
                );
                ui.end_row();

                ui.label("密码");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.password)
                            .password(!self.password_visible)
                            .hint_text("修改账号/密码时填这里")
                            .desired_width(240.0),
                    );
                    ui.checkbox(&mut self.password_visible, "显示");
                });
                ui.end_row();
            });

        if self.status.as_ref().map(|s| s.configured).unwrap_or(false) {
            ui.label(
                egui::RichText::new("已保存学号密码（密码不会显示）")
                    .weak()
                    .small(),
            );
        }

        ui.horizontal(|ui| {
            if ui.button("保存并测试登录").clicked() {
                if self.account.trim().is_empty() {
                    self.toast("学号不能为空", true);
                } else if self.password.is_empty() {
                    self.toast("请重新输入密码（服务端只存密文，无法回显）", true);
                } else {
                    self.bridge.send(
                        "set_credentials",
                        json!({"account": self.account.trim(), "password": self.password}),
                        "credentials",
                    );
                }
            }
            ui.label(
                egui::RichText::new("保存后由服务加密落盘，面板关掉也能继续登录")
                    .weak()
                    .small(),
            );
        });

        ui.separator();
        ui.heading("运行参数");
        egui::Grid::new("opt_grid")
            .num_columns(2)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                ui.label("掉线检测间隔（秒）");
                ui.add(egui::DragValue::new(&mut self.check_interval).range(3..=3600));
                ui.end_row();

                ui.label("离线/开机重试间隔（秒）");
                ui.add(egui::DragValue::new(&mut self.offline_interval).range(1..=3600));
                ui.end_row();

                ui.label("探测超时（秒）");
                ui.add(egui::DragValue::new(&mut self.probe_timeout).range(1..=30));
                ui.end_row();

                ui.label("日志级别");
                egui::ComboBox::from_id_salt("log_level")
                    .selected_text(self.log_level.clone())
                    .show_ui(ui, |ui| {
                        for l in ["ERR", "WARN", "INFO", "DEBUG"] {
                            ui.selectable_value(&mut self.log_level, l.to_string(), l);
                        }
                    });
                ui.end_row();

                ui.label("自动连接校园 WiFi");
                ui.checkbox(&mut self.auto_wifi, "允许（默认关，避免把宿舍网踢掉）");
                ui.end_row();

                ui.label("校园 WiFi 名称");
                ui.add(egui::TextEdit::singleline(&mut self.wifi_profile).desired_width(200.0));
                ui.end_row();
            });

        ui.separator();
        ui.heading("已标记为校园网的网络");
        let trusted = self
            .status
            .as_ref()
            .map(|s| s.trusted.clone())
            .unwrap_or_default();
        if trusted.is_empty() {
            ui.label(
                egui::RichText::new(
                    "（无）如果服务把当前网络认成「非校园网」，到「状态」页点「宿舍网 / 公共网」按钮即可加入。",
                )
                .weak(),
            );
        } else {
            for (i, t) in trusted.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("{}. {}", i + 1, t.describe()));
                    if ui.button("删除").clicked() {
                        self.bridge
                            .send("remove_trusted", json!({"index": i}), "trusted-remove");
                    }
                });
            }
        }

        ui.separator();
        ui.heading("本月停用");
        let mut month_off = self.month_off;
        if ui
            .checkbox(
                &mut month_off,
                "本月不再自动连接校园网（流量用尽时避免超额）",
            )
            .changed()
        {
            self.month_off = month_off;
            self.bridge
                .send("set_options", json!({ "monthOff": month_off }), "options");
        }
        ui.label(
            egui::RichText::new(
                "开启后掉线不再自动登录，直到下月 1 日自动恢复；当前已连的会话不会主动断开；「立即重连」可手动连一次。",
            )
            .weak()
            .small(),
        );
        if self.month_off && self.month_off_until_epoch > 0 {
            ui.label(
                chrono::DateTime::from_timestamp(self.month_off_until_epoch, 0)
                    .map(|d| {
                        format!(
                            "恢复时间：{}",
                            d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M")
                        )
                    })
                    .unwrap_or_default(),
            );
        }

        ui.horizontal(|ui| {
            if ui.button("保存运行参数").clicked() {
                self.bridge.send(
                    "set_options",
                    json!({
                        "checkIntervalSec": self.check_interval,
                        "offlineIntervalSec": self.offline_interval,
                        "probeTimeoutSec": self.probe_timeout,
                        "logLevel": self.log_level,
                        "autoConnectWifi": self.auto_wifi,
                        "wifiProfile": self.wifi_profile,
                    }),
                    "options",
                );
            }
            if ui.button("恢复默认设置").clicked() {
                self.confirm_reset_settings = true;
            }
        });
        if self.confirm_reset_settings {
            ui.colored_label(
                egui::Color32::from_rgb(200, 140, 0),
                "恢复默认会重置运行参数（探测间隔、超时、日志级别、WiFi、本月停用等），不影响账号密码和标记网络。",
            );
            ui.horizontal(|ui| {
                if ui.button("确认恢复").clicked() {
                    self.confirm_reset_settings = false;
                    self.bridge.send(
                        "reset_options",
                        json!({ "scope": "settings" }),
                        "reset_settings",
                    );
                }
                if ui.button("取消").clicked() {
                    self.confirm_reset_settings = false;
                }
            });
        }
    }

    fn billing_tab(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.heading("计费（只读）");
        if self.billing_unsupported {
            ui.colored_label(
                egui::Color32::from_rgb(200, 140, 0),
                "当前服务端版本过旧，不支持计费功能；请安装新版后使用。",
            );
            return;
        }
        ui.horizontal(|ui| {
            if ui.button("刷新").clicked() {
                self.billing_loading = true;
                self.billing_error.clear();
                self.bridge
                    .send("billing_refresh", json!({}), "billing_refresh");
            }
            if ui.button("立即检查并弹窗").clicked() {
                self.billing_loading = true;
                self.billing_error.clear();
                self.bridge
                    .send("billing_check_now", json!({}), "billing_check");
            }
            if ui.button("在浏览器打开").clicked() {
                if let Err(e) =
                    cn_core::shell::open_url("https://jfself.bjut.edu.cn/Self/dashboard")
                {
                    self.toast(e, true);
                }
            }
            if self.billing_loading {
                ui.spinner();
                ui.label("查询中…");
            }
        });
        ui.label(
            egui::RichText::new(
                "只读：只查余额和流量，不缴费、不改套餐。登录使用已保存的校园网账号密码。",
            )
            .weak()
            .small(),
        );
        ui.separator();

        if self.billing_need_captcha {
            ui.label("该站点要求验证码（连续登录失败会触发）。请输入图片中的字符：");
            if let Some(img) = self.billing_captcha_img.take() {
                self.billing_captcha =
                    Some(ctx.load_texture("billing_captcha", img, egui::TextureOptions::NEAREST));
            }
            if let Some(tex) = &self.billing_captcha {
                ui.add(egui::Image::new((tex.id(), egui::vec2(120.0, 40.0))));
            }
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.billing_code)
                        .desired_width(90.0)
                        .hint_text("验证码"),
                );
                if ui.button("提交").clicked() {
                    let code = self.billing_code.trim().to_string();
                    self.billing_loading = true;
                    let cmd = if self.billing_captcha_cmd == "billing_check" {
                        "billing_check_now"
                    } else {
                        "billing_refresh"
                    };
                    let label = if cmd == "billing_check_now" {
                        "billing_check"
                    } else {
                        "billing_refresh"
                    };
                    self.bridge.send(cmd, json!({ "code": code }), label);
                }
            });
            ui.label(
                egui::RichText::new("也可以点「在浏览器打开」登录一次，再回来点刷新。")
                    .weak()
                    .small(),
            );
        } else if !self.billing_error.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(200, 60, 60),
                self.billing_error.clone(),
            );
            ui.label(
                egui::RichText::new(
                    "如果提示页面解析失败，可能是站点改版；请先点「在浏览器打开」确认页面正常。",
                )
                .weak()
                .small(),
            );
        } else if let Some(d) = self.billing_data.clone() {
            egui::Grid::new("billing_grid")
                .num_columns(2)
                .spacing([24.0, 6.0])
                .show(ui, |ui| {
                    let money = |v: f64, hide: bool| {
                        if hide {
                            "•••".to_string()
                        } else {
                            format!("¥ {v:.2}")
                        }
                    };
                    ui.label("余额");
                    ui.label(money(d.balance_yuan, self.billing_hide));
                    ui.end_row();

                    ui.label("余额状态");
                    if d.arrears() {
                        ui.colored_label(
                            egui::Color32::from_rgb(200, 60, 60),
                            "余额为 0（可能已停机）".to_string(),
                        );
                    } else if d.below_package() {
                        ui.colored_label(
                            egui::Color32::from_rgb(200, 140, 0),
                            "余额不足以支付套餐".to_string(),
                        );
                    } else {
                        ui.colored_label(egui::Color32::from_rgb(46, 160, 67), "正常");
                    }
                    ui.end_row();

                    if let Some(price) = d.package_price_yuan {
                        ui.label("当前套餐价");
                        ui.label(format!("¥ {price:.2}"));
                        ui.end_row();
                    }
                    ui.label("本月已用流量");
                    ui.label(format!("{:.2} GB", d.used_mb / 1024.0));
                    ui.end_row();
                    ui.label("剩余流量");
                    ui.label(format!("{:.2} GB", d.left_mb / 1024.0));
                    ui.end_row();
                    ui.label("总配额");
                    ui.label(format!("{:.2} GB", d.total_mb() / 1024.0));
                    ui.end_row();
                    if d.used_time_min > 0 {
                        ui.label("已用时长");
                        ui.label(format!(
                            "{} 小时 {} 分",
                            d.used_time_min / 60,
                            d.used_time_min % 60
                        ));
                        ui.end_row();
                    }
                    ui.label("查询时间");
                    ui.label(
                        chrono::DateTime::from_timestamp(d.fetched_epoch, 0)
                            .map(|t| {
                                t.with_timezone(&chrono::Local)
                                    .format("%Y-%m-%d %H:%M:%S")
                                    .to_string()
                            })
                            .unwrap_or_else(|| "-".into()),
                    );
                    ui.end_row();
                });
        } else {
            ui.label("还没有查询结果，点「刷新」查询。");
        }

        ui.add_space(8.0);
        ui.separator();
        let mut hide = self.billing_hide;
        if ui.checkbox(&mut hide, "隐藏金额（余额/欠费）").changed() {
            self.billing_hide = hide;
            self.bridge.send(
                "set_options",
                json!({ "billingHideAmount": hide }),
                "options",
            );
        }
        // 主开关
        let mut auto = self.billing_auto;
        if ui
            .checkbox(&mut auto, "开机自动查询（按下面勾选的条件弹窗提醒）")
            .changed()
        {
            if auto {
                self.billing_confirm_auto = true;
            } else {
                self.billing_auto = false;
                self.bridge.send(
                    "set_options",
                    json!({ "billingAutoCheck": false }),
                    "options",
                );
            }
        }

        // 提醒 1：剩余流量低于阈值
        ui.horizontal(|ui| {
            ui.label("流量提醒：当月剩余低于");
            let resp = ui.add(
                egui::DragValue::new(&mut self.billing_flow_alert_mb)
                    .range(0..=10_000_000)
                    .speed(64)
                    .suffix(" MB"),
            );
            ui.label("（0 = 关闭；1024 MB = 1 GB）");
            // 拖动时每帧 changed，但 dragged=true；松手那帧 changed 可能已经是 false，
            // 所以必须补上 drag_stopped / lost_focus，否则数值只改了界面、没落盘。
            let should_save =
                (resp.changed() && !resp.dragged()) || resp.drag_stopped() || resp.lost_focus();
            if should_save {
                self.bridge.send(
                    "set_options",
                    json!({ "billingFlowAlertMb": self.billing_flow_alert_mb }),
                    "options",
                );
            }
        });
        ui.horizontal(|ui| {
            let mut only = self.billing_flow_only_offline;
            if ui
                .add_enabled(
                    self.billing_flow_alert_mb > 0,
                    egui::Checkbox::new(&mut only, "流量提醒仅在无法出网时弹窗"),
                )
                .changed()
            {
                self.billing_flow_only_offline = only;
                self.bridge.send(
                    "set_options",
                    json!({ "billingFlowOnlyOffline": only }),
                    "options",
                );
            }
        });

        // 提醒 2：余额不足以支付套餐
        let mut bal = self.billing_balance_alert;
        if ui
            .checkbox(&mut bal, "余额提醒：余额不足以支付当前套餐价时弹窗")
            .changed()
        {
            self.billing_balance_alert = bal;
            self.bridge.send(
                "set_options",
                json!({ "billingBalanceAlert": bal }),
                "options",
            );
        }
        ui.horizontal(|ui| {
            let mut only = self.billing_balance_only_offline;
            if ui
                .add_enabled(
                    self.billing_balance_alert,
                    egui::Checkbox::new(&mut only, "余额提醒仅在无法出网时弹窗"),
                )
                .changed()
            {
                self.billing_balance_only_offline = only;
                self.bridge.send(
                    "set_options",
                    json!({ "billingBalanceOnlyOffline": only }),
                    "options",
                );
            }
        });

        if self.billing_confirm_auto {
            ui.colored_label(
                egui::Color32::from_rgb(200, 140, 0),
                "开启后，服务会在开机联网后用已保存的校园网密码登录计费系统查询一次（只读），按勾选条件弹窗提醒。",
            );
            ui.horizontal(|ui| {
                if ui.button("确认开启").clicked() {
                    self.billing_confirm_auto = false;
                    self.billing_auto = true;
                    self.bridge.send(
                        "set_options",
                        json!({ "billingAutoCheck": true }),
                        "options",
                    );
                }
                if ui.button("取消").clicked() {
                    self.billing_confirm_auto = false;
                }
            });
        }
        ui.add_space(6.0);
        if ui.button("恢复默认计费设置").clicked() {
            self.confirm_reset_billing = true;
        }
        if self.confirm_reset_billing {
            ui.colored_label(
                egui::Color32::from_rgb(200, 140, 0),
                "恢复默认会关闭自动查询、清空流量阈值、重置两种提醒开关；不影响账号密码。",
            );
            ui.horizontal(|ui| {
                if ui.button("确认恢复").clicked() {
                    self.confirm_reset_billing = false;
                    self.bridge.send(
                        "reset_options",
                        json!({ "scope": "billing" }),
                        "reset_billing",
                    );
                }
                if ui.button("取消").clicked() {
                    self.confirm_reset_billing = false;
                }
            });
        }
        ui.label(
            egui::RichText::new(
                "提示：开机自动查询只在服务启动时执行一次；开启后想立刻验证，点「立即检查并弹窗」。连续登录失败会要求验证码。",
            )
            .weak()
            .small(),
        );
    }

    fn logs_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("刷新").clicked() {
                self.bridge.send("get_logs", json!({"tail": 500}), "logs");
            }
            ui.checkbox(&mut self.auto_log, "自动刷新（2 秒）");
            ui.label(
                egui::RichText::new(format!("共 {} 行", self.log_lines.len()))
                    .weak()
                    .small(),
            );
        });
        ui.separator();
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                let mut text = self.log_lines.join("\n");
                ui.add(
                    egui::TextEdit::multiline(&mut text)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .interactive(false),
                );
            });
    }

    fn about_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("BJUTNet 校园网自动登录");
        ui.label(format!("面板版本: {}", cn_core::version::full()));
        if let Some(st) = &self.status {
            ui.label(format!("服务版本: {}", st.svc_version));
        }
        ui.add_space(6.0);
        egui::Grid::new("about_grid")
            .num_columns(2)
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                ui.label("数据目录");
                ui.label(paths::show(&paths::data_dir()));
                ui.end_row();
                ui.label("日志文件");
                ui.label(
                    cn_core::logging::current_path()
                        .map(|p| paths::show(&p))
                        .unwrap_or_else(|| "-".into()),
                );
                ui.end_row();
            });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("生成诊断信息").clicked() {
                self.bridge.send("get_diag", json!({}), "diag");
            }
            if ui.button("打开数据目录").clicked() {
                let dir = paths::data_dir();
                let _ = std::process::Command::new("explorer.exe").arg(dir).spawn();
            }
        });

        if let Some(diag) = &self.diag_text {
            ui.separator();
            let mut text = diag.clone();
            egui::ScrollArea::vertical()
                .max_height(260.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .interactive(false),
                    );
                });
        }
    }
}

impl eframe::App for App {
    // egui 0.36 起，App 的入口是 `ui(&mut Ui, ...)`（不再是 `update(&Context, ...)`），
    // 面板也改为"在一段 Ui 里 show"。
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain_events();
        self.ensure_cfg_loaded();

        // 日志页自动刷新
        if self.tab == Tab::Logs
            && self.auto_log
            && self.last_log_at.elapsed() > Duration::from_secs(2)
        {
            self.last_log_at = Instant::now();
            self.bridge.send("get_logs", json!({"tail": 500}), "logs");
        }

        // 首次进入计费页：拉一次缓存 + 开关状态
        if self.tab == Tab::Billing && !self.billing_loaded {
            self.billing_loaded = true;
            self.billing_loading = true;
            self.bridge
                .send("billing_status", json!({}), "billing_status");
        }

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(6.0);
            self.top_bar(ui);
            ui.add_space(4.0);
            self.tabs(ui);
            if self.svc_version_mismatch {
                if let Some(st) = &self.status {
                    ui.colored_label(
                        egui::Color32::from_rgb(200, 140, 0),
                        format!(
                            "服务端版本 {} 与面板 {} 不一致：新配置项可能不生效，请安装同一版本的完整包。",
                            st.svc_version,
                            cn_core::version::full()
                        ),
                    );
                }
            }
            ui.add_space(4.0);
        });

        if let Some((msg, at, is_err)) = self.toast.clone() {
            if at.elapsed() < Duration::from_secs(6) {
                egui::Panel::bottom("toast").show(ui, |ui| {
                    let color = if is_err {
                        egui::Color32::from_rgb(200, 60, 60)
                    } else {
                        egui::Color32::from_rgb(46, 160, 67)
                    };
                    ui.colored_label(color, msg);
                });
            }
        }

        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Status => self.status_tab(ui),
            Tab::Settings => self.settings_tab(ui),
            Tab::Billing => self.billing_tab(ui),
            Tab::Logs => self.logs_tab(ui),
            Tab::About => self.about_tab(ui),
        });

        // 状态轮询由后台线程驱动，这里留个兜底重绘
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

fn format_duration(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h} 小时 {m} 分")
    } else if m > 0 {
        format!("{m} 分 {s} 秒")
    } else {
        format!("{s} 秒")
    }
}
