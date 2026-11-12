//! 面板与服务的桥：后台轮询状态 + 异步发指令。
//!
//! 所有 IPC 调用都在后台线程做，UI 线程只收 channel 消息——否则一次 3 秒超时就把界面卡死。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;
use serde_json::Value;

use cn_core::ipc::protocol::{Request, Response, Status};

pub enum UiEvent {
    Status(Result<Status, String>),
    /// (结果, 动作标签) —— 标签用于提示语（保存/重连/日志/诊断…）
    Action(Result<Response, String>, &'static str),
}

pub struct Bridge {
    pub rx: Receiver<UiEvent>,
    tx: Sender<UiEvent>,
    stop: Arc<AtomicBool>,
}

impl Bridge {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));

        let tx_status = tx.clone();
        let stop_status = stop.clone();
        let ctx_status = ctx.clone();
        std::thread::spawn(move || {
            while !stop_status.load(Ordering::Relaxed) {
                let r = match cn_core::ipc::call_cmd("status") {
                    Ok(resp) if resp.ok => serde_json::from_value::<Status>(resp.data)
                        .map_err(|e| format!("状态解析失败: {e}")),
                    Ok(resp) => Err(if resp.error.is_empty() {
                        "服务返回失败".to_string()
                    } else {
                        resp.error
                    }),
                    Err(e) => Err(format!("未连接服务: {e}")),
                };
                if let Err(e) = &r {
                    // 面板没控制台，错误落到临时文件里方便排查
                    use std::io::Write as _;
                    let path = std::env::temp_dir().join("BJUTNetGui.log");
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                    {
                        let _ = writeln!(f, "{} {}", chrono::Local::now().format("%H:%M:%S"), e);
                    }
                }
                let _ = tx_status.send(UiEvent::Status(r));
                ctx_status.request_repaint();

                // 900 ms，分片睡以便及时退出
                for _ in 0..9 {
                    if stop_status.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });

        Self { rx, tx, stop }
    }

    /// 发一条命令（后台执行，结果通过 `UiEvent::Action` 回来）
    pub fn send(&self, cmd: &'static str, params: Value, label: &'static str) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let req = Request::with(cmd, params);
            // 诊断要探测门户，给足时间；其它命令 3 秒足够
            let timeout = if cmd == "get_diag" {
                Duration::from_secs(30)
            } else {
                Duration::from_secs(3)
            };
            let r = cn_core::ipc::call_timeout(&req, timeout).map_err(|e| e.to_string());
            let _ = tx.send(UiEvent::Action(r, label));
        });
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
