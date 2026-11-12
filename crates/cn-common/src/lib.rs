//! 游戏打包公共库：自定义 payload 归档格式 + Windows 工具（快捷方式/注册表/对话框）。
//!
//! 单 exe 安装包格式：
//! ```text
//! [安装器模板 exe 字节]
//! [payload: "RGMP"(4) + u32 manifest_len + manifest(json) + 各文件压缩数据]
//! [footer: u64 payload_offset + u64 payload_len + "RGPLPAY1"(8)]
//! ```
//! 安装器运行时读取自身 exe 尾部 footer 定位 payload 并解压。

pub mod payload;
#[cfg(windows)]
pub mod winutil;

pub use payload::*;
