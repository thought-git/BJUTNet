//! 计费自助系统（jfself.bjut.edu.cn，Dr.COM 用户自助服务）只读客户端。
//!
//! 只做三件事：登录、取 dashboard、解析出「余额 / 已用流量 / 余量」。
//! **不缴费、不改套餐、不改密码**。
//!
//! 实测（2026-11）登录流程：
//! 1. GET  `/Self/login/`            → 拿 `JSESSIONID` 和隐藏的 `checkcode`
//! 2. GET  `/Self/login/randomCode`  → 拿验证码图片（同时轮换 JSESSIONID）
//! 3. POST `/Self/login/verify`      → account / password / code / checkcode / foo / bar / submit
//! 4. GET  `/Self/dashboard`         → HTML 里内嵌一段 `window.user = ...` 的 JSON
//!
//! 注意：失败若干次后必须带验证码；密码是明文随 TLS 提交（站点前端哈希被关闭）。

use std::io::Read;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 一次计费查询的结果（单位：元 / MB / 分钟）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BillingData {
    /// 账户余额（元）；负数表示欠费
    pub balance_yuan: f64,
    /// 已用流量（MB）
    pub used_mb: f64,
    /// 剩余流量（MB）
    pub left_mb: f64,
    /// 已用时长（分钟，0 表示不限时/无数据）
    pub used_time_min: u64,
    /// 当前套餐价格（元）；解析不到为 None
    pub package_price_yuan: Option<f64>,
    /// 查询时间（unix 秒）
    pub fetched_epoch: i64,
}

impl BillingData {
    pub fn total_mb(&self) -> f64 {
        (self.used_mb + self.left_mb).max(0.0)
    }

    /// 学校计费系统**不会把余额减成负数**，最低就是 0；
    /// 所以「余额为 0」等同于欠费/停机，需要提醒。
    pub fn is_arrears(&self) -> bool {
        self.balance_yuan <= 0.005
    }

    /// 兼容旧字段：余额为 0 时返回 0（不再有"欠费金额"）
    pub fn arrears_yuan(&self) -> f64 {
        0.0
    }

    /// 面板/弹窗用的状态文案
    pub fn balance_state(&self) -> &'static str {
        if self.is_arrears() {
            "余额为 0（可能已停机）"
        } else if self.balance_below_package() {
            "余额不足以支付套餐"
        } else {
            "正常"
        }
    }

    /// 余额是否不足以支付当前套餐（套餐价 Unknown 时返回 false）
    pub fn balance_below_package(&self) -> bool {
        match self.package_price_yuan {
            Some(price) => self.balance_yuan < price,
            None => false,
        }
    }

    /// 当月剩余流量是否低于阈值（threshold=0 表示关闭）
    pub fn flow_below(&self, threshold_mb: u64) -> bool {
        threshold_mb > 0 && self.left_mb < threshold_mb as f64
    }
}

/// 登录/取数的失败分类
#[derive(Debug)]
pub enum BillingError {
    /// 需要验证码：带上图片给用户看
    NeedCaptcha { image: Vec<u8> },
    /// 账号/密码问题（不重试，避免锁号）
    Auth(String),
    /// 网络 / TLS / 超时
    Network(String),
    /// 页面结构变了
    Parse(String),
}

impl BillingError {
    pub fn text(&self) -> String {
        match self {
            BillingError::NeedCaptcha { .. } => "需要验证码".into(),
            BillingError::Auth(m) => format!("账号或密码错误：{m}"),
            BillingError::Network(m) => format!("网络错误：{m}"),
            BillingError::Parse(m) => format!("页面解析失败（站点可能改版）：{m}"),
        }
    }
}

const DEFAULT_BASE: &str = "https://jfself.bjut.edu.cn";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0 Safari/537.36";

/// 带 Cookie 的最小会话
pub struct BillingClient {
    agent: ureq::Agent,
    cookies: Vec<(String, String)>,
    checkcode: String,
}

