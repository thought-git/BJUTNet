//! 校园网门户协议层：**网络判别** + **两套登录**。
//!
//! 两套协议的区别（都是实测参数，见方案 §9 / 附录 B）：
//!
//! | | 宿舍有线 | 公共无线 `bjut_wifi` |
//! |---|---|---|
//! | 端点 | `http://10.21.221.98:801/eportal/portal/login` | `https://wlgn.bjut.edu.cn/drcom/login` |
//! | 账号 | `学号@campus` | 纯学号 |
//! | 参数 | `user_account` / `user_password` | `DDDDD` / `upass` / `0MKKey` |
//! | 附加 | `wlan_user_ip/mac`、`jsVersion=4.X` | `R1=0 R3=0 R6=1 para=00`、`jsVersion=4.1` |
//!
//! 判别优先级：**门户可达性**（宿舍可达 / 公共可达，物理互斥）> SSID > 网段兜底。

pub mod dorm;
pub mod public;

use std::time::Duration;

use crate::config::Config;
use crate::http;

/// 网络类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkKind {
    None,
    Dorm,
    Public,
}

impl NetworkKind {
    pub fn text(self) -> &'static str {
        match self {
            NetworkKind::None => "非校园网",
            NetworkKind::Dorm => "宿舍有线",
            NetworkKind::Public => "公共无线",
        }
    }

    pub fn is_campus(self) -> bool {
        !matches!(self, NetworkKind::None)
    }

    /// 配置/日志里用的短名
    pub fn key(self) -> &'static str {
        match self {
            NetworkKind::None => "none",
            NetworkKind::Dorm => "dorm",
            NetworkKind::Public => "public",
        }
    }
}

/// 登录失败的分类（决定"要不要暂停等用户改密码"）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginFault {
    /// 网络层问题：超时/连不上/门户不可达 → 只退避，不暂停
    Network,
    /// 账号或密码问题 → 累计后暂停，避免把账号试锁
    AccountPassword,
    /// 门户端点不存在/协议不匹配
    Endpoint,
    Unknown,
}

impl LoginFault {
    pub fn is_account(&self) -> bool {
        matches!(self, LoginFault::AccountPassword)
    }
}

/// 一次登录尝试的结果
#[derive(Debug, Clone)]
pub struct LoginOutcome {
    pub ok: bool,
    /// 门户表示"本来就在线"
    pub already: bool,
    pub fault: LoginFault,
    pub message: String,
    pub raw_head: String,
    pub url: String,
    pub elapsed_ms: u128,
    pub attempts: Vec<String>,
}

impl LoginOutcome {
    pub fn success(url: &str, msg: &str, elapsed_ms: u128) -> Self {
        Self {
            ok: true,
            already: false,
            fault: LoginFault::Unknown,
            message: msg.to_string(),
            raw_head: String::new(),
            url: url.to_string(),
            elapsed_ms,
            attempts: Vec::new(),
        }
    }

    pub fn already(url: &str, msg: &str) -> Self {
        let mut o = Self::success(url, msg, 0);
        o.already = true;
        o
    }

    pub fn failure(url: &str, fault: LoginFault, msg: &str, raw: &str) -> Self {
        Self {
            ok: false,
            already: false,
            fault,
            message: msg.to_string(),
            raw_head: raw.chars().take(300).collect(),
            url: url.to_string(),
            elapsed_ms: 0,
            attempts: Vec::new(),
        }
    }

    pub fn summary(&self) -> String {
        if self.ok {
            if self.already {
                format!("已在线（门户提示：{}）", self.message)
            } else {
                format!("登录成功（{}，{} ms）", self.message, self.elapsed_ms)
            }
        } else {
            format!("登录失败[{:?}]：{}", self.fault, self.message)
        }
    }
}

impl std::fmt::Display for LoginFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            LoginFault::Network => "网络",
            LoginFault::AccountPassword => "账号/密码",
            LoginFault::Endpoint => "端点",
            LoginFault::Unknown => "未知",
        };
        f.write_str(s)
    }
}

/// 网络判别的证据（日志/诊断页都要看这个）
#[derive(Debug, Clone, Default)]
pub struct DetectEvidence {
    pub dorm_reachable: bool,
    pub dorm_status: Option<u16>,
    pub dorm_ms: u128,
    pub public_reachable: bool,
    pub public_status: Option<u16>,
    pub public_ms: u128,
    pub ssid: Option<String>,
    /// true = 两个门户都探不通，是按网段"猜"的
    pub guessed: bool,
    /// 命中了用户"手动标记为校园网"的条目（此时不探门户，直接返回）
    pub trusted: bool,
    pub trusted_desc: String,
    pub notes: Vec<String>,
}

