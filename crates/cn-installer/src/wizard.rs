//! 安装向导（图形界面）。

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use eframe::egui;
use cn_common::payload::GameManifest;

use cn_core::{app, net, shell};

use crate::steps::{self, Options};

enum Evt {
    Log(String),
    Pct(f32),
    Done(Result<Vec<String>, String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Welcome,
    Working,
    Done,
    Error,
    NoPayload,
}

pub struct Wizard {
    page: Page,
    args: Vec<String>,
    manifest: Option<GameManifest>,
    payload: Option<Vec<u8>>,
    payload_err: String,
    opts: Options,
    dir_text: String,
    log: Vec<String>,
    pct: f32,
    rx: Option<Receiver<Evt>>,
    error: String,
    elevated: bool,
    /// 完成页出现的时间（用于自动关窗）
    done_at: Option<std::time::Instant>,
}

impl Wizard {
    pub fn new(
        payload: Result<(GameManifest, Vec<u8>), String>,
        args: Vec<String>,
    ) -> Self {
        let (manifest, bytes, err, page) = match payload {
            Ok((m, p)) => (Some(m), Some(p), String::new(), Page::Welcome),
            Err(e) => (None, None, e, Page::NoPayload),
        };
        let mut opts = Options::default();
        // 从命令行带过来的选项（--dir / --no-desktop 等）也生效
        let has = |f: &str| args.iter().any(|a| a.eq_ignore_ascii_case(f));
        opts.desktop = !has("--no-desktop");
        opts.start_menu = !has("--no-startmenu");
        opts.launch = !has("--no-launch");
        if let Some(i) = args.iter().position(|a| a.eq_ignore_ascii_case("--dir")) {
            if let Some(v) = args.get(i + 1) {
                opts.dir = std::path::PathBuf::from(v);
            }
        }
        let dir_text = opts.dir.display().to_string();
        let elevated = net::is_elevated();
        Self {
            page,
            args,
            manifest,
            payload: bytes,
            payload_err: err,
            opts,
            dir_text,
            log: Vec::new(),
            pct: 0.0,
            rx: None,
            error: String::new(),
            elevated,
            done_at: None,
        }
    }

    fn start(&mut self) {
        let (Some(manifest), Some(payload)) = (self.manifest.clone(), self.payload.clone()) else {
            return;
        };
        self.opts.dir = PathBuf::from(self.dir_text.trim());
        if self.opts.dir.as_os_str().is_empty() {
            self.dir_text = app::default_install_dir().display().to_string();
            self.opts.dir = app::default_install_dir();
        }
        let opts = self.opts.clone();

        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.page = Page::Working;
        self.pct = 0.0;
        self.log.clear();
        self.error.clear();

        std::thread::spawn(move || {
            let tx_log = tx.clone();
            let tx_pct = tx.clone();
            let r = steps::run(
                &opts,
                &manifest,
                &payload,
                &mut |s| {
                    let _ = tx_log.send(Evt::Log(s.to_string()));
                },
                |p| {
                    let _ = tx_pct.send(Evt::Pct(p));
                },
            );
            let _ = tx.send(Evt::Done(r));
        });
    }

    fn drain(&mut self) {
        let mut done = None;
        if let Some(rx) = &self.rx {
            while let Ok(e) = rx.try_recv() {
                match e {
                    Evt::Log(s) => self.log.push(s),
                    Evt::Pct(p) => self.pct = p,
                    Evt::Done(r) => {
                        done = Some(r);
                        break;
                    }
                }
            }
        }
        if let Some(r) = done {
            self.rx = None;
            match r {
                Ok(lines) => {
                    self.log = lines;
                    self.pct = 1.0;
                    self.page = Page::Done;
                    self.done_at = Some(std::time::Instant::now());
                }
                Err(e) => {
                    self.error = e;
                    self.page = Page::Error;
                }
            }
        }
    }

    fn elevate_or_exit(&mut self, ctx: &egui::Context) {
        let mut args = self.args.clone();
        // 原样把参数传给提权后的实例
        if args.is_empty() {
            args.push("--elevated".to_string());
        }
        match shell::relaunch_as_admin(&args) {
            Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(e) => self.error = e,
        }
    }

