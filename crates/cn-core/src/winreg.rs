//! 注册表：HKLM 卸载项（64 位视图）+ 读取。
//!
//! 用 HKLM 而不是 HKCU：装到 `Program Files` 是"全机安装"，卸载项应该出现在
//! 所有用户的"应用和功能"里，而不是只有当年那个管理员账号。

use windows::core::HSTRING;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE,
    KEY_WOW64_64KEY, KEY_WRITE, REG_DWORD, REG_OPEN_CREATE_OPTIONS, REG_SAM_FLAGS, REG_SZ,
};

const KEY_WRITE_64: REG_SAM_FLAGS = REG_SAM_FLAGS(KEY_WRITE.0 | KEY_WOW64_64KEY.0);

fn sz_bytes(v: &str) -> Vec<u8> {
    let mut b: Vec<u8> = v.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    b.extend_from_slice(&[0, 0]);
    b
}

pub fn uninstall_path(app_id: &str) -> String {
    format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{app_id}")
}

/// 写 HKLM 卸载项（需要管理员）
pub fn write_uninstall(
    app_id: &str,
    display_name: &str,
    version: &str,
    publisher: &str,
    install_dir: &str,
    display_icon: &str,
    uninstall_cmd: &str,
) -> Result<(), String> {
    let path = HSTRING::from(uninstall_path(app_id));
    let mut key = HKEY::default();
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            &path,
            None,
            None,
            REG_OPEN_CREATE_OPTIONS(0),
            KEY_WRITE_64,
            None,
            &mut key,
            None,
        )
    };
    if !rc.is_ok() {
        return Err(format!("创建卸载注册表项失败: {}", rc.0));
    }

    let set_sz = |name: &str, value: &str| -> Result<(), String> {
        let data = sz_bytes(value);
        let rc = unsafe {
            RegSetValueExW(
                key,
                &HSTRING::from(name),
                None,
                REG_SZ,
                Some(&data),
            )
        };
        if rc.is_ok() {
            Ok(())
        } else {
            Err(format!("写注册表值 {name} 失败: {}", rc.0))
        }
    };

    let mut result = Ok(());
    for (name, value) in [
        ("DisplayName", display_name),
        ("DisplayVersion", version),
        ("Publisher", publisher),
        ("InstallLocation", install_dir),
        ("DisplayIcon", display_icon),
        ("UninstallString", uninstall_cmd),
        ("InstallDate", ""),
    ] {
        if value.is_empty() {
            continue;
        }
        if let Err(e) = set_sz(name, value) {
            result = Err(e);
            break;
        }
    }

    if result.is_ok() {
        // NoModify / NoRepair = 1
        for name in ["NoModify", "NoRepair"] {
            let data = 1u32.to_le_bytes();
            let rc = unsafe {
                RegSetValueExW(
                    key,
                    &HSTRING::from(name),
                    None,
                    REG_DWORD,
                    Some(&data),
                )
            };
            if !rc.is_ok() {
                result = Err(format!("写注册表值 {name} 失败: {}", rc.0));
                break;
            }
        }
    }

    unsafe {
        let _ = RegCloseKey(key);
    }
    result
}

/// 删 HKLM 卸载项（不存在也算成功）
pub fn remove_uninstall(app_id: &str) -> Result<(), String> {
    let path = HSTRING::from(uninstall_path(app_id));
    let rc = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, &path) };
    if rc.is_ok() {
        Ok(())
    } else {
        // 2 = 文件/键不存在
        let code = rc.0;
        if code == 2 {
            Ok(())
        } else {
            Err(format!("删除卸载注册表项失败: {}", code))
        }
    }
}
