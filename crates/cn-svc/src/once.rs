//! 一次性命令：`--login-once` / `--logout`。
//!
//! 存在的意义：**不装服务、不改系统，就能验证协议参数对不对**。
//! 排查"为什么服务登不上"时，先跑 `--login-once`，它能直接告诉你门户返回了什么。

use cn_core::config::Config;
use cn_core::net;
use cn_core::paths;
use cn_core::portal::{self, NetworkKind};
use cn_core::secret;
use cn_core::wlan;

/// 读配置 + 凭据 + 活跃网卡；任一步失败打印原因并返回 None
fn prepare() -> Option<(Config, secret::Credentials, net::Adapter)> {
    let (cfg, outcome) = Config::load_or_create(&paths::config_path());
    println!("配置: {}（{}）", outcome.text(), paths::show(&paths::config_path()));

    let creds = match secret::load(&paths::secret_path()) {
        Ok(Some(c)) => c,
        Ok(None) => {
            eprintln!(
                "✗ 还没有保存学号/密码（{} 不存在）",
                paths::show(&paths::secret_path())
            );
            return None;
        }
        Err(e) => {
            eprintln!("✗ 凭据解不开: {e}");
            return None;
        }
    };
    println!(
        "账号: {}（最后修改 {}）",
        creds.masked_account(),
        creds.updated_at
    );

    let adapters = net::list_adapters().ok()?;
    let active = match net::active_adapter(&adapters) {
        Some(a) => a,
        None => {
            eprintln!("✗ 没有可用的活跃出口（网线/WiFi 都没连？）");
            return None;
        }
    };
    println!("出口: {}", active.summary());
    Some((cfg, creds, active))
}

/// 写入凭据：密码从 **stdin** 读一行（不放在命令行里，免得进进程列表/日志）。
///
/// 这个命令的定位：无 GUI 时的手动配置 + 排障。正式使用由 GUI 通过 IPC 让服务写。
pub fn set_credentials(account: &str) -> i32 {
    use std::io::BufRead;

    let account = account.trim();
    if account.is_empty() {
        eprintln!("✗ 用法: BJUTNetSvc.exe --set-credentials <学号>  （密码从 stdin 读一行）");
        return 3;
    }

    let mut pwd = String::new();
    match std::io::stdin().lock().read_line(&mut pwd) {
        Ok(0) => {
            eprintln!("✗ 没有从 stdin 读到密码");
            return 3;
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("✗ 读 stdin 失败: {e}");
            return 3;
        }
    }
    let pwd = pwd.trim_end_matches(['\r', '\n']).to_string();
    if pwd.is_empty() {
        eprintln!("✗ 密码为空");
        return 3;
    }

    let creds = secret::Credentials::new(account, pwd);
    match secret::save(&paths::secret_path(), &creds) {
        Ok(()) => {
            println!(
                "✓ 凭据已保存到 {}（账号 {}，密码长度 {}）",
                paths::show(&paths::secret_path()),
                creds.masked_account(),
                creds.password.chars().count()
            );
            // 顺手把账号名写进 config.json，方便界面显示（密码不落 config）
            let cfg_path = paths::config_path();
            let (mut cfg, _) = Config::load_or_create(&cfg_path);
            if cfg.account != account {
                cfg.account = account.to_string();
                if let Err(e) = cfg.save(&cfg_path) {
                    eprintln!("! 写 config.json 失败（不影响登录）: {e}");
                }
            }
            0
        }
        Err(e) => {
            eprintln!("✗ 保存失败: {e}");
            2
        }
    }
}

/// 跑一次"判别 + 登录"，打印全过程。返回进程退出码。
pub fn login_once() -> i32 {
    let Some((cfg, creds, active)) = prepare() else {
        return 3;
    };
    let timeout = cfg.probe_timeout();

    let ssid = wlan::current_ssid();
    let ipv4 = active.ipv4.first().cloned();
    let gw = active.gateway.clone();
    let (kind, ev) = portal::detect(&cfg, ssid.as_deref(), ipv4.as_deref(), &gw, timeout);
    println!("SSID: {}", ssid.as_deref().unwrap_or("<未连接无线>"));
    println!("判别: {} ← {}", kind.text(), ev.text());
    for n in &ev.notes {
        println!("  · {n}");
    }

    match kind {
        NetworkKind::None => {
            println!("结论: 非校园网，不发起登录");
            return 3;
        }
        NetworkKind::Dorm => {
            if let Some(pc) = portal::dorm::load_config(&cfg, ipv4.as_deref().unwrap_or(""), timeout) {
                println!(
                    "宿舍门户配置: login_method={} account_suffix='{}' find_mac={} en_md5={}",
                    pc.login_method, pc.account_suffix, pc.find_mac, pc.en_md5
                );
                let suffix = portal::dorm::resolve_suffix(&cfg, Some(&pc));
                println!("实际使用后缀: '{suffix}'");
            } else {
                println!("宿舍门户配置: 读取失败（会用配置里的兜底后缀）");
            }
        }
        NetworkKind::Public => {
            println!("公共无线：账号按纯学号提交（自动去掉 @ 后缀）");
        }
    }

    let ip = ipv4.unwrap_or_default();
    let mac = active.mac.clone();
    println!(
        "提交登录: 账号={} ip={} mac={}",
        creds.masked_account(),
        ip,
        if mac.is_empty() { "-" } else { &mac }
    );

    let out = match kind {
        NetworkKind::Dorm => portal::dorm::login(&cfg, &creds, &ip, &mac, timeout),
        NetworkKind::Public => portal::public::login(&cfg, &creds, &ip, &mac, timeout),
        NetworkKind::None => unreachable!(),
    };
    for a in &out.attempts {
        println!("  尝试: {a}");
    }
    println!("结果: {}", out.summary());
    if !out.raw_head.is_empty() {
        println!("响应片段: {}", out.raw_head.replace(['\r', '\n'], " "));
    }
    if out.ok {
        0
    } else {
        2
    }
}

/// 尝试退出登录（目前只有宿舍门户的 logout 端点）
pub fn logout() -> i32 {
    let Some((cfg, creds, active)) = prepare() else {
        return 3;
    };
    let ssid = wlan::current_ssid();
    let ipv4 = active.ipv4.first().cloned();
    let gw = active.gateway.clone();
    let (kind, _) = portal::detect(&cfg, ssid.as_deref(), ipv4.as_deref(), &gw, cfg.probe_timeout());
    if kind != NetworkKind::Dorm {
        eprintln!(
            "✗ 当前判别为 {}，公共无线没有可靠的退出端点，请到门户页面手动注销",
            kind.text()
        );
        return 3;
    }
    let ip = active.ipv4.first().cloned().unwrap_or_default();
    let out = portal::dorm::logout(&cfg, &creds, &ip, &active.mac, cfg.probe_timeout());
    if out.ok {
        println!("✓ 已注销: {}", out.message);
    } else {
        println!("✗ 注销失败: {}", out.summary());
    }
    if !out.raw_head.is_empty() {
        println!("响应片段: {}", out.raw_head.replace(['\r', '\n'], " "));
    }
    if out.ok {
        0
    } else {
        2
    }
}