impl BillingClient {
    pub fn new(timeout: Duration) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(timeout)
            .timeout_read(timeout)
            .timeout_write(timeout)
            .user_agent(UA)
            // 关键：不要自动跟随 302——ureq 跟随时会丢掉我们手写的 Cookie
            .redirects(0)
            .build();
        Self {
            agent,
            cookies: Vec::new(),
            checkcode: String::new(),
        }
    }

    fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn absorb(&mut self, resp: &ureq::Response) {
        for raw in resp.all("Set-Cookie") {
            let first = raw.split(';').next().unwrap_or("");
            if let Some((k, v)) = first.split_once('=') {
                let k = k.trim().to_string();
                let v = v.trim().to_string();
                if k.is_empty() {
                    continue;
                }
                if let Some(slot) = self.cookies.iter_mut().find(|(n, _)| *n == k) {
                    slot.1 = v;
                } else {
                    self.cookies.push((k, v));
                }
            }
        }
    }

    fn get(&mut self, url: &str) -> Result<(u16, String, String), BillingError> {
        let cookie = self.cookie_header();
        let mut req = self.agent.get(url);
        if !cookie.is_empty() {
            req = req.set("Cookie", &cookie);
        }
        match req.call() {
            Ok(r) => {
                self.absorb(&r);
                let status = r.status();
                let location = header(&r, "location").unwrap_or_default();
                let body = r.into_string().unwrap_or_default();
                Ok((status, location, body))
            }
            Err(ureq::Error::Status(code, r)) => {
                self.absorb(&r);
                let location = header(&r, "location").unwrap_or_default();
                let body = r.into_string().unwrap_or_default();
                Ok((code, location, body))
            }
            Err(e) => Err(BillingError::Network(e.to_string())),
        }
    }

    fn get_bytes(&mut self, url: &str) -> Result<Vec<u8>, BillingError> {
        let cookie = self.cookie_header();
        let mut req = self.agent.get(url);
        if !cookie.is_empty() {
            req = req.set("Cookie", &cookie);
        }
        match req.call() {
            Ok(r) => {
                self.absorb(&r);
                let mut buf = Vec::new();
                r.into_reader()
                    .take(2 * 1024 * 1024)
                    .read_to_end(&mut buf)
                    .map_err(|e| BillingError::Network(e.to_string()))?;
                Ok(buf)
            }
            Err(ureq::Error::Status(code, r)) => {
                self.absorb(&r);
                Err(BillingError::Network(format!("验证码接口 HTTP {code}")))
            }
            Err(e) => Err(BillingError::Network(e.to_string())),
        }
    }

    /// 打开登录页：拿 Cookie + 隐藏的 checkcode
    pub fn begin(&mut self) -> Result<bool, BillingError> {
        let url = format!("{DEFAULT_BASE}/Self/login/");
        let (status, location, body) = self.get(&url)?;
        if (300..400).contains(&status) && location.contains("/Self/dashboard") {
            return Ok(true); // 会话已登录
        }
        self.checkcode = extract_attr(&body, "checkcode").unwrap_or_default();
        crate::logging::debug(format!(
            "billing: 登录页 cookies={} checkcode={}",
            self.cookies.len(),
            self.checkcode
        ));
        Ok(false)
    }

    /// 取验证码图片（会轮换 JSESSIONID）
    pub fn captcha(&mut self) -> Result<Vec<u8>, BillingError> {
        let url = format!("{DEFAULT_BASE}/Self/login/randomCode?t={}", rnd());
        self.get_bytes(&url)
    }

    /// 提交登录；`code` 为空时如果站点要求验证码会返回 NeedCaptcha
    pub fn login(
        &mut self,
        account: &str,
        password: &str,
        code: Option<&str>,
    ) -> Result<bool, BillingError> {
        let url = format!("{DEFAULT_BASE}/Self/login/verify");
        let form = format!(
            "account={}&password={}&code={}&checkcode={}&foo=&bar=&submit={}",
            encode(account),
            encode(password),
            encode(code.unwrap_or("")),
            encode(&self.checkcode),
            encode("登录")
        );
        let cookie = self.cookie_header();
        let mut req = self
            .agent
            .post(&url)
            .set("Content-Type", "application/x-www-form-urlencoded")
            .set("Referer", &format!("{DEFAULT_BASE}/Self/login/"))
            .set("Origin", DEFAULT_BASE);
        if !cookie.is_empty() {
            req = req.set("Cookie", &cookie);
        }
        let resp = match req.send_string(&form) {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(BillingError::Network(e.to_string())),
        };
        self.absorb(&resp);
        let status = resp.status();
        let location = header(&resp, "location").unwrap_or_default();
        let body = resp.into_string().unwrap_or_default();
        crate::logging::debug(format!(
            "billing: 登录响应 status={} location={} len={} 有验证码块={} tip={:?}",
            status,
            location,
            body.len(),
            body.contains("id=\"randomDiv\""),
            extract_tip(&body)
        ));
        if (300..400).contains(&status) {
            // 302 到非登录页 = 登录成功（dashboard 等）
            if !location.contains("/Self/login") {
                return Ok(true);
            }
            return Err(BillingError::NeedCaptcha { image: Vec::new() });
        }
        if body.contains("id=\"LoginHistoryBig\"") {
            return Ok(true);
        }
        // 还停在登录页：区分"要验证码"和"账号密码错"
        if body.contains("id=\"randomDiv\"") && !body.contains("id=\"randomDiv\" class=\"hide\"") {
            return Err(BillingError::NeedCaptcha { image: Vec::new() });
        }
        let msg = extract_tip(&body).unwrap_or_else(|| "登录被拒绝".into());
        Err(BillingError::Auth(msg))
    }

    /// 取 dashboard 并解析
    pub fn snapshot(&mut self) -> Result<BillingData, BillingError> {
        let url = format!("{DEFAULT_BASE}/Self/dashboard");
        let (status, location, body) = self.get(&url)?;
        if (300..400).contains(&status) {
            let _ = location;
            return Err(BillingError::Auth("会话已失效，请重新登录".into()));
        }
        parse_dashboard(&body)
    }
}

