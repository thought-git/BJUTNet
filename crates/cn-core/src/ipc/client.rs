//! IPC 客户端：GUI 侧用。一次调用 = 连管道 → 发一帧 → 收一帧 → 断开。
//!
//! 为什么不复用长连接：GUI 每秒查一次状态、偶尔发个命令，连接开销可忽略；
//! 短连接可以让"服务重启/崩溃"自然被感知（连接失败 → 面板显示未连接），不用做重连状态机。

use std::time::Duration;

use super::protocol::{Request, Response};
use super::win;

/// 句柄兜底关闭，保证任何返回路径都释放
struct Guard(win::Handle);

impl Drop for Guard {
    fn drop(&mut self) {
        win::close(self.0);
    }
}

/// 带超时调用
pub fn call_timeout(req: &Request, timeout: Duration) -> std::io::Result<Response> {
    let h = win::connect(timeout)?;
    let _guard = Guard(h);

    let payload = serde_json::to_vec(req)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    win::write_frame(h, &payload)?;

    let frame = win::read_frame(h)?
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "服务没有响应"))?;
    serde_json::from_slice::<Response>(&frame)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

/// 默认 3 秒超时
pub fn call(req: &Request) -> std::io::Result<Response> {
    call_timeout(req, Duration::from_secs(3))
}

/// 只发命令（无参数）
pub fn call_cmd(cmd: &str) -> std::io::Result<Response> {
    call(&Request::new(cmd))
}
