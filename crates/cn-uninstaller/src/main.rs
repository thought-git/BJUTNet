//! BJUTNet 卸载程序。
//!
//! 用法：
//!   BJUTNetUninstall.exe                交互式（弹确认框，自动提权）
//!   BJUTNetUninstall.exe --quiet        静默（安装器/脚本用）
//!   BJUTNetUninstall.exe --keep-data    保留 %ProgramData%\BJUTNet（默认保留）
//!   BJUTNetUninstall.exe --purge-data   连配置和日志一起删
//!   BJUTNetUninstall.exe --cleanup-dir <安装目录> --wait-pid <父PID>   （内部用：
//!       先把自己复制到 %TEMP%，等父进程退出后删安装目录，再登记重启自删）

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};

use cn_core::{app, net, paths, shell, svcctl, winreg};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |f: &str| args.iter().any(|a| a.eq_ignore_ascii_case(f));
    let quiet = has("--quiet") || has("--silent");
    let purge_data = has("--purge-data");

    // ---- 子进程模式：等父进程退出后清理 ----
    if let Some(pid) = value(&args, "--wait-pid").and_then(|v| v.parse::<u32>().ok()) {
        let dir = value(&args, "--cleanup-dir")
            .map(PathBuf::from)
            .unwrap_or_else(|| app::default_install_dir());
        // 兜底：如果自己就在待删目录里，先把自己挪到 %LOCALAPPDATA% 再删
        if let Ok(me) = std::env::current_exe() {
            if me.starts_with(&dir) {
                let copy = paths::local_app_dir().join("BJUTNetUninstall_tmp.exe");
                if std::fs::copy(&me, &copy).is_ok() {
                    let mut c = std::process::Command::new(&copy);
                    c.arg("--quiet")
                        .arg("--cleanup-dir").arg(&dir)
                        .arg("--wait-pid").arg(pid.to_string());
                    if purge_data { c.arg("--purge-data"); }
                    if c.spawn().is_ok() { std::process::exit(0); }
                }
            }
        }
        shell::wait_process_exit(pid, 60_000);
        std::thread::sleep(std::time::Duration::from_millis(400));
        let code = cleanup(&dir, purge_data, quiet);
        if let Ok(me) = std::env::current_exe() {
            let _ = shell::schedule_delete(&me);
        }
        std::process::exit(code);
    }

    let self_exe = std::env::current_exe().unwrap_or_default();
    let install_dir = self_exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(app::default_install_dir);

    if !quiet {
        if !net::is_elevated() {
            match shell::relaunch_as_admin(&args) {
                Ok(()) => return,
                Err(e) => {
                    cn_common::winutil::info(
                        &format!("需要管理员权限才能卸载。\n\n{e}"),
                        "卸载 BJUTNet",
                    );
                    return;
                }
            }
        }
        let msg = format!(
            "确定要卸载 BJUTNet 吗？\n\n安装目录：{}\n服务：{}（会停止并删除）\n\n{}",
            install_dir.display(),
            app::SERVICE_NAME,
            if purge_data {
                "配置和日志也会被删除。"
            } else {
                "配置和日志（%ProgramData%\\BJUTNet）会保留，重装后仍然可用。"
            }
        );
        if !cn_common::winutil::confirm(&msg, "卸载 BJUTNet") {
            return;
        }
    }

    // 自己就在安装目录里 → 复制到 %TEMP% 再跑（否则删不掉自己）
    if self_exe.starts_with(&install_dir) {
        let tmp = std::env::temp_dir().join(format!(
            "BJUTNetUninstall_{}.exe",
            std::process::id()
        ));
        if std::fs::copy(&self_exe, &tmp).is_ok() {
            let mut cmd = std::process::Command::new(&tmp);
            cmd.arg("--quiet")
                .arg("--cleanup-dir")
                .arg(&install_dir)
                .arg("--wait-pid")
                .arg(std::process::id().to_string());
            if purge_data {
                cmd.arg("--purge-data");
            }
            if cmd.spawn().is_ok() {
                return;
            }
        }
    }

    let code = cleanup(&install_dir, purge_data, quiet);
    std::process::exit(code);
}

fn cleanup(install_dir: &Path, purge_data: bool, quiet: bool) -> i32 {
    let mut ok = true;

    // 1. 结束面板进程
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/IM", app::GUI_EXE, "/T"])
        .output();

    // 2. 停删服务
    match svcctl::remove(app::SERVICE_NAME) {
        Ok(()) => say(quiet, "服务已停止并删除"),
        Err(e) => {
            ok = false;
            say(quiet, &format!("✗ 删除服务失败: {e}"));
        }
    }

    // 3. 快捷方式（按安装清单里的选择删）
    let meta = std::fs::read_to_string(install_dir.join(app::INSTALL_META))
        .ok()
        .and_then(|t| serde_json::from_str::<app::InstallMeta>(&t).ok())
        .unwrap_or_default();
    if meta.desktop_shortcut || !meta.desktop_shortcut {
        // 无论清单怎么写，都尝试删一遍（不存在就当成功）
        for dir in [
            cn_common::winutil::desktop_dir(),
            cn_common::winutil::start_menu_programs_dir(),
        ] {
            if let Some(d) = dir {
                let lnk = Path::new(&d).join(format!("{}.lnk", app::DISPLAY_NAME));
                let _ = std::fs::remove_file(lnk);
            }
        }
        say(quiet, "快捷方式已清理");
    }

    // 4. 卸载注册项
    match winreg::remove_uninstall(app::APP_ID) {
        Ok(()) => say(quiet, "卸载注册项已删除"),
        Err(e) => {
            ok = false;
            say(quiet, &format!("✗ 删除卸载注册项失败: {e}"));
        }
    }

    // 5. 数据目录
    if purge_data {
        let data = paths::data_dir();
        match std::fs::remove_dir_all(&data) {
            Ok(()) => say(quiet, &format!("已删除 {}", data.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => say(quiet, &format!("⚠ 删除数据目录失败: {e}")),
        }
    } else {
        say(quiet, "已保留配置与日志（%ProgramData%\\BJUTNet）");
    }

    // 6. 安装目录
    match std::fs::remove_dir_all(install_dir) {
        Ok(()) => say(quiet, &format!("已删除 {}", install_dir.display())),
        Err(e) => {
            // 可能还有文件被占用：逐个登记重启删除
            ok = false;
            say(quiet, &format!("⚠ 删除安装目录失败（登记重启后删除）: {e}"));
            if let Ok(entries) = std::fs::read_dir(install_dir) {
                for e in entries.flatten() {
                    let _ = shell::schedule_delete(&e.path());
                }
            }
        }
    }

    if !quiet {
        cn_common::winutil::info(
            if ok {
                "BJUTNet 已卸载。"
            } else {
                "BJUTNet 已卸载，但有部分项目未能完全清理（详见日志文件）。"
            },
            "卸载 BJUTNet",
        );
    }
    if ok { 0 } else { 2 }
}

fn say(quiet: bool, line: &str) {
    if !quiet {
        println!("{line}");
    }
    let path = paths::tool_log("uninstall");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write as _;
        let _ = writeln!(f, "{line}");
    }
}

fn value(args: &[String], key: &str) -> Option<String> {
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
