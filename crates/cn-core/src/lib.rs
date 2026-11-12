//! CampusNet 共享库（GUI / 服务 / 安装器共用）。
//!
//! 模块划分：
//! - `util`    宽字符 / 错误码 / 时间
//! - `version` 版本号
//! - `paths`   数据目录（%ProgramData%\CampusNet）与安装目录
//! - `logging` 文件日志（按天切）+ 控制台回显
//! - `config`  config.json 模型与原子读写
//! - `secret`  DPAPI 加密的 secrets.dat
//! - `net`     网卡枚举 / 默认出口 / 虚拟网卡识别 / 是否提权
//! - `wlan`    WLAN SSID
//! - `http`    HTTP 探活（generate_204）

pub mod config;
pub mod app;
pub mod http;
pub mod ipc;
pub mod logging;
pub mod net;
pub mod paths;
pub mod portal;
pub mod secret;
pub mod shell;
pub mod svcctl;
pub mod winreg;
pub mod util;
pub mod version;
pub mod wlan;

pub use version::VERSION;
