//! 中文字体 + 窗口图标（与面板同一套做法）。

use eframe::egui;

pub fn setup(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for path in [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyhbd.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
        r"C:\Windows\Fonts\Deng.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(
                "cjk".to_owned(),
                std::sync::Arc::new(egui::FontData::from_owned(bytes)),
            );
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push("cjk".to_owned());
            }
            break;
        }
    }
    ctx.set_fonts(fonts);
}

/// 用仓库里的 `assets/icon.ico` 做窗口图标（与 exe 图标同一张图）
pub fn window_icon() -> egui::IconData {
    static ICO: &[u8] = include_bytes!("../../../assets/icon.ico");
    if let Some((rgba, w, h)) = decode_ico(ICO) {
        return scale_to_icon(&rgba, w, h, 32);
    }
    fallback_icon()
}

fn decode_ico(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let dir = ico::IconDir::read(std::io::Cursor::new(bytes)).ok()?;
    // 取最大的一张，再缩到 32×32（标题栏/任务栏用）
    let entry = dir
        .entries()
        .iter()
        .max_by_key(|e| e.width() * e.height())?;
    let img = entry.decode().ok()?;
    Some((img.rgba_data().to_vec(), img.width(), img.height()))
}

/// 最近邻缩放（图标缩放不需要插值，保持边缘清晰）
fn scale_to_icon(rgba: &[u8], w: u32, h: u32, target: u32) -> egui::IconData {
    let mut out = vec![0u8; (target * target * 4) as usize];
    if w == 0 || h == 0 {
        return fallback_icon();
    }
    for ty in 0..target {
        for tx in 0..target {
            let sx = (tx as u64 * w as u64 / target as u64) as u32;
            let sy = (ty as u64 * h as u64 / target as u64) as u32;
            let si = ((sy * w + sx) * 4) as usize;
            let di = ((ty * target + tx) * 4) as usize;
            if si + 4 <= rgba.len() {
                out[di..di + 4].copy_from_slice(&rgba[si..si + 4]);
            }
        }
    }
    egui::IconData {
        rgba: out,
        width: target,
        height: target,
    }
}

/// 读不到图标文件时的兜底（纯代码生成）
fn fallback_icon() -> egui::IconData {
    let size = 32u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let c = (size as f32 - 1.0) / 2.0;
    let r = size as f32 * 0.44;
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let i = ((y * size + x) * 4) as usize;
            if d <= r {
                rgba[i] = 30;
                rgba[i + 1] = 100;
                rgba[i + 2] = 200;
                rgba[i + 3] = 255;
            }
            if d <= r * 0.5 {
                rgba[i] = 245;
                rgba[i + 1] = 248;
                rgba[i + 2] = 255;
            }
        }
    }
    egui::IconData { rgba, width: size, height: size }
}
