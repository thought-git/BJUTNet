//! 服务 ↔ GUI 的本地 IPC：**命名管道 + 长度前缀 JSON**。
//!
//! 为什么不用本机 TCP：
//! - 管道可以带**显式安全描述符**（SYSTEM/管理员全权，普通用户读写），端口号做不到"谁可以连"；
//! - 不占端口、不会被防火墙/HIPS 拦。
//!
//! 协议：4 字节小端长度 + UTF-8 JSON（请求 `Request` / 响应 `Response`）。
//! 单连接可连续收发多条，服务端每处理一条就 flush。

pub mod client;
pub mod protocol;
pub mod win;

pub use client::{call, call_cmd, call_timeout};
pub use protocol::{Request, Response, Status, PROTO};
pub use win::{pipe_name, pipe_path, Handle, DEFAULT_PIPE};
