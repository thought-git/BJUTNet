//! 公共无线 `bjut_wifi` 的门户协议（老版 drcom CGI，`login_method=0`）。
//!
//! 实测成功的请求（浏览器抓包）：
//!
//! ```text
//! GET https://wlgn.bjut.edu.cn/drcom/login?callback=dr1003
//!     &DDDDD=20250000              ← 纯学号，无后缀、无 ",1," 前缀
//!     &upass=...
//!     &0MKKey=123456
//!     &R1=0 &R2= &R3=0 &R6=1 &para=00
//!     &v6ip= &terminal_type=2 &lang=zh-cn &jsVersion=4.1 &v=6353 &lang=zh
//! Referer: https://wlgn.bjut.edu.cn/a79.htm
//! → dr1003({"result":1,"uid":"20250000","v46ip":"10.20.66.197",...})
//! ```
//!
//! 三个坑（都写在方案 §9.2 里）：
//! 1. 端口必须 **443**（不是 loadConfig 给的 801/802）；
//! 2. 账号是**纯学号**；
//! 3. `R6=1`、`jsVersion=4.1`（宿舍是 `R6=0`、`4.X`）。

use std::time::Duration;

use super::{
    classify_msg, json_int, json_str, percent_encode, rnd, split_account, unwrap_jsonp, LoginFault,
    LoginOutcome,
};
use crate::config::Config;
use crate::http;
use crate::secret::Credentials;

/// 浏览器 UA（老 CGI 有时看 UA，带上最稳）
pub const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                             (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

fn base(cfg: &Config) -> String {
    cfg.public.base.trim_end_matches('/').to_string()
}

fn referer(cfg: &Config) -> String {
    format!("{}{}", base(cfg), cfg.public.referer_path)
}

/// 老版 CGI 查询串
fn drcom_query(account: &str, password: &str, ip: &str, mac_plain: &str) -> String {
    format!(
        "callback=dr{n}&DDDDD={acct}&upass={pwd}&0MKKey=123456\
         &R1=0&R2=&R3=0&R6=1&para=00&v6ip=\
         &terminal_type=2&lang=zh-cn&jsVersion=4.1\
         &wlan_user_ip={ip}&wlan_user_mac={mac}&v={v}&lang=zh",
        n = 1000 + (rnd() % 9000),
        acct = percent_encode(account),
        pwd = percent_encode(password),
        ip = percent_encode(ip),
        mac = percent_encode(mac_plain),
        v = rnd()
    )
}

/// 登录公共无线门户
pub fn login(
    cfg: &Config,
    creds: &Credentials,
    ip: &str,
    mac: &str,
    timeout: Duration,
) -> LoginOutcome {
    let base = base(cfg);
    let (account, _) = split_account(&creds.account);
    let mac_plain = mac.replace([':', '-'], "");
    let ref_url = referer(cfg);
    let headers = [("Referer", ref_url.as_str()), ("User-Agent", BROWSER_UA)];
    let q = drcom_query(account, &creds.password, ip, &mac_plain);

    let mut attempts: Vec<String> = Vec::new();

    // ---- 尝试 1：drcom CGI（实测唯一确认可用的那条）----
    let url1 = format!("{base}/drcom/login?{q}");
    let r1 = http::fetch(&url1, timeout, &headers);
    let out1 = judge(&url1, r1, "drcom");
    attempts.push(out1.summary());
    if out1.ok {
        let mut o = out1;
        o.attempts = attempts;
        return o;
    }

    // ---- 尝试 2：同一套 CGI 参数走 eportal 端点（不同 AC 的退路）----
    let url2 = format!("{base}/eportal/portal/login?{q}");
    let r2 = http::fetch(&url2, timeout, &headers);
    let out2 = judge(&url2, r2, "eportal+cgi");
    attempts.push(out2.summary());

    let mut best = if out2.fault.is_account() { out2 } else { out1 };
    best.attempts = attempts;
    best
}

/// 统一判读
fn judge(url: &str, r: http::FetchResult, tag: &str) -> LoginOutcome {
    if !r.reachable() {
        let mut o = LoginOutcome::failure(url, LoginFault::Network, &r.error, "");
        o.elapsed_ms = r.elapsed_ms;
        return o;
    }

    let body = unwrap_jsonp(&r.body);
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        let result = json_int(&v, "result").unwrap_or(-1);
        let ret_code = json_int(&v, "ret_code").unwrap_or(-1);
        let msg = json_str(&v, "msg").unwrap_or("").to_string();

        if result == 1 {
            let mut o = LoginOutcome::success(
                url,
                if msg.is_empty() {
                    "result=1"
                } else {
                    msg.as_str()
                },
                r.elapsed_ms,
            );
            o.raw_head = body.chars().take(300).collect();
            return o;
        }
        // 与宿舍一致：本系 AC 的"已在线"只看 ret_code=2，msg 可能是数字串
        if ret_code == 2 {
            return LoginOutcome::already(
                url,
                if msg.is_empty() { "ret_code=2 已在线" } else { &msg },
            );
        }
        let mut o = LoginOutcome::failure(
            url,
            classify_msg(&msg),
            &format!("{tag}: {msg}"),
            &r.body,
        );
        o.elapsed_ms = r.elapsed_ms;
        return o;
    }

    let lower = r.body.to_ascii_lowercase();
    if lower.contains("<html") || lower.contains("<!doctype") || r.status == Some(404) {
        let mut o = LoginOutcome::failure(
            url,
            LoginFault::Endpoint,
            &format!("{tag}: 端点不存在或返回 HTML（HTTP {:?}）", r.status),
            &r.body,
        );
        o.elapsed_ms = r.elapsed_ms;
        return o;
    }
    if lower.contains("login_ok") {
        return LoginOutcome::success(url, "纯文本成功响应", r.elapsed_ms);
    }

    let mut o = LoginOutcome::failure(
        url,
        LoginFault::Unknown,
        &format!("{tag}: 响应无法解析"),
        &r.body,
    );
    o.elapsed_ms = r.elapsed_ms;
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_has_drcom_signature() {
        let q = drcom_query("20250000", "p@ss", "10.20.66.197", "AABBCCDDEEFF");
        assert!(q.contains("DDDDD=20250000"));
        assert!(q.contains("upass=p%40ss"));
        assert!(q.contains("0MKKey=123456"));
        assert!(q.contains("R1=0"));
        assert!(q.contains("R6=1"));
        assert!(q.contains("para=00"));
        assert!(q.contains("jsVersion=4.1"));
        // 不能出现宿舍那套参数
        assert!(!q.contains("user_account="));
    }
}
