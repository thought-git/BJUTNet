//! `config.json` 模型。
//!
//! 原则：
//! - **非敏感项才放这里**（账号名可以放，密码只在 secrets.dat）。
//! - 读失败不能拖死服务：文件缺失 → 写一份默认值；JSON 坏了 → 备份成 `.bad` 后回落默认值。
//! - 写文件走 "临时文件 + 原子替换"，避免断电写坏。
//! - 未知字段忽略（方便以后加字段、也兼容旧版本文件）。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::logging::Level;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DormCfg {
    /// 宿舍门户根地址（不含 `/eportal/portal`）
    pub base: String,
    /// 是否用 loadConfig 探测 login_method / account_suffix
    pub load_config: bool,
    /// loadConfig 拿不到后缀时的兜底（实测本环境是 @campus）
    pub fallback_suffix: String,
}

impl Default for DormCfg {
    fn default() -> Self {
        Self {
            base: "http://10.21.221.98:801".into(),
            load_config: true,
            fallback_suffix: "@campus".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PublicCfg {
    /// 公共无线门户根地址（HTTPS，标准端口）
    pub base: String,
    /// Referer 路径（实测浏览器登录带 /a79.htm）
    pub referer_path: String,
    /// 域名式门户优先用标准端口
    pub prefer_standard_port: bool,
}

impl Default for PublicCfg {
    fn default() -> Self {
        Self {
            base: "https://wlgn.bjut.edu.cn".into(),
            referer_path: "/a79.htm".into(),
            prefer_standard_port: true,
        }
    }
}


/// 用户手动标记的"校园网"（面板里"把当前网络标记为校园网"写这里）。
///
/// 三个字段都是"命中任一即可"：SSID / 网关 / 网段。这样无论是
/// 换了一台路由器（网关变）、还是同网段换了 IP，都能认出来。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct TrustedNet {
    /// "dorm"（宿舍有线）/ "public"（公共无线）
    pub kind: String,
    /// 备注（面板里展示用，默认自动生成）
    pub name: String,
    /// 无线名（有线时为空）
    pub ssid: String,
    /// 网关地址
    pub gateway: String,
    /// 形如 `10.126.64.0/19`；空 = 不按网段
    pub subnet: String,
}

impl TrustedNet {
    pub fn is_public(&self) -> bool {
        self.kind.eq_ignore_ascii_case("public")
    }

    /// 命中任一条件即算同一张网络
    pub fn matches(&self, ssid: Option<&str>, ip: Option<&str>, gateway: &str) -> bool {
        if !self.ssid.is_empty() {
            if let Some(s) = ssid {
                if s.eq_ignore_ascii_case(&self.ssid) {
                    return true;
                }
            }
        }
        if !self.gateway.is_empty() && !gateway.is_empty() && gateway.eq_ignore_ascii_case(&self.gateway) {
            return true;
        }
        if !self.subnet.is_empty() {
            if let Some(i) = ip {
                if crate::net::ip_in_cidr(i, &self.subnet) {
                    return true;
                }
            }
        }
        false
    }

    pub fn describe(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        let kind = if self.is_public() { "公共无线" } else { "宿舍有线" };
        let mut parts = vec![kind.to_string()];
        if !self.ssid.is_empty() {
            parts.push(format!("SSID={}", self.ssid));
        }
        if !self.gateway.is_empty() {
            parts.push(format!("网关={}", self.gateway));
        }
        if !self.subnet.is_empty() {
            parts.push(format!("网段={}", self.subnet));
        }
        parts.join(" ")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub version: u32,

    /// 学号（用于界面展示；登录时以 secrets.dat 为准）
    pub account: String,
    /// 账号后缀兜底（loadConfig 拿不到时用）
    pub account_suffix_fallback: String,

    /// 在线时的探测间隔
    pub check_interval_sec: u64,
    /// 离线/待登录时的探测间隔
    pub offline_interval_sec: u64,
    /// 单次 HTTP 探测超时
    pub probe_timeout_sec: u64,
    /// 自定义在线探测地址（留空 = 用内置的两个 generate_204）
    pub probe_urls: Vec<String>,
    /// 失败退避秒数
    pub backoff: Vec<u64>,
    /// 连续多少次"账号密码类"失败后暂停
    pub max_password_fails: u32,
    /// 暂停多少分钟
    pub pause_minutes: u64,

    /// 是否允许服务自动去连 bjut_wifi（默认关：避免把宿舍有线踢掉）
    pub auto_connect_wifi: bool,
    pub wifi_profile: String,

    /// ERR / WARN / INFO / DEBUG
    pub log_level: String,

    pub dorm: DormCfg,
    pub public: PublicCfg,

    /// 用户手动标记为校园网的网络（面板里加）
    pub trusted_networks: Vec<TrustedNet>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            account: String::new(),
            account_suffix_fallback: "@campus".into(),
            check_interval_sec: 5,
            offline_interval_sec: 2,
            probe_timeout_sec: 3,
            probe_urls: Vec::new(),
            backoff: vec![2, 4, 8, 15, 30, 60],
            max_password_fails: 3,
            pause_minutes: 10,
            auto_connect_wifi: false,
            wifi_profile: "bjut_wifi".into(),
            log_level: "INFO".into(),
            dorm: DormCfg::default(),
            public: PublicCfg::default(),
            trusted_networks: Vec::new(),
        }
    }
}

impl Config {
    pub fn level(&self) -> Level {
        Level::parse(&self.log_level)
    }

