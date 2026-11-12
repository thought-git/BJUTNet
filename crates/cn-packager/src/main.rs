//! 打包器：把 dist 目录压成单文件安装包。
//!
//! ```text
//! cn-pack --template <BJUTNetSetup.exe> --dist <目录> --version 1.0.0 --out <输出.exe>
//! ```
//!
//! 原理：在安装器模板 exe 尾部追加
//! `payload = "RGMP" + u32(manifest长度) + manifest(json) + 各文件数据`
//! `footer  = u64(payload偏移) + u64(payload长度) + "RGPLPAY1"`
//!（自定义格式：模板 exe + 压缩载荷，实现见 `cn_common::payload`）

use std::path::{Path, PathBuf};

use cn_common::payload::{build_setup_exe, pack_dir, GameManifest};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |key: &str| -> Option<String> {
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if a == key {
                return it.next().cloned();
            }
            if let Some(rest) = a.strip_prefix(&format!("{key}=")) {
                return Some(rest.to_string());
            }
        }
        None
    };

    let template = value("--template").map(PathBuf::from);
    let dist = value("--dist").map(PathBuf::from);
    let out = value("--out").map(PathBuf::from);
    let version = value("--version").unwrap_or_else(|| "0.1.0".to_string());

    let (Some(template), Some(dist), Some(out)) = (template, dist, out) else {
        eprintln!("用法: cn-pack --template <BJUTNetSetup.exe> --dist <目录> --version <版本> --out <输出.exe>");
        std::process::exit(2);
    };

    let template_bytes = match std::fs::read(&template) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("✗ 读模板失败 {}: {e}", template.display());
            std::process::exit(2);
        }
    };
    if !dist.is_dir() {
        eprintln!("✗ dist 目录不存在: {}", dist.display());
        std::process::exit(2);
    }

    let mut manifest = GameManifest {
        game_name: "BJUTNet".into(),
        version: version.clone(),
        main_exe: "BJUTNet.exe".into(),
        install_dir_name: "BJUTNet".into(),
        uninstaller_name: "BJUTNetUninstall.exe".into(),
        desktop_shortcut: true,
        start_menu_shortcut: true,
        files: Vec::new(),
    };

    println!("打包 {} → {}", dist.display(), out.display());
    let payload = match pack_dir(&dist, &mut manifest, &[], |done, total| {
        if done == total || done % 5 == 0 {
            print!("\r  压缩 {done}/{total}");
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
        }
    }) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("\n✗ 打包失败: {e}");
            std::process::exit(2);
        }
    };
    println!();

    let exe = build_setup_exe(&template_bytes, &payload);
    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&out, &exe) {
        eprintln!("✗ 写输出失败: {e}");
        std::process::exit(2);
    }

    println!("✓ 完成: {}", out.display());
    println!("  模板 {} 字节 + 载荷 {} 字节 = {} 字节 ({:.2} MB)",
        template_bytes.len(),
        payload.len(),
        exe.len(),
        exe.len() as f64 / 1048576.0);
    for f in &manifest.files {
        println!("  {:>10}  {}{}", f.size, f.path, if f.compressed { "" } else { " (未压缩)" });
    }
    let _ = Path::new(&out);
}
