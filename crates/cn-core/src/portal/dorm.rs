//! 宿舍有线的门户协议（`login_method=1` 新版 Portal + 老版 CGI 回退）。
//!
//! 关键教训：**账号后缀不能丢**。旧方案日志里反复出现
//! `user_account=20250000`（少了 `@campus`）→ 服务端回
//! `无法获取用户认证账号！` → 认证永远失败。
//! 这里做三级兜底：`loadConfig.account_suffix` → 配置 `accountSuffixFallback` → 内置 `@campus`。

use std::time::Duration;

use super::{
    b64, json_int, json_str, percent_encode, rnd, unwrap_jsonp, with_suffix, LoginFault,
    LoginOutcome,
};
use crate::config::Config;
use crate::http;
use crate::secret::Credentials;

/// `loadConfig` 里我们关心的字段
#[derive(Debug, Clone, Default)]
pub struct DormPortalCfg {
    pub login_method: i64,
    pub account_suffix: String,
    pub find_mac: i64,
    pub en_md5: i64,
}

fn base(cfg: &Config) -> String {
    cfg.dorm.base.trim_end_matches('/').to_string()
}

/// 拉门户配置（未认证状态下也能访问，所以可以当"我是不是在宿舍网"的探针）
pub fn load_config(cfg: &Config, ip: &str, timeout: Duration) -> Option<DormPortalCfg> {
    let url = format!(
        "{base}/eportal/portal/page/loadConfig?program_index=&wlan_vlan_id=1\
         &wlan_user_ip={ipb64}&wlan_user_ipv6=&wlan_user_ssid=&wlan_user_areaid=\
         &wlan_ac_ip=&wlan_ap_mac=000000000000&gw_id=000000000000",
        base = base(cfg),
        ipb64 = b64(ip)
    );
    let r = http::fetch(&url, timeout, &[]);
    if !r.reachable() {
        return None;
    }
    let body = unwrap_jsonp(&r.body);
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let data = v.get("data").unwrap_or(&v);
    let mut pc = DormPortalCfg::default();
    pc.login_method = json_int(data, "login_method")
        .or_else(|| json_int(&v, "login_method"))
        .unwrap_or(1);
    pc.account_suffix = json_str(data, "account_suffix")
        .unwrap_or("")
        .to_string();
    pc.find_mac = json_int(data, "find_mac").unwrap_or(0);
    pc.en_md5 = json_int(data, "en_md5").unwrap_or(0);
    Some(pc)
}

/// 解析账号后缀：loadConfig 优先，失败用配置兜底
pub fn resolve_suffix(cfg: &Config, pc: Option<&DormPortalCfg>) -> String {
    if let Some(p) = pc {
        let s = p.account_suffix.trim();
        if !s.is_empty() {
            return s.to_string();
        }
    }
    // 配置里显式写的兜底（默认 @campus）优先于 dorm 子节点里的兜底
    if !cfg.account_suffix_fallback.trim().is_empty() {
        return cfg.account_suffix_fallback.trim().to_string();
    }
    cfg.dorm.fallback_suffix.trim().to_string()
}