/// 一次完整查询：begin → captcha → login → dashboard。
///
/// `code` 为 None 时先试一次不带验证码的登录；如果站点要验证码，返回
/// `NeedCaptcha`（附图片），调用方让用户看图输入后带 code 再来一次。
pub fn fetch(
    account: &str,
    password: &str,
    code: Option<&str>,
    timeout: Duration,
) -> Result<BillingData, BillingError> {
    let mut c = BillingClient::new(timeout);
    let already = c.begin()?;
    if already {
        return c.snapshot();
    }
    let image = c.captcha()?;
    match c.login(account, password, code) {
        Ok(true) => c.snapshot(),
        Ok(false) => Err(BillingError::Auth("登录未成功".into())),
        Err(BillingError::NeedCaptcha { .. }) => Err(BillingError::NeedCaptcha { image }),
        Err(BillingError::Auth(_)) if code.is_none() && !image.is_empty() => {
            // 站点没提示要验证码但也没登录成功：把验证码图片交给用户再试一次
            Err(BillingError::NeedCaptcha { image })
        }
        Err(e) => Err(e),
    }
}

/// 从 dashboard HTML 里解析内嵌的 user JSON
pub fn parse_dashboard(html: &str) -> Result<BillingData, BillingError> {
    let marker = html
        .find("\"leftMoney\"")
        .ok_or_else(|| BillingError::Parse("没找到 leftMoney 字段".into()))?;
    let open = html[..marker]
        .rfind("})(")
        .map(|i| i + 3)
        .ok_or_else(|| BillingError::Parse("没找到内嵌 JSON 起点".into()))?;
    let json = balanced_object(&html[open..])
        .ok_or_else(|| BillingError::Parse("内嵌 JSON 括号不匹配".into()))?;
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| BillingError::Parse(format!("JSON 解析失败: {e}")))?;
    let num = |k: &str| v.get(k).and_then(|x| x.as_f64());
    let balance = num("leftMoney").ok_or_else(|| BillingError::Parse("缺 leftMoney".into()))?;
    let used = num("useFlow").unwrap_or(0.0);
    let left = num("leftFlow").unwrap_or(0.0);
    let used_time = v.get("useTime").and_then(|x| x.as_u64()).unwrap_or(0);
    let package_price = v
        .get("serviceDefault")
        .and_then(|x| x.get("baseMoney"))
        .and_then(|x| x.as_f64())
        .or_else(|| {
            v.get("userGroup")
                .and_then(|x| x.get("baseMoney"))
                .and_then(|x| x.as_f64())
        });
    Ok(BillingData {
        balance_yuan: balance,
        used_mb: used,
        left_mb: left,
        used_time_min: used_time,
        package_price_yuan: package_price,
        fetched_epoch: chrono::Local::now().timestamp(),
    })
}

