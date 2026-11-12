//! HTTP 探活。
//!
//! 铁律：**判在线只能用 HTTP，不能用 ping**（校园网屏蔽 ICMP，旧方案在这里误判过）。
//!
//! 判定规则（沿用旧方案验证过的逻辑）：
//! - 204 → 在线
//! - 200 且 body 很短（≤4B） → 在线（部分 captive 替代实现如此）
//! - 200 且 body 里出现 `a79.htm` / `eportal` / `Dr.COM` / `上网登录` → 被劫持 → 离线
//! - 超时/连不上 → 离线

use std::io::Read;
use std::time::{Duration, Instant};

pub const DEFAULT_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) CampusNet/0.1";

/// 默认探测目标（与旧方案一致：两个不同厂商的 204）
pub const PROBE_URLS: &[&str] = &[
    "http://cp.cloudflare.com/generate_204",
    "http://connect.rom.miui.com/generate_204",
];

#[derive(Debug, Clone)]
pub struct ProbeReport {
    pub url: String,
    pub status: Option<u16>,
    pub body_head: String,
    pub error: String,
    pub elapsed_ms: u128,
}

impl ProbeReport {
    pub fn ok_204(&self) -> bool {
        self.status == Some(204)
    }

    pub fn text(&self) -> String {
        match (self.status, self.error.is_empty()) {
            (Some(s), true) => format!("{} → {s} ({} ms)", self.url, self.elapsed_ms),
            _ => format!("{} → 失败: {} ({} ms)", self.url, self.error, self.elapsed_ms),
        }
    }
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(timeout)
        .timeout_read(timeout)
        .user_agent(DEFAULT_UA)
        .build()
}

/// 完整响应（body 不超过 64 KiB），门户的 loadConfig / login 响应要完整 JSON 才能解析
#[derive(Debug, Clone)]
pub struct FetchResult {
    pub status: Option<u16>,
    pub body: String,
    pub error: String,
    pub elapsed_ms: u128,
}

impl FetchResult {
    pub fn reachable(&self) -> bool {
        self.status.is_some()
    }
}

/// 发一个 GET 并保留完整 body（截断到 64 KiB）
pub fn fetch(url: &str, timeout: Duration, headers: &[(&str, &str)]) -> FetchResult {
    let started = Instant::now();
    let mut out = FetchResult {
        status: None,
        body: String::new(),
        error: String::new(),
        elapsed_ms: 0,
    };

    let agent = agent(timeout);
    let mut req = agent.get(url);
    for (k, v) in headers {
        req = req.set(k, v);
    }

    let read_body = |resp: ureq::Response, out: &mut FetchResult| {
        out.status = Some(resp.status());
        let mut buf = Vec::new();
        let _ = resp
            .into_reader()
            .take(64 * 1024)
            .read_to_end(&mut buf);
        out.body = String::from_utf8_lossy(&buf).to_string();
    };

    match req.call() {
        Ok(resp) => read_body(resp, &mut out),
        Err(ureq::Error::Status(code, resp)) => {
            out.status = Some(code);
            let mut buf = Vec::new();
            let _ = resp.into_reader().take(64 * 1024).read_to_end(&mut buf);
            out.body = String::from_utf8_lossy(&buf).to_string();
        }
        Err(ureq::Error::Transport(t)) => out.error = t.to_string(),
    }
    out.elapsed_ms = started.elapsed().as_millis();
    out
}

/// 发一个 GET（不带额外请求头），返回报告（永不 panic）
pub fn get(url: &str, timeout: Duration) -> ProbeReport {
    get_with(url, timeout, &[])
}

/// 发一个 GET，可带额外请求头（门户登录要 Referer / 老式 UA）
pub fn get_with(url: &str, timeout: Duration, headers: &[(&str, &str)]) -> ProbeReport {
    let f = fetch(url, timeout, headers);
    ProbeReport {
        url: url.to_string(),
        status: f.status,
        body_head: f.body.trim().chars().take(300).collect(),
        error: f.error,
        elapsed_ms: f.elapsed_ms,
    }
}

/// 判"这个地址能不能通"：只要拿到任何 HTTP 状态码就算可达
/// （实测：宿舍门户 `/a79.htm` 返回 **404**，但 4 ms 就回来了，链路是通的）
pub fn reachable(url: &str, timeout: Duration) -> bool {
    get(url, timeout).status.is_some()
}

/// 单次探测的结论
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Online,
    Offline,
    Hijacked,
}

impl Verdict {
    pub fn text(self) -> &'static str {
        match self {
            Verdict::Online => "在线",
            Verdict::Offline => "离线",
            Verdict::Hijacked => "被门户劫持",
        }
    }
}

/// 是不是"内容型"探测点（返回固定文本，而不是 204 空响应）
fn is_content_probe(url: &str) -> bool {
    url.contains("msftconnecttest.com/connecttest.txt") || url.contains("captive.apple.com/hotspot-detect.html")
}