/// 登录宿舍门户。成功返回 `ok=true`；"已经在线"也算成功（`already=true`）。
pub fn login(
    cfg: &Config,
    creds: &Credentials,
    ip: &str,
    mac: &str,
    timeout: Duration,
) -> LoginOutcome {
    let base = base(cfg);
    let pc = if cfg.dorm.load_config {
        load_config(cfg, ip, timeout)
    } else {
        None
    };
    let suffix = resolve_suffix(cfg, pc.as_ref());
    let account = with_suffix(&creds.account, &suffix);
    let login_method = pc.as_ref().map(|p| p.login_method).unwrap_or(1);
    let mac_plain = mac.replace([':', '-'], "");

    let mut attempts: Vec<String> = Vec::new();
    // 依次尝试，最终报告"第一处账号类失败"（更贴近真实原因）
    let mut chosen: Option<LoginOutcome> = None;
    let keep = |cand: LoginOutcome, chosen: &mut Option<LoginOutcome>| {
        let better = match chosen.as_ref() {
            None => true,
            Some(cur) => !cur.fault.is_account() && cand.fault.is_account(),
        };
        if better {
            *chosen = Some(cand);
        }
    };

    // ---- 尝试 1：新版 Portal 参数（login_method=1），账号带后缀 ----
    let account_has_suffix = account.contains('@') && !suffix.is_empty();
    crate::logging::debug(format!(
        "门户登录尝试: account={account} suffix={suffix} login_method={login_method}"
    ));
    let url1 = format!(
        "{base}/eportal/portal/login?{q}",
        base = base,
        q = portal_query(login_method, &account, &creds.password, ip, &mac_plain)
    );
    let r1 = http::fetch(&url1, timeout, &[]);
    let out1 = judge(&url1, r1, &format!("portal(login_method={login_method})"));
    attempts.push(out1.summary());
    if out1.ok {
        let mut o = out1;
        o.attempts = attempts;
        return o;
    }
    // 账号格式被拒时（不同 AC 对 user_account 的期望不同），换"纯学号"再试一次。
    // 只在响应明确指向账号格式/不存在时才换，避免多试一次让密码错计数翻倍。
    let looks_account_format = out1.message.contains("无法获取")
        || out1.message.contains("账号")
        || out1.message.to_ascii_lowercase().contains("account");
    keep(out1, &mut chosen);

    if account_has_suffix && looks_account_format {
        let bare = super::split_account(&creds.account).0;
        let url1b = format!(
            "{base}/eportal/portal/login?{q}",
            base = base,
            q = portal_query(login_method, bare, &creds.password, ip, &mac_plain)
        );
        let r1b = http::fetch(&url1b, timeout, &[]);
        let out1b = judge(&url1b, r1b, &format!("portal(login_method={login_method}, 纯学号)"));
        attempts.push(out1b.summary());
        if out1b.ok {
            let mut o = out1b;
            o.attempts = attempts;
            return o;
        }
        keep(out1b, &mut chosen);
    }

    // ---- 尝试 2：老版 CGI 参数（同一端点，纯学号）----
    let url2 = format!(
        "{base}/eportal/portal/login?{q}",
        base = base,
        q = legacy_query(
            super::split_account(&creds.account).0,
            &creds.password,
            ip,
            &mac_plain
        )
    );
    let r2 = http::fetch(&url2, timeout, &[]);
    let out2 = judge(&url2, r2, "legacy(DDDDD/upass)");
    attempts.push(out2.summary());
    if out2.ok {
        let mut o = out2;
        o.attempts = attempts;
        return o;
    }
    keep(out2, &mut chosen);

    // 走到这里至少有过一次尝试，chosen 必然有值
    let mut best = chosen.unwrap_or_else(|| {
        LoginOutcome::failure(&url1, LoginFault::Unknown, "登录失败（没有可用的尝试结果）", "")
    });
    best.attempts = attempts;
    best
}

/// 退出登录。
///
/// 实测（2026-11-12）：
/// - 只带 `user_account` 会被拒：`注销操作非本机终端！`
/// - 必须同时带 `wlan_user_ip` + `wlan_user_mac`，且账号用**纯学号**（带 @ 后缀反而失败）
/// - 成功响应：`{"result":1,"msg":"Portal协议注销成功！"}`
pub fn logout(
    cfg: &Config,
    creds: &Credentials,
    ip: &str,
    mac: &str,
    timeout: Duration,
) -> LoginOutcome {
    let base = base(cfg);
    let (id, _) = super::split_account(&creds.account);
    let mac_plain = mac.replace([':', '-'], "");
    let url = format!(
        "{base}/eportal/portal/logout?login_method=1&user_account={acct}         &wlan_user_ip={ip}&wlan_user_mac={mac}         &jsVersion=4.X&terminal_type=1&lang=zh&v={v}",
        base = base,
        acct = percent_encode(id),
        ip = percent_encode(ip),
        mac = percent_encode(&mac_plain),
        v = rnd()
    );
    let r = http::fetch(&url, timeout, &[]);
    let body = unwrap_jsonp(&r.body);
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let result = json_int(&v, "result").unwrap_or(-1);
    if result == 1 {
        let msg = json_str(&v, "msg").unwrap_or("Portal 注销成功");
        return LoginOutcome::success(&url, msg, r.elapsed_ms);
    }
    let msg = json_str(&v, "msg").unwrap_or("").to_string();
    LoginOutcome::failure(
        &url,
        if msg.is_empty() {
            LoginFault::Unknown
        } else {
            classify(&msg)
        },
        if msg.is_empty() { "退出失败（无 msg）" } else { msg.as_str() },
        &r.body,
    )
}

fn portal_query(
    login_method: i64,
    account: &str,
    password: &str,
    ip: &str,
    mac_plain: &str,
) -> String {
    format!(
        "login_method={m}&user_account={acct}&user_password={pwd}\
         &wlan_user_ip={ip}&wlan_user_ipv6=&wlan_user_mac={mac}\
         &wlan_ac_ip=&wlan_ac_name=&jsVersion=4.X&terminal_type=1&lang=zh&v={v}",
        m = login_method,
        acct = super::percent_encode_account(account),
        pwd = percent_encode(password),
        ip = percent_encode(ip),
        mac = percent_encode(mac_plain),
        v = rnd()
    )
}

fn legacy_query(account: &str, password: &str, ip: &str, mac_plain: &str) -> String {
    format!(
        "DDDDD={acct}&upass={pwd}&0MKKey=123456&R1=&R2=&R3=&R6=0&para=&v6ip=\
         &terminal_type=1&lang=zh&wlan_user_ip={ip}&wlan_user_mac={mac}&v={v}",
        acct = super::percent_encode_account(account),
        pwd = percent_encode(password),
        ip = percent_encode(ip),
        mac = percent_encode(mac_plain),
        v = rnd()
    )
}

