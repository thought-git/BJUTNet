//! 网卡枚举与"当前活跃出口"判定。
//!
//! 与旧方案的区别（旧方案的坑）：
//! - 不按"网卡速度打分"，而是按 **默认路由 + 接口度量** 选出口；
//! - 虚拟网卡黑名单补齐了 `array networks` / `tap-windows` / `wintun` / `fortinet` 等
//!   （本机就装着一个 Array Networks SSL VPN，旧清单里没有）。

use std::ffi::c_void;
use std::net::Ipv4Addr;

use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS, HANDLE};
use windows::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, GetBestInterfaceEx, GAA_FLAG_INCLUDE_ALL_INTERFACES,
    GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_MULTICAST,
    IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::Networking::WinSock::{
    AF_INET, ADDRESS_FAMILY, SOCKADDR, SOCKADDR_IN,
};

use crate::util;

/// 虚拟网卡识别关键字（小写匹配 Description / FriendlyName）
pub const VIRTUAL_PATTERNS: &[&str] = &[
    "vmware",
    "virtualbox",
    "hyper-v",
    "vethernet",
    "loopback",
    "tap-windows",
    "tap-",
    "tun",
    "wintun",
    "npcap",
    "winpcap",
    "wan miniport",
    "bluetooth",
    "radmin",
    "zerotier",
    "tailscale",
    "wireguard",
    "openvpn",
    "anyconnect",
    "sangfor",
    "array networks",
    "fortinet",
    "pulse secure",
    "juniper",
    "sonicwall",
    "softether",
    "acceleration",
    "加速器",
];

const IF_TYPE_ETHERNET_CSMACD: u32 = 6;
const IF_TYPE_IEEE80211: u32 = 71;

#[derive(Debug, Clone)]
pub struct Adapter {
    pub if_index: u32,
    /// 网络连接名（"以太网 3"）
    pub name: String,
    /// 网卡描述
    pub desc: String,
    /// MAC（大写冒号分隔，空表示取不到）
    pub mac: String,
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    /// 默认网关（取第一个 IPv4 网关）
    pub gateway: String,
    /// 连接是否 Up
    pub up: bool,
    pub if_type: u32,
    /// 注册表/系统里的 IPv4 接口度量（越小越优先）
    pub ipv4_metric: u32,
    pub ipv6_metric: u32,
    /// 首个 IPv4 的前缀长度（子网判定用）
    pub prefix_len: u8,
    pub is_virtual: bool,
    pub virtual_reason: String,
    pub is_wireless: bool,
}

impl Adapter {
    pub fn is_ethernet(&self) -> bool {
        self.if_type == IF_TYPE_ETHERNET_CSMACD
    }

    /// 该网卡是否"看起来是可用出口"：Up + 有 IPv4 + 有网关 + 不是虚拟网卡
    pub fn usable(&self) -> bool {
        self.up && !self.ipv4.is_empty() && !self.gateway.is_empty() && !self.is_virtual
    }

    pub fn ipv4_first(&self) -> Option<Ipv4Addr> {
        self.ipv4.first().and_then(|s| s.parse().ok())
    }

    pub fn summary(&self) -> String {
        format!(
            "{}（{}） ipv4={} gw={} mac={} up={} metric={}{}",
            self.name,
            self.desc,
            if self.ipv4.is_empty() {
                "-".to_string()
            } else {
                self.ipv4.join(",")
            },
            if self.gateway.is_empty() { "-" } else { &self.gateway },
            if self.mac.is_empty() { "-" } else { &self.mac },
            self.up,
            self.ipv4_metric,
            if self.is_virtual {
                format!(" [虚拟: {}]", self.virtual_reason)
            } else {
                String::new()
            }
        )
    }
}

fn is_virtual(name: &str, desc: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    let d = desc.to_ascii_lowercase();
    for p in VIRTUAL_PATTERNS {
        if n.contains(p) || d.contains(p) {
            return Some(p);
        }
    }
    None
}