impl DetectEvidence {
    pub fn text(&self) -> String {
        format!(
            "宿舍门户={} 公共门户={}{}",
            if self.dorm_reachable {
                format!("可达({:?},{}ms)", self.dorm_status, self.dorm_ms)
            } else {
                "不可达".to_string()
            },
            if self.public_reachable {
                format!("可达({:?},{}ms)", self.public_status, self.public_ms)
            } else {
                "不可达".to_string()
            },
            if self.trusted {
                format!(" [用户标记为校园网{}]", if self.trusted_desc.is_empty() { String::new() } else { format!(": {}", self.trusted_desc) })
            } else if self.guessed {
                " [按网段推断]".to_string()
            } else {
                String::new()
            }
        )
    }
}

/// 门户探针地址
pub fn dorm_probe_url(cfg: &Config) -> String {
    format!("{}/a79.htm", cfg.dorm.base.trim_end_matches('/'))
}

pub fn public_probe_url(cfg: &Config) -> String {
    format!(
        "{}{}",
        cfg.public.base.trim_end_matches('/'),
        cfg.public.referer_path
    )
}

/// 网络判别。
///
/// `ssid`：当前 WLAN 的 SSID（没连无线给 None）
/// `ipv4`：活跃网卡的 IPv4（网段兜底用）
pub fn detect(
    cfg: &Config,
    ssid: Option<&str>,
    ipv4: Option<&str>,
    gateway: &str,
    timeout: Duration,
) -> (NetworkKind, DetectEvidence) {
    // 0) 用户手动标记优先：命中就按标记走，不再探门户（快，且不受门户临时故障影响）
    if let Some(t) = cfg.trusted_match(ssid, ipv4, gateway) {
        let kind = if t.is_public() {
            NetworkKind::Public
        } else {
            NetworkKind::Dorm
        };
        let ev = DetectEvidence {
            dorm_reachable: false,
            dorm_status: None,
            dorm_ms: 0,
            public_reachable: false,
            public_status: None,
            public_ms: 0,
            ssid: ssid.map(|s| s.to_string()),
            guessed: false,
            trusted: true,
            trusted_desc: t.describe(),
            notes: vec![format!("用户手动标记：{}", t.describe())],
        };
        return (kind, ev);
    }

    // 单个门户探测最多 2.5 s，避免离线时判别一次卡 6 s
    let probe_timeout = timeout.min(Duration::from_millis(2500));

    let dorm_url = dorm_probe_url(cfg);
    let public_url = public_probe_url(cfg);

    let dorm = http::get(&dorm_url, probe_timeout);
    let public = http::get(&public_url, probe_timeout);

    let mut ev = DetectEvidence {
        dorm_reachable: dorm.status.is_some(),
        dorm_status: dorm.status,
        dorm_ms: dorm.elapsed_ms,
        public_reachable: public.status.is_some(),
        public_status: public.status,
        public_ms: public.elapsed_ms,
        ssid: ssid.map(|s| s.to_string()),
        guessed: false,
        trusted: false,
        trusted_desc: String::new(),
        notes: Vec::new(),
    };
    if !dorm.error.is_empty() {
        ev.notes.push(format!("宿舍门户探测失败: {}", dorm.error));
    }
    if !public.error.is_empty() {
        ev.notes.push(format!("公共门户探测失败: {}", public.error));
    }

    let kind = match (ev.dorm_reachable, ev.public_reachable) {
        (true, false) => NetworkKind::Dorm,
        (false, true) => NetworkKind::Public,
        (true, true) => {
            // 极少见：两个门户都通。无线且 SSID 是校园 WiFi 时按公共网处理
            let is_wifi = ssid
                .map(|s| s.eq_ignore_ascii_case(&cfg.wifi_profile))
                .unwrap_or(false);
            ev.notes
                .push(format!("两个门户都可达，按 SSID={:?} 判定", ssid));
            if is_wifi {
                NetworkKind::Public
            } else {
                NetworkKind::Dorm
            }
        }
        (false, false) => {
            // 都探不通：按网段兜底（门户临时故障时也有机会登录）
            let guessed = match ipv4 {
                Some(ip) if ip.starts_with("10.126.") => Some(NetworkKind::Dorm),
                Some(ip) if ip.starts_with("10.20.") => Some(NetworkKind::Public),
                _ => None,
            };
            if let Some(k) = guessed {
                ev.guessed = true;
                ev.notes
                    .push(format!("两个门户都不可达，按网段 {} 推断为 {}", ipv4.unwrap_or("-"), k.text()));
                k
            } else {
                NetworkKind::None
            }
        }
    };

    (kind, ev)
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

/// 门户的 b64 参数（会把 IPv4 编码后塞进 query）
pub fn b64(input: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(input.as_bytes())
}

/// 账号专用编码：**保留 `@` 原样**。
///
/// 实测（2026-11-12，真实宿舍 AC）：`user_account=20250000%40campus` 会被
/// 拒为"无法获取用户认证账号！"，而原样 `20250000@campus` 直接认证成功。
/// 所以账号参数不能走通用百分号编码。
pub fn percent_encode_account(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 3);
    for b in input.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '@') {
            out.push(c);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// RFC3986 百分号编码（url 里只有 unreserved 字符不需要转义）
pub fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 3);
    for b in input.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// 从 JSONP 响应里抠出 JSON 主体：`jsonpReturn({...});` / `dr1003({...})`
