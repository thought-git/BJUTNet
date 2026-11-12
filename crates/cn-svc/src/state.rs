//! 认证状态机。
//!
//! ```text
//!   Unconfigured ──写入凭据──▶ Probing ──判定网络──▶ Authenticating ──成功──▶ Online
//!                                 │                      │
//!                                 ├─非校园网────────────▶ Idle（60 s 重探）
//!                                 │                      ├─网络类失败──▶ Backoff（2→4→8→15→30→60）
//!                                 │                      └─密码类失败≥N─▶ Paused（默认 10 min）
//!                                 └─凭据被清空/解不开──▶ Unconfigured
//! ```
//!
//! 设计要点：
//! - **成功清零**：一次成功就把两个失败计数和退避档位全部归零。
//! - **区分失败类型**：网络问题只退避；账号/密码问题累计到 `maxPasswordFails` 才暂停，
//!   避免密码填错时反复重试把账号试锁。
//! - `Paused` 期间仍然接受"立即重连"（GUI 手动触发），只是不自动重试。

use std::time::{Duration, Instant};

use cn_core::config::Config;
use cn_core::portal::{LoginFault, NetworkKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Unconfigured,
    Idle,
    Probing,
    Authenticating,
    Online,
    Backoff,
    Paused,
}

impl State {
    pub fn text(self) -> &'static str {
        match self {
            State::Unconfigured => "未配置",
            State::Idle => "待机（非校园网）",
            State::Probing => "检测中",
            State::Authenticating => "登录中",
            State::Online => "在线",
            State::Backoff => "退避等待",
            State::Paused => "已暂停",
        }
    }

    pub fn is_online(self) -> bool {
        matches!(self, State::Online)
    }
}

pub struct Machine {
    pub state: State,
    pub kind: NetworkKind,
    /// 连续"网络类"失败
    pub fail_streak: u32,
    /// 连续"账号/密码类"失败
    pub pw_fail_streak: u32,
    /// 退避档位（对应 cfg.backoff 的下标）
    pub backoff_idx: usize,
    /// 下一次允许尝试的时间
    pub next_try: Instant,
    pub paused_until: Option<Instant>,
    /// 手动"暂停直到我恢复"（没有到期时间）
    pub paused_forever: bool,
    pub last_ok_at: Option<Instant>,
    /// 最近一次成功的 unix 时间（给面板显示）
    pub last_ok_epoch: i64,
    pub last_ok_elapsed_ms: Option<u128>,
    pub last_err: String,
    /// 状态变化时给日志用
    pub changed: bool,
}

impl Machine {
    pub fn new() -> Self {
        Self {
            state: State::Unconfigured,
            kind: NetworkKind::None,
            fail_streak: 0,
            pw_fail_streak: 0,
            backoff_idx: 0,
            next_try: Instant::now(),
            paused_until: None,
            paused_forever: false,
            last_ok_at: None,
            last_ok_epoch: 0,
            last_ok_elapsed_ms: None,
            last_err: String::new(),
            changed: true,
        }
    }

    fn set(&mut self, s: State) {
        if self.state != s {
            self.state = s;
            self.changed = true;
        }
    }

    /// 开始一轮探测
    pub fn on_probing(&mut self) {
        self.set(State::Probing);
    }

    /// 正在提交登录
    pub fn on_authenticating(&mut self) {
        self.set(State::Authenticating);
    }

    /// 凭据不可用（没配、解不开）→ 回到未配置
    pub fn set_unconfigured(&mut self) {
        self.set(State::Unconfigured);
        self.kind = NetworkKind::None;
        self.paused_until = None;
        self.paused_forever = false;
    }

    /// 探测到出网正常（不管是不是我们登录的）
    pub fn on_online(&mut self) {
        if self.state != State::Online {
            self.set(State::Online);
        }
        self.fail_streak = 0;
        self.pw_fail_streak = 0;
        self.backoff_idx = 0;
        self.paused_until = None;
        self.paused_forever = false;
        self.last_ok_at = Some(Instant::now());
        self.last_ok_epoch = chrono::Local::now().timestamp();
        self.last_err.clear();
    }

    /// 掉线
    pub fn on_offline(&mut self) {
        if self.state == State::Online {
            self.set(State::Probing);
            self.next_try = Instant::now();
        }
    }

    /// 登录成功
    pub fn on_login_ok(&mut self, elapsed_ms: u128) {
        self.set(State::Online);
        self.fail_streak = 0;
        self.pw_fail_streak = 0;
        self.backoff_idx = 0;
        self.paused_until = None;
        self.paused_forever = false;
        self.last_ok_at = Some(Instant::now());
        self.last_ok_epoch = chrono::Local::now().timestamp();
        self.last_ok_elapsed_ms = Some(elapsed_ms);
        self.last_err.clear();
    }

    /// 登录失败：按类型决定退避还是暂停
    pub fn on_login_fail(&mut self, fault: LoginFault, msg: &str, cfg: &Config) {
        self.last_err = msg.to_string();
        if fault.is_account() {
            self.pw_fail_streak += 1;
            if self.pw_fail_streak >= cfg.max_password_fails.max(1) {
                let mins = cfg.pause_minutes.max(1);
                self.paused_forever = false;
                self.paused_until = Some(Instant::now() + Duration::from_secs(mins * 60));
                self.set(State::Paused);
                return;
            }
        } else {
            self.fail_streak += 1;
        }

        // 退避：2 → 4 → 8 → 15 → 30 → 60（取配置里的档位，走完停在最后一档）
        let backoff = if cfg.backoff.is_empty() {
            vec![2, 4, 8, 15, 30, 60]
        } else {
            cfg.backoff.clone()
        };
        let idx = self.backoff_idx.min(backoff.len().saturating_sub(1));
        let delay = backoff.get(idx).copied().unwrap_or(60).max(1);
        self.backoff_idx = (self.backoff_idx + 1).min(backoff.len().saturating_sub(1));
        self.next_try = Instant::now() + Duration::from_secs(delay);
        self.set(State::Backoff);
    }