/// NDIS 轻量过滤层 / QoS 调度器 / 虚拟交换机扩展：挂在真网卡下面的"伪网卡"，
/// 每张真网卡都会派生好几张，混进列表里只会干扰判断。
pub fn is_filter_layer(name: &str, desc: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let d = desc.to_ascii_lowercase();
    n.ends_with("-0000")
        || d.contains("lightweight filter")
        || d.contains("qos packet scheduler")
        || d.contains("virtual switch extension filter")
        || d.contains("native wifi filter")
}

/// 占位伪网卡：没有独立出口（WAN Miniport、内核调试、Teredo/6to4/IP-HTTPS）
pub fn is_placeholder(desc: &str) -> bool {
    let d = desc.to_ascii_lowercase();
    d.contains("wan miniport")
        || d.contains("kernel debug")
        || d.contains("teredo")
        || d.contains("6to4")
        || d.contains("ip-https")
        || d.contains("remote ndis")
}

fn sockaddr_family(p: *const SOCKADDR) -> ADDRESS_FAMILY {
    if p.is_null() {
        return ADDRESS_FAMILY(0);
    }
    unsafe { (*p).sa_family }
}

fn sockaddr_ipv4(p: *const SOCKADDR) -> Option<Ipv4Addr> {
    if p.is_null() {
        return None;
    }
    unsafe {
        if (*p).sa_family != AF_INET {
            return None;
        }
        let sin = &*(p as *const SOCKADDR_IN);
        let raw = sin.sin_addr.S_un.S_addr;
        // S_addr 是网络字节序（大端）原样存进 u32
        Some(Ipv4Addr::from(u32::from_be(raw)))
    }
}

/// 枚举所有网卡（不过滤虚拟网卡，调用方自己判断）
fn enumerate_adapters() -> std::io::Result<Vec<Adapter>> {
    let flags = GAA_FLAG_INCLUDE_ALL_INTERFACES
        | GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_MULTICAST
        | GAA_FLAG_INCLUDE_GATEWAYS;

    let mut size: u32 = 0;
    unsafe {
        let rc = GetAdaptersAddresses(0 /* AF_UNSPEC */, flags, None, None, &mut size);
        if rc != ERROR_BUFFER_OVERFLOW.0 && rc != ERROR_SUCCESS.0 {
            return Err(std::io::Error::other(format!(
                "GetAdaptersAddresses(探测大小) 失败: rc={rc} 错误码={}",
                util::last_error()
            )));
        }
    }
    if size == 0 {
        return Ok(Vec::new());
    }

    // 多给一点余量：枚举期间网卡可能变化
    let mut buf = vec![0u8; size as usize + 4096];
    let head: *mut IP_ADAPTER_ADDRESSES_LH;
    unsafe {
        let rc = GetAdaptersAddresses(
            0,
            flags,
            None,
            Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH),
            &mut size,
        );
        if rc != ERROR_SUCCESS.0 {
            return Err(std::io::Error::other(format!(
                "GetAdaptersAddresses 失败: rc={rc} 错误码={}",
                util::last_error()
            )));
        }
        head = buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
    }

    let mut out = Vec::new();
    let mut cur = head;
    while !cur.is_null() {
        unsafe {
            let a = &*cur;

            let name = util::from_wide_ptr(a.FriendlyName.0);
            let desc = util::from_wide_ptr(a.Description.0);

            let mac_len = (a.PhysicalAddressLength as usize).min(8);
            let mac = if mac_len == 0 {
                String::new()
            } else {
                a.PhysicalAddress[..mac_len]
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(":")
            };

            let mut ipv4 = Vec::new();
            let mut ipv6 = Vec::new();
            let mut prefix_len = 0u8;
            let mut u = a.FirstUnicastAddress;
            while !u.is_null() {
                let ua = &*u;
                match sockaddr_family(ua.Address.lpSockaddr) {
                    f if f == AF_INET => {
                        if let Some(ip) = sockaddr_ipv4(ua.Address.lpSockaddr) {
                            if ipv4.is_empty() {
                                prefix_len = ua.OnLinkPrefixLength;
                            }
                            ipv4.push(ip.to_string());
                        }
                    }
                    f if f.0 == 23 => {
                        // AF_INET6：只记一个，用于日志/诊断
                        if ipv6.is_empty() {
                            ipv6.push("(ipv6)".to_string());
                        }
                    }
                    _ => {}
                }
                u = ua.Next;
            }

            let mut gateway = String::new();
            let mut g = a.FirstGatewayAddress;
            while !g.is_null() {
                let ga = &*g;
                if let Some(ip) = sockaddr_ipv4(ga.Address.lpSockaddr) {
                    gateway = ip.to_string();
                    break;
                }
                g = ga.Next;
            }

            let v = is_virtual(&name, &desc);
            out.push(Adapter {
                if_index: a.Anonymous1.Anonymous.IfIndex,
                name,
                desc,
                mac,
                ipv4,
                ipv6,
                gateway,
                up: a.OperStatus.0 == 1, // IfOperStatusUp
                if_type: a.IfType,
                ipv4_metric: a.Ipv4Metric,
                ipv6_metric: a.Ipv6Metric,
                prefix_len,
                is_virtual: v.is_some(),
                virtual_reason: v.unwrap_or("").to_string(),
                is_wireless: a.IfType == IF_TYPE_IEEE80211,
            });

            cur = a.Next;
        }
    }
    // 过滤掉 NDIS 过滤层与占位伪网卡（一张真网卡会派生出好几张伪网卡）
    out.retain(|a| !is_filter_layer(&a.name, &a.desc) && !is_placeholder(&a.desc));
    Ok(out)
}

