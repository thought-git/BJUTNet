//! 安装步骤：GUI 向导与 `--silent` 共用，本身不含任何 UI 依赖。
//!
//! 每一步都往 `log()` 里写一行，向导页实时显示；失败时按相反顺序回滚。

use std::io::Write as _;
use std::path::{Path, PathBuf};

use cn_core::{app, net, shell, svcctl, winreg};
use cn_common::payload::{extract_payload, GameManifest};

#[derive(Clone)]
pub struct Options {
    pub dir: PathBuf,
    pub desktop: bool,
    pub start_menu: bool,
    pub launch: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            dir: app::default_install_dir(),
            desktop: true,
            start_menu: true,
            launch: true,
        }
    }
}

pub type LogFn<'a> = &'a mut dyn FnMut(&str);

/// 从自身 exe 读出 payload
pub fn read_self_payload() -> Result<(GameManifest, Vec<u8>), String> {
    let exe = std::env::current_exe().map_err(|e| format!("取自身路径失败: {e}"))?;
    cn_common::payload::read_payload(&exe).ok_or_else(|| {
        "这个 exe 里没有安装载荷（它是模板，不是打包后的 BJUTNetSetup-*.exe）".to_string()
    })
}

/// 执行安装。返回安装日志（供完成页展示）。
pub fn run(
    opts: &Options,
    manifest: &GameManifest,
    payload: &[u8],
    log: LogFn,
    mut progress: impl FnMut(f32),
) -> Result<Vec<String>, String> {
    let mut lines: Vec<String> = Vec::new();
    {
        let mut say = |s: String| {
            log(&s);
            lines.push(s);
        };

        // 1. 权限
        progress(0.01);
        say(format!("目标目录: {}", opts.dir.display()));
        if !net::is_elevated() {
            return Err("需要管理员权限（装到 Program Files / 注册服务都要管理员）".into());
        }

        // 2. 收尾正在运行的旧进程/服务 + 迁移上一代（CampusNet）
        progress(0.04);
        stop_old(&mut say)?;
        migrate_legacy(&mut say);

        // 3. 解包
        progress(0.10);
        say(format!("安装目录: {}", opts.dir.display()));
        if let Err(e) = std::fs::create_dir_all(&opts.dir) {
            return Err(format!("创建安装目录失败: {e}"));
        }
        let base = 0.10f32;
        let span = 0.55f32;
        let total = manifest.files.len().max(1);
        let mut last_pct = 0.0f32;
        if let Err(e) = extract_payload(manifest, payload, &opts.dir, |done, total, path| {
            let p = base + span * (done as f32 / total as f32);
            if p - last_pct >= 0.05 || done == total {
                last_pct = p;
                progress(p);
            }
            if done % 8 == 0 || done == total {
                eprintln!("  解包 {done}/{total}  {path}");
            }
        }) {
            cleanup_partial(opts, &mut say);
            return Err(format!("解包失败: {e}"));
        }
        say(format!("已解包 {total} 个文件"));

        // 5. 安装清单（卸载器要读）
        progress(0.70);
        let meta = app::InstallMeta {
            version: manifest.version.clone(),
            install_dir: opts.dir.to_string_lossy().to_string(),
            desktop_shortcut: opts.desktop,
            start_menu_shortcut: opts.start_menu,
            service_name: app::SERVICE_NAME.to_string(),
            installed_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        };
        if let Err(e) = std::fs::write(
            opts.dir.join(app::INSTALL_META),
            serde_json::to_string_pretty(&meta).unwrap_or_default(),
        ) {
            cleanup_partial(opts, &mut say);
            return Err(format!("写安装清单失败: {e}"));
        }
        let _ = std::fs::write(opts.dir.join(app::VERSION_FILE), &manifest.version);

        // 6. 快捷方式
        progress(0.76);
        if let Err(e) = create_shortcuts(opts, &mut say) {
            cleanup_partial(opts, &mut say);
            return Err(e);
        }

        // 7. HKLM 卸载项
        progress(0.82);
        let icon = opts.dir.join(app::GUI_EXE);
        let uninst = opts.dir.join(app::UNINST_EXE);
        if let Err(e) = winreg::write_uninstall(
            app::APP_ID,
            app::DISPLAY_NAME,
            &manifest.version,
            app::PUBLISHER,
            &opts.dir.to_string_lossy(),
            &icon.to_string_lossy(),
            &format!("\"{}\"", uninst.display()),
        ) {
            cleanup_partial(opts, &mut say);
            return Err(e);
        }
        say("已注册卸载项（HKLM）".into());

        // 8. 服务
        progress(0.86);
        let svc = opts.dir.join(app::SVC_EXE);
        if !svc.is_file() {
            cleanup_partial(opts, &mut say);
            return Err(format!("载荷里没有 {}", app::SVC_EXE));
        }
        let bin_path = format!("\"{}\" --service", svc.display());
        // 不开"延迟自动启动"：延迟启动会让开机后 1~2 分钟内没人登录。
        // 普通自动启动 + 服务内部"等网络就绪"重试，开机后几秒内就能登录。
        if let Err(e) = svcctl::install(
            app::SERVICE_NAME,
            app::SERVICE_DISPLAY,
            app::SERVICE_DESC,
            &bin_path,
            false,
        ) {
            cleanup_partial(opts, &mut say);
            return Err(e);
        }
        say(format!("服务已注册: {}", bin_path));
        match svcctl::start(app::SERVICE_NAME) {
            Ok(()) => say("服务已启动（RUNNING），开机后会自动登录".into()),
            Err(e) => {
                // 服务起不来不算安装失败：文件都在，用户还能开面板看日志
                say(format!("⚠ 服务没能启动: {e}"));
                say("  可以在面板里看日志，或命令行跑 BJUTNetSvc.exe --console 排查".into());
            }
        }

        // 9. 启动面板（以普通用户身份）
        progress(0.95);
        if opts.launch {
            let gui = opts.dir.join(app::GUI_EXE);
            match shell::launch_as_invoker(&gui) {
                Ok(()) => say("已启动面板".into()),
                Err(e) => say(format!("⚠ 启动面板失败: {e}（可以从开始菜单手动打开）")),
            }
        }

        progress(1.0);
        say("安装完成".into());
    }
    Ok(lines)
}

