//! BJUTNet 面板（GUI）。
//!
//! 定位：**可选的控制面板**。配置一次之后就可以关掉，服务自己开机自启、自己登录。
//! 没有托盘：关窗口就是退出面板，服务完全不受影响（两者是独立进程）。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod bridge;
mod fonts;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let viewport = egui::ViewportBuilder::default()
        .with_title("BJUTNet 校园网自动登录")
        .with_inner_size([780.0, 600.0])
        .with_min_inner_size([700.0, 520.0])
        .with_icon(fonts::window_icon());

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "BJUTNet",
        options,
        Box::new(|cc| {
            fonts::setup(&cc.egui_ctx);
            Ok(Box::new(app::App::new(cc)))
        }),
    )
}