/// 真实网卡（已过滤 NDIS 过滤层 / WAN Miniport 等伪网卡），按接口度量排序
pub fn list_adapters() -> std::io::Result<Vec<Adapter>> {
    let mut list = enumerate_adapters()?;
    list.sort_by_key(|a| a.ipv4_metric);
    Ok(list)
}

/// 全部适配器（含过滤层/伪网卡），只给深度诊断用
pub fn list_all_adapters() -> std::io::Result<Vec<Adapter>> {
    enumerate_adapters()
}

/// 系统认为"去往 8.8.8.8 该走哪张网卡"（等价于默认路由）
pub fn best_interface_index() -> Option<u32> {
    unsafe {
        let mut sin = SOCKADDR_IN::default();
        sin.sin_family = AF_INET;
        sin.sin_port = 0;
        sin.sin_addr.S_un.S_addr = u32::from_ne_bytes([8, 8, 8, 8]);
        let mut idx: u32 = 0;
        let rc = GetBestInterfaceEx(&sin as *const _ as *const SOCKADDR, &mut idx);
        if rc == ERROR_SUCCESS.0 {
            Some(idx)
        } else {
            None
        }
    }
}

/// 当前活跃出口：优先用系统默认路由；若它指向虚拟网卡（VPN 抢了默认路由），
/// 退化为"可用网卡里度量最小的一台"。
pub fn active_adapter(adapters: &[Adapter]) -> Option<Adapter> {
    if let Some(idx) = best_interface_index() {
        if let Some(a) = adapters.iter().find(|a| a.if_index == idx && !a.is_virtual) {
            return Some(a.clone());
        }
    }
    adapters
        .iter()
        .filter(|a| a.usable())
        .min_by_key(|a| a.ipv4_metric)
        .cloned()
}