pub fn unwrap_jsonp(body: &str) -> &str {
    match (body.find('{'), body.rfind('}')) {
        (Some(s), Some(e)) if e >= s => &body[s..=e],
        _ => body.trim(),
    }
}

/// 取整数字段（容忍数字/字符串两种形态）
pub fn json_int(v: &serde_json::Value, key: &str) -> Option<i64> {
    let node = v.get(key)?;
    if let Some(n) = node.as_i64() {
        return Some(n);
    }
    node.as_str().and_then(|s| s.trim().parse::<i64>().ok())
}

/// 取字符串字段
pub fn json_str<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|n| n.as_str())
}

/// 账号拆成 (学号, 后缀)；没后缀时后缀为空
pub fn split_account(account: &str) -> (&str, &str) {
    match account.split_once('@') {
        Some((id, suffix)) => (id, suffix),
        None => (account, ""),
    }
}

/// 给账号补后缀（已经带 `@` 就不动）
pub fn with_suffix(account: &str, suffix: &str) -> String {
    if account.contains('@') || suffix.trim().is_empty() {
        account.to_string()
    } else {
        format!("{account}{suffix}")
    }
}

/// 随机 `v` 参数（门户 JS 会带一个随机数，防缓存）
pub fn rnd() -> u32 {
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
        % 89_000)
        + 10_000
}

/// 从门户的 `msg` 判失败类型（决定"只退避"还是"暂停等用户改密码"）
pub fn classify_msg(msg: &str) -> LoginFault {
    let m = msg.to_ascii_lowercase();
    if msg.contains("已经在线") {
        return LoginFault::Unknown;
    }
    if msg.contains("密码")
        || msg.contains("账号")
        || msg.contains("用户")
        || msg.contains("认证失败")
        || m.contains("authentication fail")
        || m.contains("auth error")
        || m.contains("auth fail")
        || m.contains("ldap")
        || m.contains("password")
        || m.contains("account")
    {
        return LoginFault::AccountPassword;
    }
    if m.contains("timeout") || m.contains("connect") || m.contains("refused") {
        return LoginFault::Network;
    }
    LoginFault::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonp_unwrap() {
        assert_eq!(
            unwrap_jsonp(r#"jsonpReturn({"a":1});"#),
            r#"{"a":1}"#
        );
        assert_eq!(unwrap_jsonp(r#"dr1003({"result":1})"#), r#"{"result":1}"#);
        assert_eq!(unwrap_jsonp("404 Not Found"), "404 Not Found");
    }

    #[test]
    fn account_split_and_suffix() {
        assert_eq!(split_account("20250000"), ("20250000", ""));
        assert_eq!(split_account("20250000@campus"), ("20250000", "campus"));
        assert_eq!(with_suffix("20250000", "@campus"), "20250000@campus");
        assert_eq!(with_suffix("20250000@campus", "@campus"), "20250000@campus");
        assert_eq!(with_suffix("20250000", ""), "20250000");
    }

    #[test]
    fn classify_ldap_auth_error_is_account_fault() {
        // 实测：密码错时门户返回 {"result":0,"msg":"ldap auth error"}
        assert_eq!(classify_msg("ldap auth error"), LoginFault::AccountPassword);
        assert_eq!(classify_msg("密码错误"), LoginFault::AccountPassword);
        assert_eq!(classify_msg("无法获取用户认证账号！"), LoginFault::AccountPassword);
        assert_eq!(classify_msg("timeout"), LoginFault::Network);
    }

    #[test]
    fn account_keeps_at_sign() {
        assert_eq!(percent_encode_account("20250000@campus"), "20250000@campus");
        assert_eq!(percent_encode_account("a b@c"), "a%20b@c");
    }

    #[test]
    fn percent_encode_escapes_at() {
        assert_eq!(percent_encode("20250000@campus"), "20250000%40campus");
    }

    #[test]
    fn b64_matches_known_value() {
        // 实测：10.126.83.10 → MTAuMTI2LjgzLjEw
        assert_eq!(b64("10.126.83.10"), "MTAuMTI2LjgzLjEw");
    }
}