/// 统一判读门户响应
fn judge(url: &str, r: http::FetchResult, tag: &str) -> LoginOutcome {
    if !r.reachable() {
        let mut o = LoginOutcome::failure(url, LoginFault::Network, &r.error, "");
        o.elapsed_ms = r.elapsed_ms;
        return o;
    }

    let body = unwrap_jsonp(&r.body);
    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();

    if let Some(v) = parsed {
        let result = json_int(&v, "result").unwrap_or(-1);
        let ret_code = json_int(&v, "ret_code").unwrap_or(-1);
        let msg = json_str(&v, "msg").unwrap_or("").to_string();

        if result == 1 {
            let mut o = LoginOutcome::success(url, if msg.is_empty() { "result=1" } else { &msg }, r.elapsed_ms);
            o.raw_head = body.chars().take(300).collect();
            return o;
        }
        // 实测：本 AC 对"已经在线"返回 ret_code=2 且 msg 是数字串（如 "512"），
        // 并不是文案"已经在线"——所以只认 ret_code，别认 msg。
        if ret_code == 2 {
            return LoginOutcome::already(
                url,
                if msg.is_empty() { "ret_code=2 已在线" } else { &msg },
            );
        }
        let mut o = LoginOutcome::failure(url, classify(&msg), &format!("{tag}: {msg}"), &r.body);
        o.elapsed_ms = r.elapsed_ms;
        return o;
    }

    // 不是 JSON：可能是 404/HTML（端点不存在）或老式纯文本成功
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
    // 老 drcom 的成功响应有时就是纯文本
    if lower.contains("login_ok") || lower.contains("dr100") {
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

/// 从门户 msg 判失败类型（实现统一在 `portal::classify_msg`）
fn classify(msg: &str) -> LoginFault {
    super::classify_msg(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn suffix_prefers_loadconfig() {
        let c = cfg();
        let pc = DormPortalCfg {
            login_method: 1,
            account_suffix: "@campus".into(),
            ..Default::default()
        };
        assert_eq!(resolve_suffix(&c, Some(&pc)), "@campus");
    }

    #[test]
    fn suffix_falls_back_to_config_when_loadconfig_fails() {
        let c = cfg(); // accountSuffixFallback 默认 @campus
        assert_eq!(resolve_suffix(&c, None), "@campus");
    }

    #[test]
    fn suffix_falls_back_to_inner_default() {
        let mut c = cfg();
        c.account_suffix_fallback = String::new();
        c.dorm.fallback_suffix = "@campus-inner".into();
        assert_eq!(resolve_suffix(&c, None), "@campus-inner");
    }

    #[test]
    fn classify_account_errors() {
        assert_eq!(classify("无法获取用户认证账号！"), LoginFault::AccountPassword);
        assert_eq!(classify("Authentication fail"), LoginFault::AccountPassword);
        assert_eq!(classify("密码错误"), LoginFault::AccountPassword);
        assert_eq!(classify("connect timeout"), LoginFault::Network);
    }
}


/// 门户在线会话（`online_list` 的一行）
#[derive(Debug, Clone, Default)]
pub struct OnlineSession {
    pub account: String,
    pub ip: String,
    pub mac: String,
    pub online_time: String,
    pub time_long_sec: i64,
    pub session: i64,
}

/// 查门户当前在线列表（未认证时也能调，用来做二次确认/诊断）
pub fn online_list(cfg: &Config, timeout: Duration) -> Option<Vec<OnlineSession>> {
    let url = format!(
        "{base}/eportal/portal/online_list?user_account=&user_password=&wlan_user_mac=         &wlan_user_ip=0&curr_user_ip=0&jsVersion=4.X",
        base = base(cfg)
    );
    let r = http::fetch(&url, timeout, &[]);
    if !r.reachable() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(unwrap_jsonp(&r.body)).ok()?;
    if json_int(&v, "result").unwrap_or(0) != 1 {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    if let Some(list) = v.get("list").and_then(|l| l.as_array()) {
        for item in list {
            out.push(OnlineSession {
                account: json_str(item, "user_account").unwrap_or("").to_string(),
                ip: json_str(item, "online_ip").unwrap_or("").to_string(),
                mac: json_str(item, "online_mac").unwrap_or("").to_string(),
                online_time: json_str(item, "online_time").unwrap_or("").to_string(),
                time_long_sec: json_str(item, "time_long")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
                session: json_str(item, "online_session")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
            });
        }
    }
    Some(out)
}

/// 门户是否认为本机 IP 在线（用于 204 之外的第二信号）
pub fn is_online(cfg: &Config, ip: &str, timeout: Duration) -> bool {
    online_list(cfg, timeout)
        .map(|list| list.iter().any(|s| s.ip == ip))
        .unwrap_or(false)
}