fn stop_old(say: &mut dyn FnMut(String)) -> Result<(), String> {
    // 先停服务（避免它把进程又拉起来）
    if svcctl::service_state(app::SERVICE_NAME).is_some() {
        say("检测到已安装的 BJUTNetSvc，先停止".into());
        let _ = svcctl::stop(app::SERVICE_NAME);
    }
    for name in [app::GUI_EXE, app::SVC_EXE] {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/IM", name, "/T"])
            .output();
    }
    // 旧方案的服务
    if svcctl::service_state(app::LEGACY_SERVICE).is_some() {
        say(format!(
            "⚠ 发现旧方案的 {} 服务，正在停止并删除（否则两个客户端抢同一会话）",
            app::LEGACY_SERVICE
        ));
        match svcctl::remove(app::LEGACY_SERVICE) {
            Ok(()) => say("旧服务已移除".into()),
            Err(e) => say(format!("⚠ 旧服务移除失败（可稍后手动 sc delete）: {e}")),
        }
    }
    Ok(())
}

fn create_shortcuts(opts: &Options, say: &mut dyn FnMut(String)) -> Result<(), String> {
    let gui = opts.dir.join(app::GUI_EXE);
    let target = gui.to_string_lossy().to_string();
    cn_common::winutil::com_init().map_err(|e| format!("COM 初始化失败: {e}"))?;

    if opts.desktop {
        if let Some(desk) = cn_common::winutil::desktop_dir() {
            let lnk = Path::new(&desk).join(format!("{}.lnk", app::DISPLAY_NAME));
            cn_common::winutil::create_shortcut(
                &lnk.to_string_lossy(),
                &target,
                "",
                &target,
                "校园网自动登录面板",
            )
            .map_err(|e| format!("创建桌面快捷方式失败: {e}"))?;
            say(format!("桌面快捷方式: {}", lnk.display()));
        }
    }
    if opts.start_menu {
        if let Some(sm) = cn_common::winutil::start_menu_programs_dir() {
            let lnk = Path::new(&sm).join(format!("{}.lnk", app::DISPLAY_NAME));
            cn_common::winutil::create_shortcut(
                &lnk.to_string_lossy(),
                &target,
                "",
                &target,
                "校园网自动登录面板",
            )
            .map_err(|e| format!("创建开始菜单快捷方式失败: {e}"))?;
            say(format!("开始菜单快捷方式: {}", lnk.display()));
        }
    }
    Ok(())
}

