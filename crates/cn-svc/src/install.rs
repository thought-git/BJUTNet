//! `--install` / `--uninstall`：命令行方式装/删服务。
//!
//! SCM 的 FFI 已经搬到 `cn_core::svcctl`（安装器/卸载器也要用），这里只留 CLI 外壳。
//! 正式安装请用 `BJUTNetSetup.exe`；这两个命令是给排障和"手工装一次"用的。

use cn_core::app;
use cn_core::{net, paths, svcctl};

pub fn install() -> i32 {
    if !net::is_elevated() {
        eprintln!("✗ 需要管理员权限：请右键「以管理员身份运行」后再执行 --install");
        return 1;
    }
    let exe = paths::exe_path();
    if !exe.is_file() {
        eprintln!("✗ 找不到自身 exe: {}", exe.display());
        return 1;
    }
    let bin_path = format!("\"{}\" --service", exe.display());
    println!("安装服务");
    println!("  服务名 : {}", app::SERVICE_NAME);
    println!("  显示名 : {}", app::SERVICE_DISPLAY);
    println!("  路径   : {bin_path}");

    match svcctl::install(
        app::SERVICE_NAME,
        app::SERVICE_DISPLAY,
        app::SERVICE_DESC,
        &bin_path,
        false, // 普通自动启动：开机即登录（延迟启动会白等 1~2 分钟）
    ) {
        Ok(()) => println!("  服务已创建（自动启动 + 延迟启动 + 失败自动重启）"),
        Err(e) => {
            eprintln!("✗ {e}");
            return 2;
        }
    }

    match svcctl::start(app::SERVICE_NAME) {
        Ok(()) => {
            println!("✓ 服务已安装并启动（{} = RUNNING）", app::SERVICE_NAME);
            0
        }
        Err(e) => {
            eprintln!("✗ {e}");
            eprintln!("  排查：事件查看器 → Windows 日志 → 系统（筛 Service Control Manager）");
            eprintln!("  或先手动跑：BJUTNetSvc.exe --console 看它卡在哪");
            2
        }
    }
}

pub fn uninstall() -> i32 {
    if !net::is_elevated() {
        eprintln!("✗ 需要管理员权限：请右键「以管理员身份运行」后再执行 --uninstall");
        return 1;
    }
    if svcctl::service_state(app::SERVICE_NAME).is_none() {
        println!("服务 {} 不存在，无需卸载", app::SERVICE_NAME);
        return 0;
    }
    match svcctl::remove(app::SERVICE_NAME) {
        Ok(()) => {
            println!("✓ 服务已停止并删除（{}）", app::SERVICE_NAME);
            0
        }
        Err(e) => {
            eprintln!("✗ {e}");
            2
        }
    }
}
