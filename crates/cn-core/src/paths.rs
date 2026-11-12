//! 目录约定。所有路径只在这里出现一次，方便以后搬家。
//!
//! 正式部署：
//!   %ProgramData%\CampusNet\config.json / secrets.dat / logs\
//!
//! 开发/自检：可用环境变量 `CAMPUSNET_DATA` 指向任意可写目录，
//! 这样**不需要管理员权限**也能把 `--console` 跑起来（本机就是非提权）。

use std::path::{Path, PathBuf};

/// 环境变量：覆盖数据目录（开发/自检用）
pub const ENV_DATA_DIR: &str = "CAMPUSNET_DATA";

/// 数据目录
pub fn data_dir() -> PathBuf {
    if let Ok(v) = std::env::var(ENV_DATA_DIR) {
        if !v.trim().is_empty() {
            return PathBuf::from(v);
        }
    }
    match std::env::var("ProgramData") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v).join(crate::app::DATA_SUBDIR),
        _ => PathBuf::from(r"C:\ProgramData").join(crate::app::DATA_SUBDIR),
    }
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

pub fn secret_path() -> PathBuf {
    data_dir().join("secrets.dat")
}

pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

/// 当前 exe 的完整路径（已去掉 `.\` 之类的当前目录分量，日志里好看一点）
pub fn exe_path() -> PathBuf {
    let p = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("CampusNetSvc.exe"));
    normalize(&p)
}

fn normalize(p: &Path) -> PathBuf {
    p.components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect()
}

/// 当前 exe 所在目录（= 安装目录）
pub fn install_dir() -> PathBuf {
    exe_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 确保目录存在
pub fn ensure_dir(p: &Path) -> std::io::Result<()> {
    if p.as_os_str().is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(p)
}

/// 显示用：统一用反斜杠（避免 `F:/a/b` 和 `F:\a\b` 混排）
pub fn show(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\")
}

/// `%LOCALAPPDATA%\CampusNet`（安装器/卸载器写自己的日志用；不依赖 TMP 环境变量）
pub fn local_app_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(|p| PathBuf::from(p).join(crate::app::DATA_SUBDIR))
        .unwrap_or_else(data_dir)
}

/// 工具日志路径：`%LOCALAPPDATA%\CampusNet\<name>.log`（目录会自动创建）
pub fn tool_log(name: &str) -> PathBuf {
    let dir = local_app_dir();
    let _ = ensure_dir(&dir);
    dir.join(format!("{name}.log"))
}
