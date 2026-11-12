//! IPC 消息结构（服务端和 GUI 共用一份，避免两边字段漂移）。

use serde::{Deserialize, Serialize};

/// 协议版本：GUI 与服务不一致时提示重装
pub const PROTO: u32 = 1;

/// 服务状态快照（GUI 状态页每秒拉一次）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    pub svc_version: String,
    pub proto: u32,
    /// 状态文案（未配置 / 待机 / 检测中 / 登录中 / 在线 / 退避等待 / 已暂停）
    pub state: String,
    /// 网络类型（非校园网 / 宿舍有线 / 公共无线）
    pub net_kind: String,
    /// 网络判别的依据（"宿舍门户=可达…" / "用户手动标记…"）
    pub net_evidence: String,
    /// 当前出口网卡的前缀长度（面板用来算网段）
    pub prefix_len: u8,
    /// 用户标记为校园网的网络列表
    pub trusted: Vec<crate::config::TrustedNet>,
    pub ssid: String,
    pub ip: String,
    pub mac: String,
    pub gateway: String,
    /// 出网是否正常（204）
    pub online: bool,
    /// 是否已配置凭据
    pub configured: bool,
    /// 账号（服务端已打码，如 250****23）
    pub account: String,
    /// 最近一次登录成功的 unix 时间（0 = 没有）
    pub last_ok_epoch: i64,
    pub last_ok_elapsed_ms: u64,
    pub last_err: String,
    /// 暂停到什么时候（unix 时间，0 = 没暂停）
    pub paused_until_epoch: i64,
    /// true = 手动"暂停直到恢复"（没有到期时间）
    pub paused_forever: bool,
    /// 距离下次重试还有多少秒（0 = 立刻）
    pub retry_in_sec: i64,
    pub ticks: u64,
    pub uptime_sec: u64,
    /// 连续失败计数：网络类 / 密码类
    pub fail_streak: u32,
    pub pw_fail_streak: u32,
}

impl Status {
    pub fn state_color(&self) -> (u8, u8, u8) {
        if self.online {
            (46, 160, 67) // 绿
        } else if self.state.contains("登录") || self.state.contains("检测") {
            (200, 140, 0) // 黄
        } else if self.state.contains("暂停") || !self.last_err.is_empty() {
            (200, 60, 60) // 红
        } else if self.state.contains("未配置") {
            (200, 140, 0)
        } else {
            (120, 120, 120) // 灰
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub cmd: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

impl Request {
    pub fn new(cmd: &str) -> Self {
        Self {
            cmd: cmd.to_string(),
            params: serde_json::Value::Null,
        }
    }

    pub fn with(cmd: &str, params: serde_json::Value) -> Self {
        Self {
            cmd: cmd.to_string(),
            params,
        }
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.params.get(key).and_then(|v| v.as_str())
    }

    pub fn u64(&self, key: &str) -> Option<u64> {
        self.params.get(key).and_then(|v| v.as_u64())
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        self.params.get(key).and_then(|v| v.as_bool())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Response {
    pub ok: bool,
    pub error: String,
    pub data: serde_json::Value,
}

impl Default for Response {
    fn default() -> Self {
        Self {
            ok: false,
            error: String::new(),
            data: serde_json::Value::Null,
        }
    }
}

impl Response {
    pub fn ok(data: serde_json::Value) -> Self {
        Self {
            ok: true,
            error: String::new(),
            data,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: msg.into(),
            data: serde_json::Value::Null,
        }
    }
}
