//! 小工具：宽字符、错误码、时间、脱敏。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

/// UTF-8 → 以 NUL 结尾的 UTF-16（给 Win32 API 用）
pub fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// 以 NUL 结尾的 UTF-16 指针 → String（空指针返回空串）
///
/// # Safety
/// `ptr` 必须要么为空，要么指向一段以 NUL 结尾的合法 UTF-16。
pub unsafe fn from_wide_ptr(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    let slice = std::slice::from_raw_parts(ptr, len);
    String::from_utf16_lossy(slice)
}

/// 最近一次 Win32 错误码
pub fn last_error() -> u32 {
    std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(0) as u32
}

/// 本地时间，`2027-01-01 12:00:00.123` 形态
pub fn now_text() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

/// 本地日期，`20270101`
pub fn today_key() -> String {
    chrono::Local::now().format("%Y%m%d").to_string()
}

/// 计算机名（DPAPI 附加熵用；取不到就空）
pub fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

/// 日志脱敏：把 URL 里的密码参数替换成 `***`
pub fn mask_url(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find(|c| c == '?' || c == '&') {
        out.push_str(&rest[..=pos]);
        rest = &rest[pos + 1..];
        // 取 key=value 这一段
        let end = rest.find('&').unwrap_or(rest.len());
        let pair = &rest[..end];
        match pair.split_once('=') {
            Some((k, v)) if k.eq_ignore_ascii_case("upass") || k.eq_ignore_ascii_case("user_password") => {
                out.push_str(k);
                out.push_str("=***");
                let _ = v;
            }
            _ => out.push_str(pair),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_url_hides_password() {
        let u = "http://h/eportal/portal/login?login_method=1&user_account=1&user_password=Secret123&v=9";
        let m = mask_url(u);
        assert!(!m.contains("Secret123"), "明文密码泄漏: {m}");
        assert!(m.contains("user_password=***"));
        assert!(m.contains("login_method=1"));
    }
}