fn header(resp: &ureq::Response, name: &str) -> Option<String> {
    resp.header(name).map(|s| s.to_string())
}

fn balanced_object(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut in_str = false;
    let mut esc = false;
    let start = s.find('{')?;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        let c = b as char;
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return s.get(start..=i);
                }
            }
            _ => {}
        }
    }
    None
}

fn extract_attr(html: &str, name: &str) -> Option<String> {
    let pat = format!("name=\"{name}\"");
    let i = html.find(&pat)?;
    let rest = &html[i + pat.len()..];
    let v = rest.find("value=\"")?;
    let rest = &rest[v + 7..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// 登录失败页上服务端塞进 JS 的提示文案
fn extract_tip(html: &str) -> Option<String> {
    let i = html.find("(function (tip)")?;
    let rest = &html[i..];
    let close = rest.find("})(")?;
    let arg = &rest[close + 3..];
    let end = arg.find(')')?;
    let s = arg[..end].trim().trim_matches('"').trim();
    if s.is_empty() || s == "null" {
        None
    } else {
        Some(s.to_string())
    }
}

fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
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

fn rnd() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
        % 1_000_000
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML: &str = r#"<script>
(function (user) { window.user = user || {}; })({"leftMoney":20.44501,"leftFlow":42951.626,"useFlow":79928.374,"useTime":19200,"userName":"20250000","serviceDefault":{"baseMoney":20}});
</script><div id="randomDiv" class="hide"></div>"#;

    #[test]
    fn parse_embedded_user_json() {
        let d = parse_dashboard(HTML).unwrap();
        assert!((d.balance_yuan - 20.44501).abs() < 1e-4);
        assert!((d.used_mb - 79928.374).abs() < 1e-3);
        assert!((d.left_mb - 42951.626).abs() < 1e-3);
        assert_eq!(d.used_time_min, 19200);
        assert!((d.total_mb() - 122880.0).abs() < 1.0);
        assert!(!d.is_arrears());
        assert_eq!(d.arrears_yuan(), 0.0);
        assert_eq!(d.package_price_yuan, Some(20.0));
        assert!(!d.balance_below_package()); // 20.445 > 20
        assert!(d.flow_below(45000));        // 42951 < 45000
        assert!(!d.flow_below(40000));
        assert!(!d.flow_below(0));           // 0 = 关闭
    }

    #[test]
    fn zero_balance_is_arrears() {
        // 学校计费系统最低就是 0，不会出现负数
        let html = r#"(function (user) { window.user = user || {}; })({"leftMoney":0,"leftFlow":100,"useFlow":200,"serviceDefault":{"baseMoney":20}});"#;
        let d = parse_dashboard(html).unwrap();
        assert!(d.is_arrears());
        assert_eq!(d.balance_state(), "余额为 0（可能已停机）");
        assert_eq!(d.arrears_yuan(), 0.0);
    }

    #[test]
    fn tiny_positive_balance_is_not_arrears() {
        let html = r#"(function (user) { window.user = user || {}; })({"leftMoney":0.01,"leftFlow":100,"useFlow":200,"serviceDefault":{"baseMoney":20}});"#;
        let d = parse_dashboard(html).unwrap();
        assert!(!d.is_arrears());
        // 0.01 元虽然不为 0，但低于 20 元套餐价
        assert_eq!(d.balance_state(), "余额不足以支付套餐");
        assert!(d.balance_below_package());
    }

    #[test]
    fn parse_garbage_fails() {
        assert!(parse_dashboard("<html>nothing</html>").is_err());
    }

    #[test]
    fn form_encoding() {
        assert_eq!(encode("a b@c"), "a%20b%40c");
        assert_eq!(encode("20250000"), "20250000");
    }
}
