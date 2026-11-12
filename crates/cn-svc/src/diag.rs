//! 诊断信息采集（`--probe` 与 GUI「关于/诊断」共用一份，避免两处漂移）。

use std::fmt::Write as _;

use cn_core::config::Config;
use cn_core::http;
use cn_core::net;
use cn_core::paths;
use cn_core::portal::{self, NetworkKind};
use cn_core::secret;
use cn_core::version;
use cn_core::wlan;

pub fn text() -> String {
    let mut s = String::new();

    let _ = writeln!(s, "===== BJUTNet 诊断 =====");
    let _ = writeln!(s, "版本     : {}", version::full());
    let _ = writeln!(s, "exe      : {}", paths::show(&paths::exe_path()));
    let _ = writeln!(s, "安装目录 : {}", paths::show(&paths::install_dir()));
    let _ = writeln!(s, "数据目录 : {}", paths::show(&paths::data_dir()));
    let _ = writeln!(
        s,
        "管理员   : {}",
        if net::is_elevated() { "是" } else { "否（普通用户）" }
    );
    let _ = writeln!(s);

    // ---- 配置 ----
    let (cfg, outcome) = Config::load_or_create(&paths::config_path());
    let _ = writeln!(s, "--- 配置 ---");
    let _ = writeln!(s, "文件     : {}", paths::show(&paths::config_path()));
    let _ = writeln!(s, "状态     : {}", outcome.text());
    let _ = writeln!(
        s,
        "账号     : {}",
        if cfg.account.is_empty() {
            "<未配置>".to_string()
        } else {
            secret::mask(&cfg.account)
        }
    );
    let _ = writeln!(s, "后缀兜底 : {}", cfg.account_suffix_fallback);
    let _ = writeln!(s, "宿舍门户 : {}", cfg.dorm.base);
    let _ = writeln!(s, "公共门户 : {}", cfg.public.base);
    let _ = writeln!(
        s,
        "节奏     : 在线 {}s / 离线 {}s，超时 {}s",
        cfg.online_interval().as_secs(),
        cfg.offline_interval().as_secs(),
        cfg.probe_timeout().as_secs()
    );
    let _ = writeln!(s);

    // ---- 凭据 ----
    let _ = writeln!(s, "--- 凭据 ---");
    let sp = paths::secret_path();
    let _ = writeln!(s, "文件     : {}", paths::show(&sp));
    if secret::exists(&sp) {
        match secret::load(&sp) {
            Ok(Some(c)) => {
                let _ = writeln!(
                    s,
                    "状态     : 可解密（{}，修改于 {}）",
                    c.masked_account(),
                    c.updated_at
                );
            }
            Ok(None) => {
                let _ = writeln!(s, "状态     : 文件存在但为空");
            }
            Err(e) => {
                let _ = writeln!(s, "状态     : 解密失败 —— {e}");
            }
        }
    } else {
        let _ = writeln!(s, "状态     : 尚未配置");
    }
    let _ = writeln!(s);

    // ---- 网卡 ----
    let _ = writeln!(s, "--- 网卡 ---");
    let adapters = net::list_adapters().unwrap_or_default();
    let active = net::active_adapter(&adapters);
    let active_index = active.as_ref().map(|a| a.if_index);
    for a in &adapters {
        let mark = if Some(a.if_index) == active_index {
            " ★"
        } else {
            "  "
        };
        let _ = writeln!(s, "{mark} {}", a.summary());
        if let (Some(ip), Some(gw)) = (a.ipv4_first(), a.gateway.parse().ok()) {
            let _ = writeln!(
                s,
                "    子网: prefix={} 同子网={}",
                a.prefix_len,
                net::same_subnet(ip, gw, a.prefix_len)
            );
        }
    }
    let _ = writeln!(
        s,
        "系统默认出口(GetBestInterfaceEx): {:?}",
        net::best_interface_index()
    );
    let _ = writeln!(
        s,
        "活跃出口 : {}",
        active
            .as_ref()
            .map(|a| a.summary())
            .unwrap_or_else(|| "<无>".into())
    );
    let _ = writeln!(
        s,
        "SSID     : {}",
        wlan::current_ssid().unwrap_or_else(|| "<未连接无线>".into())
    );
    let _ = writeln!(s);

    // ---- 出网 ----
    let timeout = cfg.probe_timeout();
    let _ = writeln!(s, "--- 出网探测（HTTP，不用 ping） ---");
    let targets = cfg.probe_targets();
    let refs: Vec<&str> = targets.iter().map(|x| x.as_str()).collect();
    let (verdict, reports) = http::internet_ok(&refs, timeout);
    for r in &reports {
        let _ = writeln!(s, "  {}", r.text());
    }
    let _ = writeln!(s, "结论     : {}", verdict.text());
    let _ = writeln!(s);

    // ---- 门户 ----
    let _ = writeln!(s, "--- 门户可达性（网络判别的核心信号） ---");
    let dorm_url = portal::dorm_probe_url(&cfg);
    let public_url = portal::public_probe_url(&cfg);
    for (label, url) in [("宿舍", &dorm_url), ("公共", &public_url)] {
        let r = http::get(url, timeout);
        let _ = writeln!(
            s,
            "  {label}: {} → {}",
            url,
            if r.status.is_some() {
                format!("可达 (HTTP {:?}, {} ms)", r.status, r.elapsed_ms)
            } else {
                format!("不可达 ({})", r.error)
            }
        );
    }
    let _ = writeln!(s);

    // ---- 判别 ----
    let _ = writeln!(s, "--- 用户标记的校园网 ---");
    if cfg.trusted_networks.is_empty() {
        let _ = writeln!(s, "（无）");
    } else {
        for t in &cfg.trusted_networks {
            let _ = writeln!(s, "  · {}", t.describe());
        }
    }
    let _ = writeln!(s, "--- 网络判别（用户标记 > 门户指纹 > 网段兜底） ---");
    let ssid = wlan::current_ssid();
    let ipv4 = active.as_ref().and_then(|a| a.ipv4.first().cloned());
    let gw = active.as_ref().map(|a| a.gateway.clone()).unwrap_or_default();
    let (kind, ev) = portal::detect(&cfg, ssid.as_deref(), ipv4.as_deref(), &gw, timeout);
    let _ = writeln!(s, "结果     : {}", kind.text());
    let _ = writeln!(s, "依据     : {}", ev.text());
    for n in &ev.notes {
        let _ = writeln!(s, "  · {n}");
    }
    if kind == NetworkKind::Dorm {
        match portal::dorm::load_config(&cfg, ipv4.as_deref().unwrap_or(""), timeout) {
            Some(pc) => {
                let _ = writeln!(
                    s,
                    "宿舍配置 : login_method={} account_suffix='{}' find_mac={} en_md5={}",
                    pc.login_method, pc.account_suffix, pc.find_mac, pc.en_md5
                );
                let _ = writeln!(
                    s,
                    "实际后缀 : '{}'（loadConfig 优先，失败才用配置兜底）",
                    portal::dorm::resolve_suffix(&cfg, Some(&pc))
                );
            }
            None => {
                let _ = writeln!(s, "宿舍配置 : 读取失败（登录时用配置兜底后缀）");
            }
        }
        match portal::dorm::online_list(&cfg, timeout) {
            Some(list) if list.is_empty() => {
                let _ = writeln!(s, "门户在线会话: 无");
            }
            Some(list) => {
                for x in &list {
                    let _ = writeln!(
                        s,
                        "门户在线会话: account={} ip={} 上线于 {} 已在线 {}s",
                        x.account, x.ip, x.online_time, x.time_long_sec
                    );
                }
            }
            None => {
                let _ = writeln!(s, "门户在线会话: 查询失败");
            }
        }
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "===== 诊断结束 =====");
    s
}