/// 单条探测的判定。
///
/// **M5 修正（用户实测反馈）**：旧逻辑把"任何 200 且内容不像门户"都算在线，
/// 结果校园网**没登录**时 AC 把请求劫持成登录页（200/重定向后 200），也被判成
/// "在线"，于是永远不会去登录。现在改成：
/// - 只有 **204 + 空 body** 才算在线（generate_204 的标准语义）；
/// - 204 却带内容 → 中间设备改写过 → 劫持；
/// - generate_204 类端点返回任何 2xx/3xx → 劫持（未认证）；
/// - 内容型探测点必须**全文匹配**预期文本才算在线；
/// - 其余状态码 → 离线。
fn judge(url: &str, r: &ProbeReport) -> Option<Verdict> {
    let status = r.status?;
    let body = r.body_head.trim();

    match status {
        204 => {
            if body.is_empty() {
                Some(Verdict::Online)
            } else {
                Some(Verdict::Hijacked)
            }
        }
        200 if is_content_probe(url) => {
            // 微软连通性探测：正常应正好是 "Microsoft Connect Test"
            if body.eq_ignore_ascii_case("Microsoft Connect Test") {
                Some(Verdict::Online)
            } else {
                Some(Verdict::Hijacked)
            }
        }
        200..=299 | 300..=399 => {
            // 到 generate_204 却回了内容/重定向：说明被门户接管了
            Some(Verdict::Hijacked)
        }
        _ => Some(Verdict::Offline),
    }
}

/// 和 `internet_ok` 一样，但**一旦确认在线就提前结束**（在线巡检用，省流量）
pub fn internet_ok_quick(urls: &[&str], timeout: Duration) -> (Verdict, Vec<ProbeReport>) {
    let mut reports = Vec::new();
    for url in urls {
        let r = get(url, timeout);
        let online = judge(url, &r) == Some(Verdict::Online);
        reports.push(r);
        if online {
            return (Verdict::Online, reports);
        }
    }
    let verdict = if reports.iter().any(|r| judge(&r.url, r) == Some(Verdict::Hijacked)) {
        Verdict::Hijacked
    } else {
        Verdict::Offline
    };
    (verdict, reports)
}

/// 探一组 URL，聚合出结论（任意一个严格在线即在线；出现劫持则优先判劫持）
pub fn internet_ok(urls: &[&str], timeout: Duration) -> (Verdict, Vec<ProbeReport>) {
    let mut reports = Vec::new();
    let mut online = 0usize;
    let mut hijacked = 0usize;
    let mut answered = 0usize;

    for url in urls {
        let r = get(url, timeout);
        if let Some(v) = judge(url, &r) {
            answered += 1;
            match v {
                Verdict::Online => online += 1,
                Verdict::Hijacked => hijacked += 1,
                Verdict::Offline => {}
            }
        }
        reports.push(r);
    }

    // 任意一条拿到"严格 204"就说明真的能出网（优先于别的探测点被劫持）
    let verdict = if online > 0 {
        Verdict::Online
    } else if hijacked > 0 {
        Verdict::Hijacked
    } else {
        let _ = answered;
        Verdict::Offline
    };
    (verdict, reports)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(url: &str, status: Option<u16>, body: &str) -> ProbeReport {
        ProbeReport {
            url: url.into(),
            status,
            body_head: body.into(),
            error: String::new(),
            elapsed_ms: 0,
        }
    }

    #[test]
    fn judge_rules() {
        let g = "http://cp.cloudflare.com/generate_204";
        // 标准在线
        assert_eq!(judge(g, &probe(g, Some(204), "")), Some(Verdict::Online));
        // 204 却带内容 = 中间设备改写
        assert_eq!(judge(g, &probe(g, Some(204), "hi")), Some(Verdict::Hijacked));
        // 未登录被劫持成登录页（200 + 任意内容）→ 现在必须判劫持，不能再判在线
        assert_eq!(
            judge(g, &probe(g, Some(200), "<html>校园网登录</html>")),
            Some(Verdict::Hijacked)
        );
        assert_eq!(judge(g, &probe(g, Some(200), "ok")), Some(Verdict::Hijacked));
        // 重定向
        assert_eq!(judge(g, &probe(g, Some(302), "")), Some(Verdict::Hijacked));
        // 内容型探测点：匹配才在线
        let m = "http://www.msftconnecttest.com/connecttest.txt";
        assert_eq!(
            judge(m, &probe(m, Some(200), "Microsoft Connect Test")),
            Some(Verdict::Online)
        );
        assert_eq!(
            judge(m, &probe(m, Some(200), "<html>portal</html>")),
            Some(Verdict::Hijacked)
        );
        // 连不上
        assert_eq!(judge(g, &probe(g, None, "")), None);
        assert_eq!(judge(g, &probe(g, Some(403), "")), Some(Verdict::Offline));
    }

    #[test]
    fn internet_ok_needs_strict_204() {
        // 两个探测点都被劫持 → 整体判劫持（worker 会当作"需要登录"）
        let reports = vec![probe("a", Some(200), "<html>login</html>")];
        let mut hijacked = 0;
        for r in &reports {
            if judge(&r.url, r) == Some(Verdict::Hijacked) {
                hijacked += 1;
            }
        }
        assert_eq!(hijacked, 1);
    }
}
