//! 取当前 WLAN 的 SSID（走 `wlanapi.dll`，不解析 `netsh` 的中文输出）。
//!
//! 为什么不用 `netsh wlan show interfaces`：输出是本地化中文，格式随版本变，
//! 解析脆弱；`WlanQueryInterface` 拿的是结构化数据，且 **服务在 Session 0 也能调**。

use std::ffi::c_void;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::WiFi::{
    wlan_intf_opcode_current_connection, WlanCloseHandle, WlanEnumInterfaces, WlanFreeMemory,
    WlanOpenHandle, WlanQueryInterface, WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO_LIST,
};

const WLAN_CLIENT_VERSION: u32 = 2;
const ERROR_SUCCESS: u32 = 0;
const WLAN_INTERFACE_STATE_CONNECTED: i32 = 1;

/// 打开 wlanapi 句柄（拿不到通常说明"无线服务未运行"或没有无线网卡）
pub fn open() -> Option<HANDLE> {
    unsafe {
        let mut handle = HANDLE::default();
        let mut negotiated: u32 = 0;
        let rc = WlanOpenHandle(WLAN_CLIENT_VERSION, None, &mut negotiated, &mut handle);
        if rc != ERROR_SUCCESS {
            return None;
        }
        Some(handle)
    }
}

pub fn close(handle: HANDLE) {
    unsafe {
        let _ = WlanCloseHandle(handle, None);
    }
}

/// 当前连着的 SSID（未连接/没有无线网卡 → None）
pub fn current_ssid() -> Option<String> {
    let handle = open()?;
    let result = current_ssid_with(handle);
    close(handle);
    result
}

fn current_ssid_with(handle: HANDLE) -> Option<String> {
    unsafe {
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        let rc = WlanEnumInterfaces(handle, None, &mut list);
        if rc != ERROR_SUCCESS || list.is_null() {
            return None;
        }

        let mut found: Option<String> = None;
        let count = (*list).dwNumberOfItems as usize;
        let items = (*list).InterfaceInfo.as_ptr();

        for i in 0..count {
            let info = &*items.add(i);
            let mut data_size: u32 = 0;
            let mut data: *mut c_void = std::ptr::null_mut();
            let rc = WlanQueryInterface(
                handle,
                &info.InterfaceGuid,
                wlan_intf_opcode_current_connection,
                None,
                &mut data_size,
                &mut data,
                None,
            );
            if rc != ERROR_SUCCESS || data.is_null() {
                continue;
            }
            let attrs = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
            if attrs.isState.0 == WLAN_INTERFACE_STATE_CONNECTED {
                let ssid = &attrs.wlanAssociationAttributes.dot11Ssid;
                let len = (ssid.uSSIDLength as usize).min(ssid.ucSSID.len());
                if len > 0 {
                    found = Some(String::from_utf8_lossy(&ssid.ucSSID[..len]).to_string());
                }
            }
            WlanFreeMemory(data);
            if found.is_some() {
                break;
            }
        }

        WlanFreeMemory(list as *const c_void);
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssid_call_does_not_panic() {
        // 没连 WiFi 时返回 None，也算通过
        let ssid = current_ssid();
        println!("当前 SSID: {ssid:?}");
    }
}