/// 是否以管理员/系统身份运行（服务里恒为 true）
pub fn is_elevated() -> bool {
    #[repr(C)]
    struct TokenElevation {
        token_is_elevated: u32,
    }
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_ELEVATION_CLASS: u32 = 20;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> HANDLE;
        fn CloseHandle(h: HANDLE) -> i32;
    }
    #[link(name = "advapi32")]
    extern "system" {
        fn OpenProcessToken(process: HANDLE, access: u32, token: *mut HANDLE) -> i32;
        fn GetTokenInformation(
            token: HANDLE,
            class: u32,
            info: *mut c_void,
            len: u32,
            ret_len: *mut u32,
        ) -> i32;
    }

    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elev = TokenElevation { token_is_elevated: 0 };
        let mut ret: u32 = 0;
        let ok = GetTokenInformation(
            token,
            TOKEN_ELEVATION_CLASS,
            &mut elev as *mut _ as *mut c_void,
            std::mem::size_of::<TokenElevation>() as u32,
            &mut ret,
        );
        let _ = CloseHandle(token);
        ok != 0 && elev.token_is_elevated != 0
    }
}

/// 判断 IP 是否属于同一子网（给 M2 的"IP 与网关不同子网"检测用）
pub fn same_subnet(ip: Ipv4Addr, gw: Ipv4Addr, prefix: u8) -> bool {
    if prefix == 0 || prefix > 32 {
        return false;
    }
    let mask: u32 = if prefix == 32 {
        u32::MAX
    } else {
        u32::MAX << (32 - prefix)
    };
    (u32::from(ip) & mask) == (u32::from(gw) & mask)
}


/// `ip` 是否落在 `cidr`（形如 `10.126.64.0/19`，不带 `/` 视为 /32）
pub fn ip_in_cidr(ip: &str, cidr: &str) -> bool {
    let (base, len) = match cidr.split_once('/') {
        Some((b, l)) => (b.trim(), l.trim().parse::<u8>().ok().unwrap_or(32)),
        None => (cidr.trim(), 32),
    };
    let (Ok(ip), Ok(base)) = (ip.trim().parse::<Ipv4Addr>(), base.parse::<Ipv4Addr>()) else {
        return false;
    };
    let prefix = len.min(32);
    let mask: u32 = if prefix == 0 {
        0
    } else if prefix == 32 {
        u32::MAX
    } else {
        u32::MAX << (32 - prefix)
    };
    (u32::from(ip) & mask) == (u32::from(base) & mask)
}

/// 由 `ip/prefix` 算出网络地址字符串，如 `10.126.83.10` + 19 → `10.126.64.0/19`
pub fn subnet_of(ip: &str, prefix: u8) -> String {
    let Ok(ip) = ip.trim().parse::<Ipv4Addr>() else {
        return String::new();
    };
    let p = prefix.min(32);
    let mask: u32 = if p == 0 {
        0
    } else if p == 32 {
        u32::MAX
    } else {
        u32::MAX << (32 - p)
    };
    let net = Ipv4Addr::from(u32::from(ip) & mask);
    format!("{net}/{p}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subnet_math() {
        let ip: Ipv4Addr = "10.126.83.10".parse().unwrap();
        let gw: Ipv4Addr = "10.126.95.254".parse().unwrap();
        assert!(same_subnet(ip, gw, 19));
        assert!(!same_subnet(ip, gw, 24));
        assert_eq!(subnet_of("10.126.83.10", 19), "10.126.64.0/19");
        assert!(ip_in_cidr("10.126.83.10", "10.126.64.0/19"));
        assert!(!ip_in_cidr("10.20.1.2", "10.126.64.0/19"));
        assert!(ip_in_cidr("10.20.1.2", "10.20.1.0/24"));
        // 不带前缀 = 精确匹配（/32）
        assert!(!ip_in_cidr("10.20.1.2", "10.20.0.0"));
        assert!(ip_in_cidr("10.20.1.2", "10.20.1.2"));
    }

    #[test]
    fn enumerate_does_not_panic() {
        // 只要求不 panic、能列出东西（CI 环境可能为空）
        let _ = list_adapters().expect("list_adapters");
    }
}
