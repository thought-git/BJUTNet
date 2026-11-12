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
    Logs,
    About,
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
        if let Some(cfg) = Config::load_existing(&paths::config_path()) {
            self.account = cfg.account.clone();
            self.check_interval = cfg.check_interval_sec;
            self.offline_interval = cfg.offline_interval_sec;
            self.probe_timeout = cfg.probe_timeout_sec;
            self.log_level = cfg.log_level.clone();
            self.auto_wifi = cfg.auto_connect_wifi;
            self.wifi_profile = cfg.wifi_profile.clone();
            self.cfg_loaded = true;
        }
    }

    fn drain_events(&mut self) {
        while let Ok(ev) = self.bridge.rx.try_recv() {
            match ev {
                UiEvent::Status(Ok(st)) => {
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
                        self.toast(format!("{label} 失败: {}", resp.error), true);
                    } else {
                        match label {
                            "logs" => {
                                if let Some(arr) = resp.data.get("lines").and_then(|v| v.as_array()) {
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
                                let msg = resp
                                    .data
                                    .get("msg")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(if added {
                                        "已加入标记列表，服务会立刻按校园网重新判别"
                                    } else {
                                        "这张网络已经在标记列表里了"
                                    });
                                self.toast(msg.to_string(), false);
                            }
                            "verify_login" => {
                                self.toast("已注销并开始重新登录，几秒后看状态/最近错误", false);
                            }
                            "trusted-remove" => {
                                self.toast("已移除该标记", false);
                            }
                            "credentials" => {
                                self.cfg_loaded = false; // 重新拉一次配置
                                let online = self
                                    .status
                                    .as_ref()
                                    .map(|s| s.online)
                                    .unwrap_or(false);
                                if online {
                                    self.toast(
                                        "已保存。当前在线，密码要等掉线才会验证；想立刻确认请点「重连并验证」",
                                        false,
                                    );
                                } else {
                                    self.toast("已保存，服务正在用新密码登录，稍等看状态页", false);
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
                    self.toast(format!("{label} 失败: {e}"), true);
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
                    format!(
                        "{} · {} · {}",
                        st.state,
                        st.net_kind,
                        if st.online { "出网正常" } else { "出网异常" }
                    ),
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
                row("网络", st.net_kind.clone());
                row(
                    "判别依据",
                    if st.net_evidence.is_empty() {
                        "-".into()
                    } else {
                        st.net_evidence.clone()
                    },
                );
                row("SSID", if st.ssid.is_empty() { "-".into() } else { st.ssid.clone() });
                row("本机 IP", if st.ip.is_empty() { "-".into() } else { st.ip.clone() });
                row("网关", if st.gateway.is_empty() { "-".into() } else { st.gateway.clone() });
                row("MAC", if st.mac.is_empty() { "-".into() } else { st.mac.clone() });
                row("账号", if st.account.is_empty() { "<未配置>".into() } else { st.account.clone() });
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
                row(
                    "暂停",
                    if st.paused_forever {
                        "手动暂停（直到点恢复）".into()
                    } else if st.paused_until_epoch > 0 {
                        chrono::DateTime::from_timestamp(st.paused_until_epoch, 0)
                            .map(|t| {
                                format!(
                                    "到 {}",
                                    t.with_timezone(&chrono::Local).format("%H:%M:%S")
                                )
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
            if ui.button("暂停直到恢复").on_hover_text(
                "不会自动恢复：点「恢复自动登录」或重启服务才解除",
            ).clicked() {
                self.bridge.send("pause", json!({"forever": true}), "pause");
            }
            ui.separator();
            if ui.button("关闭服务").on_hover_text(
                "结束服务进程（本次运行）。服务启动类型仍是「自动」，下次开机会自己起来",
            ).clicked() {
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
            if ui.button("宿舍有线").clicked() {
                self.bridge
                    .send("add_trusted_current", json!({"kind": "dorm"}), "trusted");
            }
            if ui.button("公共无线").clicked() {
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
                ui.checkbox(&mut self.auto_wifi, "允许（默认关，避免把宿舍有线踢掉）");
                ui.end_row();

                ui.label("校园 WiFi 名称");
                ui.add(
                    egui::TextEdit::singleline(&mut self.wifi_profile).desired_width(200.0),
                );
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
                    "（无）如果服务把当前网络认成「非校园网」，到「状态」页点「宿舍有线 / 公共无线」按钮即可加入。",
                )
                .weak(),
            );
        } else {
            for (i, t) in trusted.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("{}. {}", i + 1, t.describe()));
                    if ui.button("删除").clicked() {
                        self.bridge.send(
                            "remove_trusted",
                            json!({"index": i}),
                            "trusted-remove",
                        );
                    }
                });
            }
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
        });
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

        // 日志页自动刷新
        if self.tab == Tab::Logs
            && self.auto_log
            && self.last_log_at.elapsed() > Duration::from_secs(2)
        {
            self.last_log_at = Instant::now();
            self.bridge.send("get_logs", json!({"tail": 500}), "logs");
        }

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(6.0);
            self.top_bar(ui);
            ui.add_space(4.0);
            self.tabs(ui);
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