    /// 非校园网：进入待机，下一轮 60 s 后再探
    pub fn on_not_campus(&mut self) {
        self.set(State::Idle);
        self.kind = NetworkKind::None;
        self.next_try = Instant::now() + Duration::from_secs(60);
    }

    pub fn on_kind(&mut self, kind: NetworkKind) {
        self.kind = kind;
    }

    pub fn is_paused(&self, now: Instant) -> bool {
        if self.state != State::Paused {
            return false;
        }
        if self.paused_forever {
            return true;
        }
        self.paused_until.map(|t| now < t).unwrap_or(false)
    }

    /// 暂停是否该结束了（"暂停直到手动恢复"永远不自动结束）
    pub fn pause_expired(&self, now: Instant) -> bool {
        if self.state != State::Paused || self.paused_forever {
            return false;
        }
        self.paused_until.map(|t| now >= t).unwrap_or(true)
    }

    /// 现在是否允许发起一次登录尝试
    pub fn due(&self, now: Instant) -> bool {
        now >= self.next_try
    }

    /// 面板点了「暂停 N 分钟」
    pub fn pause(&mut self, minutes: u64) {
        let mins = minutes.max(1);
        self.paused_forever = false;
        self.paused_until = Some(Instant::now() + Duration::from_secs(mins * 60));
        self.set(State::Paused);
    }

    /// 面板点了「暂停直到手动恢复」：没有到期时间，只能点恢复/重启服务解除
    pub fn pause_until_resume(&mut self) {
        self.paused_forever = true;
        self.paused_until = None;
        self.set(State::Paused);
    }

    /// 强制立刻尝试（GUI「立即重连」/ 网络变化）
    pub fn force_now(&mut self) {
        self.next_try = Instant::now();
        self.paused_until = None;
        self.paused_forever = false;
        if self.state == State::Paused {
            self.pw_fail_streak = 0;
            self.set(State::Probing);
        }
    }

    /// 下一轮 sleep 时长
    pub fn interval(&self, cfg: &Config, now: Instant) -> Duration {
        match self.state {
            State::Online => cfg.online_interval(),
            State::Paused => {
                let left = self
                    .paused_until
                    .map(|t| t.saturating_duration_since(now))
                    .unwrap_or(Duration::from_secs(60));
                left.min(Duration::from_secs(60)).max(Duration::from_secs(5))
            }
            State::Backoff => {
                let left = self.next_try.saturating_duration_since(now);
                left.min(Duration::from_secs(60)).max(Duration::from_secs(1))
            }
            State::Idle => Duration::from_secs(60),
            _ => cfg.offline_interval(),
        }
    }

    /// 一行状态摘要（日志/心跳/以后 GUI 都用它）
    pub fn summary(&self) -> String {
        let mut s = format!("state={} net={}", self.state.text(), self.kind.text());
        if let Some(t) = self.paused_until {
            if self.state == State::Paused {
                let left = t.saturating_duration_since(Instant::now()).as_secs();
                s.push_str(&format!(" 暂停剩余={left}s"));
            }
        }
        if self.fail_streak > 0 || self.pw_fail_streak > 0 {
            s.push_str(&format!(
                " 失败(网络={}/密码={})",
                self.fail_streak, self.pw_fail_streak
            ));
        }
        if !self.last_err.is_empty() {
            s.push_str(&format!(" 最近错误={}", self.last_err));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_resets_counters() {
        let cfg = Config::default();
        let mut m = Machine::new();
        m.on_login_fail(LoginFault::Network, "timeout", &cfg);
        m.on_login_fail(LoginFault::Network, "timeout", &cfg);
        assert_eq!(m.fail_streak, 2);
        m.on_login_ok(100);
        assert_eq!(m.fail_streak, 0);
        assert_eq!(m.backoff_idx, 0);
        assert_eq!(m.state, State::Online);
    }

    #[test]
    fn password_failures_pause() {
        let mut cfg = Config::default();
        cfg.max_password_fails = 3;
        cfg.pause_minutes = 10;
        let mut m = Machine::new();
        for _ in 0..2 {
            m.on_login_fail(LoginFault::AccountPassword, "认证失败", &cfg);
            assert_eq!(m.state, State::Backoff);
        }
        m.on_login_fail(LoginFault::AccountPassword, "认证失败", &cfg);
        assert_eq!(m.state, State::Paused);
        assert!(m.is_paused(Instant::now()));
        m.force_now();
        assert_eq!(m.state, State::Probing);
    }

    #[test]
    fn backoff_goes_through_steps() {
        let mut cfg = Config::default();
        cfg.backoff = vec![2, 4];
        let mut m = Machine::new();
        m.on_login_fail(LoginFault::Network, "x", &cfg);
        assert_eq!(m.backoff_idx, 1);
        m.on_login_fail(LoginFault::Network, "x", &cfg);
        assert_eq!(m.backoff_idx, 1); // 停在最后一档
        assert_eq!(m.state, State::Backoff);
    }
}
