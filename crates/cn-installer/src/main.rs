//! BJUTNet 安装程序（单文件 exe）。
//!
//! 用法：
//!   BJUTNetSetup-1.0.0.exe                    双击 → 图形向导（会请求管理员）
//!   BJUTNetSetup-1.0.0.exe --selftest         检查内置载荷（不需要管理员）
//!   BJUTNetSetup-1.0.0.exe --extract-to <dir> 只解包（不需要管理员，调试用）
//!   BJUTNetSetup-1.0.0.exe --silent [--dir X] [--desktop|--no-desktop]
//!                            [--startmenu|--no-startmenu] [--no-launch]
//!   日志（release 版本控制台不可见时）: %TEMP%\BJUTNetSetup.log

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod fonts;
mod steps;
mod wizard;

use std::path::PathBuf;

use eframe::egui;

use cn_core::{net, shell};
use steps::{Options, log_line};

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = cli(&args) {
        std::process::exit(code);
    }

    // 图形向导默认提权：双击就开始 UAC，而不是等点了"安装"才弹。
    // 环境变量 BJUTNET_SETUP_NO_ELEVATE=1 仅供自动化测试（不弹 UAC，只开窗口）。
    let test_no_elevate = std::env::var("BJUTNET_SETUP_NO_ELEVATE").ok().as_deref() == Some("1");
    if !net::is_elevated() && !test_no_elevate {
        match shell::relaunch_as_admin(&args) {
            Ok(()) => return Ok(()),
            Err(e) => eprintln!("提权失败（将以只读向导继续）: {e}"),
        }
    }

    let payload = steps::read_self_payload();
    let viewport = egui::ViewportBuilder::default()
        .with_title("BJUTNet 安装程序")
        .with_inner_size([720.0, 560.0])
        .with_min_inner_size([640.0, 480.0])
        .with_icon(fonts::window_icon());

    eframe::run_native(
        "BJUTNetSetup",
        eframe::NativeOptions {
            viewport,
            ..Default::default()
        },
        Box::new(move |cc| {
            fonts::setup(&cc.egui_ctx);
            Ok(Box::new(wizard::Wizard::new(payload, args)))
        }),
    )
}

fn has(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a.eq_ignore_ascii_case(flag))
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

fn cli(args: &[String]) -> Option<i32> {
    if has(args, "--help") || has(args, "-h") {
        log_line("BJUTNet 安装程序");
        log_line("  (无参数)                图形向导");
        log_line("  --selftest              检查内置载荷");
        log_line("  --extract-to <dir>      只解包到目录（不需要管理员）");
        log_line("  --silent [--dir <dir>]  静默安装");
        log_line("           [--desktop|--no-desktop] [--startmenu|--no-startmenu]");
        log_line("           [--no-launch]");
        return Some(0);
    }

    if has(args, "--selftest") {
        return Some(match steps::read_self_payload() {
            Ok((m, payload)) => {
                log_line("✓ 找到安装载荷");
                log_line(&format!("  版本: {}", m.version));
                log_line(&format!("  主程序: {}", m.main_exe));
                log_line(&format!("  文件数: {}", m.files.len()));
                let raw: u64 = m.files.iter().map(|f| f.size).sum();
                log_line(&format!(
                    "  原始大小: {:.2} MB / 载荷 {:.2} MB",
                    raw as f64 / 1048576.0,
                    payload.len() as f64 / 1048576.0
                ));
                for f in &m.files {
                    log_line(&format!("  {:>10}  {}", f.size, f.path));
                }
                0
            }
            Err(e) => {
                log_line(&format!("✗ {e}"));
                2
            }
        });
    }

    if let Some(dir) = value(args, "--extract-to") {
        let dir = PathBuf::from(dir);
        let (m, payload) = match steps::read_self_payload() {
            Ok(v) => v,
            Err(e) => {
                log_line(&format!("✗ {e}"));
                return Some(2);
            }
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log_line(&format!("✗ 创建目录失败: {e}"));
            return Some(2);
        }
        let mut n = 0usize;
        let r = cn_common::payload::extract_payload(&m, &payload, &dir, |i, total, path| {
            n = i;
            if i % 8 == 0 || i == total {
                log_line(&format!("  {i}/{total} {path}"));
            }
        });
        match r {
            Ok(()) => {
                log_line(&format!(
                    "✓ 已解包 {n} 个文件到 {}",
                    dir.display()
                ));
                Some(0)
            }
            Err(e) => {
                log_line(&format!("✗ 解包失败: {e}"));
                Some(2)
            }
        }
    } else if has(args, "--silent") || has(args, "--quiet") {
        Some(silent(args))
    } else {
        None
    }
}

fn silent(args: &[String]) -> i32 {
    let (manifest, payload) = match steps::read_self_payload() {
        Ok(v) => v,
        Err(e) => {
            log_line(&format!("✗ {e}"));
            return 2;
        }
    };
    let mut opts = Options::default();
    if let Some(d) = value(args, "--dir") {
        opts.dir = PathBuf::from(d);
    }
    opts.desktop = !has(args, "--no-desktop");
    opts.start_menu = !has(args, "--no-startmenu");
    opts.launch = !has(args, "--no-launch");

    let mut progress = |_p: f32| {};
    match steps::run(
        &opts,
        &manifest,
        &payload,
        &mut |s| log_line(s),
        &mut progress,
    ) {
        Ok(_) => 0,
        Err(e) => {
            log_line(&format!("✗ 安装失败: {e}"));
            2
        }
    }
}