/// 失败回滚：服务 → 卸载项 → 快捷方式 → 目录
fn cleanup_partial(opts: &Options, say: &mut dyn FnMut(String)) {
    say("安装失败，开始回滚…".into());
    let _ = svcctl::remove(app::SERVICE_NAME);
    let _ = winreg::remove_uninstall(app::APP_ID);
    remove_shortcuts();
    let _ = std::fs::remove_dir_all(&opts.dir);
}

pub fn remove_shortcuts() {
    for dir in [
        cn_common::winutil::desktop_dir(),
        cn_common::winutil::start_menu_programs_dir(),
    ] {
        if let Some(d) = dir {
            let lnk = Path::new(&d).join(format!("{}.lnk", app::DISPLAY_NAME));
            let _ = std::fs::remove_file(lnk);
        }
    }
}

/// 命令行输出同时写 `%TEMP%\BJUTNetSetup.log`（release 是 windows 子系统，stdout 可能不可见）
pub fn log_line(s: &str) {
    println!("{s}");
    let path = cn_core::paths::tool_log("setup");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{s}");
    }
}

/// 把上一代（CampusNet）的东西清掉，并把数据搬过来。
///
/// 两个服务同名会打架（都去登录同一个账号），旧目录留着也会让"应用和功能"里出现两条，
/// 所以升级时主动收尾。
fn migrate_legacy(say: &mut dyn FnMut(String)) {
    // 旧进程
    for exe in [app::LEGACY_GUI_EXE, app::LEGACY_SVC_EXE] {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/IM", exe, "/T"])
            .output();
    }
    // 旧服务（含更早的 NetSvc）
    for svc in [app::LEGACY_SERVICE, app::LEGACY_SERVICE_NETSVC] {
        if svc.is_empty() {
            continue;
        }
        if svcctl::service_state(svc).is_some() {
            say(format!("发现旧服务 {svc}，停止并删除"));
            match svcctl::remove(svc) {
                Ok(()) => say(format!("  已移除旧服务 {svc}")),
                Err(e) => say(format!("  ⚠ 旧服务 {svc} 移除失败: {e}")),
            }
        }
    }
    // 旧安装目录
    let old_dir = app::program_files().join(app::LEGACY_APP_ID);
    if old_dir.is_dir() {
        say(format!("清理旧安装目录: {}", old_dir.display()));
        let _ = std::fs::remove_dir_all(&old_dir);
    }
    // 旧卸载项
    let _ = winreg::remove_uninstall(app::LEGACY_APP_ID);

    // 数据迁移：旧 ProgramData 目录 → 新目录（只在新的还没有配置时搬）
    let old_data = app::program_data().join(app::LEGACY_DATA_SUBDIR);
    let new_data = cn_core::paths::data_dir();
    if old_data.is_dir() && !new_data.join("config.json").exists() {
        say(format!(
            "迁移配置数据: {} → {}",
            old_data.display(),
            new_data.display()
        ));
        let _ = std::fs::create_dir_all(&new_data);
        for name in ["config.json", "secrets.dat"] {
            let from = old_data.join(name);
            if from.is_file() {
                match std::fs::copy(&from, new_data.join(name)) {
                    Ok(_) => say(format!("  已迁移 {name}")),
                    Err(e) => say(format!("  ⚠ 迁移 {name} 失败: {e}")),
                }
            }
        }
    }
}