    /// 在线时的轮询间隔 = **掉线发现延迟**
    ///
    /// 掉线本身不会触发任何系统事件（网卡还在、IP 没变、门户会话没了），
    /// 所以只能靠 HTTP 探测发现。默认 5 s（下限 3 s，防止配置写 0 把 CPU 打满）。
    pub fn online_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.check_interval_sec.clamp(3, 3600))
    }

    /// 离线时的轮询间隔
    pub fn offline_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.offline_interval_sec.max(2))
    }

    pub fn probe_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.probe_timeout_sec.clamp(1, 30))
    }

    /// 当前网络是否被用户标记过（返回命中的那条）
    pub fn trusted_match(&self, ssid: Option<&str>, ip: Option<&str>, gateway: &str) -> Option<&TrustedNet> {
        self.trusted_networks
            .iter()
            .find(|t| t.matches(ssid, ip, gateway))
    }

    /// 实际使用的在线探测地址（配置留空就用内置两个）
    pub fn probe_targets(&self) -> Vec<String> {
        if self.probe_urls.is_empty() {
            crate::http::PROBE_URLS
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            self.probe_urls.clone()
        }
    }

    /// 只读：文件不存在或解析失败返回 None（GUI 用，避免面板顺手写文件）
    pub fn load_existing(path: &Path) -> Option<Config> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
    }

    /// 读配置；文件不存在时写一份默认值。
    pub fn load_or_create(path: &Path) -> (Config, LoadOutcome) {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                // 容忍 BOM
                let text = text.trim_start_matches('\u{feff}');
                match serde_json::from_str::<Config>(text) {
                    Ok(cfg) => (cfg, LoadOutcome::Loaded),
                    Err(e) => {
                        let bad = path.with_extension("json.bad");
                        let _ = std::fs::rename(path, &bad);
                        crate::logging::warn(format!(
                            "config.json 解析失败（{e}），已备份为 {}，本次使用默认配置",
                            bad.display()
                        ));
                        let cfg = Config::default();
                        let _ = cfg.save(path);
                        (cfg, LoadOutcome::Recovered)
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Config::default();
                let _ = cfg.save(path);
                (cfg, LoadOutcome::Created)
            }
            Err(e) => {
                crate::logging::warn(format!(
                    "config.json 读不动（{e}），使用默认配置（不会覆盖原文件）"
                ));
                (Config::default(), LoadOutcome::ReadFailed)
            }
        }
    }

    /// 原子保存：先写 `.tmp` 再替换
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            crate::paths::ensure_dir(dir)?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::other(format!("序列化配置失败: {e}")))?;
        let tmp = tmp_sibling(path);
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.write_all(b"\r\n")?;
            f.sync_all()?;
        }
        replace(&tmp, path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadOutcome {
    Loaded,
    Created,
    Recovered,
    ReadFailed,
}

impl LoadOutcome {
    pub fn text(self) -> &'static str {
        match self {
            LoadOutcome::Loaded => "已加载",
            LoadOutcome::Created => "文件不存在，已生成默认配置",
            LoadOutcome::Recovered => "文件损坏，已备份并重置",
            LoadOutcome::ReadFailed => "读取失败，使用内存默认值",
        }
    }
}

pub fn tmp_sibling(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".tmp");
    PathBuf::from(s)
}

/// 原子替换（同目录 rename）
pub fn replace(tmp: &Path, dst: &Path) -> std::io::Result<()> {
    if dst.exists() {
        let _ = std::fs::remove_file(dst);
    }
    std::fs::rename(tmp, dst)
}