    fn welcome(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.heading("安装 BJUTNet 校园网自动登录");
        ui.add_space(4.0);
        ui.label(
            "安装内容：\n\
             · BJUTNet 面板（可选打开，关掉不影响登录）\n\
             · BJUTNetSvc 系统服务（开机自启、自动识别宿舍/无线网络并登录）\n\
             · 卸载程序与卸载注册项",
        );
        ui.add_space(8.0);

        egui::Grid::new("dir_grid")
            .num_columns(3)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                ui.label("安装目录");
                ui.add(
                    egui::TextEdit::singleline(&mut self.dir_text).desired_width(380.0),
                );
                if ui.button("浏览…").clicked() {
                    let start = PathBuf::from(&self.dir_text);
                    if let Some(p) = cn_common::winutil::pick_folder(&start.to_string_lossy()) {
                        self.dir_text = p;
                    }
                }
                ui.end_row();
            });

        ui.add_space(6.0);
        ui.checkbox(&mut self.opts.desktop, "创建桌面快捷方式");
        ui.checkbox(&mut self.opts.start_menu, "创建开始菜单快捷方式");
        ui.checkbox(&mut self.opts.launch, "安装完成后打开面板");

        ui.add_space(10.0);
        if !self.elevated {
            ui.colored_label(
                egui::Color32::from_rgb(200, 140, 0),
                "⚠ 当前不是管理员：点「开始安装」会弹出 UAC 请求提权",
            );
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let label = if self.elevated { "开始安装" } else { "以管理员身份安装" };
            if ui.button(egui::RichText::new(label).size(16.0)).clicked() {
                if self.elevated {
                    self.start();
                } else {
                    self.elevate_or_exit(ctx);
                }
            }
            if ui.button("退出").clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
        if !self.error.is_empty() {
            ui.add_space(6.0);
            ui.colored_label(egui::Color32::from_rgb(200, 60, 60), &self.error);
        }
    }

    fn working(&mut self, ui: &mut egui::Ui) {
        ui.heading("正在安装…");
        ui.add_space(8.0);
        ui.add(egui::ProgressBar::new(self.pct).show_percentage());
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                let mut text = self.log.join("\n");
                ui.add(
                    egui::TextEdit::multiline(&mut text)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .interactive(false),
                );
            });
    }

    fn done(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        ui.colored_label(
            egui::Color32::from_rgb(46, 160, 67),
            egui::RichText::new("✓ 安装完成").size(18.0),
        );
        ui.add_space(4.0);
        ui.label(
            "服务已设为开机自启（延迟启动），面板已自动打开。\n\
             以后不需要常开面板：服务会自己检测网络并登录；面板只是查看/修改设置用的。\n\n\
             本窗口 3 秒后自动关闭。",
        );
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .max_height(240.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut text = self.log.join("\n");
                ui.add(
                    egui::TextEdit::multiline(&mut text)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .interactive(false),
                );
            });
    }

    fn error_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.colored_label(
            egui::Color32::from_rgb(200, 60, 60),
            egui::RichText::new("✗ 安装失败").size(18.0),
        );
        ui.add_space(4.0);
        ui.label(&self.error);
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .max_height(240.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut text = self.log.join("\n");
                ui.add(
                    egui::TextEdit::multiline(&mut text)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .interactive(false),
                );
            });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("返回").clicked() {
                self.page = Page::Welcome;
            }
            if ui.button("退出").clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }
}

impl eframe::App for Wizard {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain();

        egui::CentralPanel::default().show(ui, |ui| match self.page {
            Page::Welcome => self.welcome(ui, &ctx),
            Page::Working => self.working(ui),
            Page::Done => self.done(ui, &ctx),
            Page::Error => self.error_page(ui, &ctx),
            Page::NoPayload => {
                ui.heading("无法安装");
                ui.add_space(6.0);
                ui.colored_label(egui::Color32::from_rgb(200, 60, 60), &self.payload_err);
                ui.add_space(10.0);
                ui.label("请使用打包好的 BJUTNetSetup-*.exe 运行（当前这个 exe 不含安装载荷）。");
                if ui.button("退出").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        });

        match self.page {
            Page::Working => ctx.request_repaint_after(std::time::Duration::from_millis(120)),
            Page::Done => {
                // 完成页没有按钮，3 秒后自己关（面板已经自动打开了）
                if self
                    .done_at
                    .map(|t| t.elapsed() >= std::time::Duration::from_secs(3))
                    .unwrap_or(false)
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(200));
                }
            }
            _ => {}
        }
    }
}
