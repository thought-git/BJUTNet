//! 版本号：一处定义，GUI/服务/安装器共用。

/// 语义版本：发布构建用 `CAMPUSNET_VERSION=1.0.0` 注入，普通构建退回 crate 版本
pub const VERSION: &str = match option_env!("CAMPUSNET_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// 展示用版本号：**只显示语义版本**，不带构建时间戳（用户要求）
pub fn full() -> String {
    VERSION.to_string()
}
