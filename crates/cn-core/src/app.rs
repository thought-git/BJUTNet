//! 应用身份常量与默认路径（服务、面板、安装器、卸载器共用一份）。

use std::path::PathBuf;

pub const APP_ID: &str = "BJUTNet";
pub const APP_NAME: &str = "BJUTNet";
pub const DISPLAY_NAME: &str = "BJUTNet 校园网自动登录";
pub const PUBLISHER: &str = "BJUTNet";
pub const GUI_EXE: &str = "BJUTNet.exe";
pub const SVC_EXE: &str = "BJUTNetSvc.exe";
pub const UNINST_EXE: &str = "BJUTNetUninstall.exe";
pub const VERSION_FILE: &str = "version.txt";
pub const INSTALL_META: &str = "install.json";

pub const SERVICE_NAME: &str = "BJUTNetSvc";
pub const SERVICE_DISPLAY: &str = "BJUTNet 校园网自动登录服务";
pub const SERVICE_DESC: &str = "自动检测校园网环境（宿舍有线 / 公共无线）并完成 Dr.COM 门户认证";

pub const UNINSTALL_REG_PATH: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\BJUTNet";

/// `%ProgramData%` 下的子目录名
pub const DATA_SUBDIR: &str = "BJUTNet";

// ---------------------------------------------------------------------------
// 上一代（CampusNet）遗留物：安装新版时自动清掉，并把数据迁过来
// ---------------------------------------------------------------------------

/// 旧版本的安装/服务名（升级时必须停删，否则两个服务抢同一账号）
pub const LEGACY_APP_ID: &str = "CampusNet";
/// 上一代（CampusNet）的服务名
pub const LEGACY_SERVICE: &str = "CampusNetSvc";
/// 更早那版（PowerShell 方案）的服务名 —— 两个都要停删，否则会抢同一账号会话
pub const LEGACY_SERVICE_NETSVC: &str = "NetSvc";
pub const LEGACY_GUI_EXE: &str = "CampusNet.exe";
pub const LEGACY_SVC_EXE: &str = "CampusNetSvc.exe";
pub const LEGACY_DATA_SUBDIR: &str = "CampusNet";

/// `%ProgramData%`
pub fn program_data() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
}

/// `%ProgramFiles%`
pub fn program_files() -> PathBuf {
    std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
}

/// 默认安装目录：`C:\Program Files\CampusNet`
pub fn default_install_dir() -> PathBuf {
    program_files().join(APP_ID)
}

/// 安装清单（记录快捷方式选项等，卸载器用）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct InstallMeta {
    pub version: String,
    pub install_dir: String,
    pub desktop_shortcut: bool,
    pub start_menu_shortcut: bool,
    pub service_name: String,
    pub installed_at: String,
}

impl Default for InstallMeta {
    fn default() -> Self {
        Self {
            version: String::new(),
            install_dir: String::new(),
            desktop_shortcut: false,
            start_menu_shortcut: false,
            service_name: SERVICE_NAME.to_string(),
            installed_at: String::new(),
        }
    }
}
