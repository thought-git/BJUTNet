//! Windows 工具：快捷方式、注册表卸载项、已知文件夹、文件夹选择对话框、消息框。
use windows::core::{Interface, HSTRING, PWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    IPersistFile,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_WRITE, REG_DWORD, REG_OPEN_CREATE_OPTIONS, REG_SZ,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Programs, IFileOpenDialog, IShellItem, IShellLinkW,
    SHGetKnownFolderPath, SIGDN_FILESYSPATH, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS,
    FileOpenDialog, KNOWN_FOLDER_FLAG, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_ICONQUESTION, MB_YESNO, MESSAGEBOX_STYLE,
};

/// 初始化 COM（STA）。重复调用安全（CoInitializeEx 幂等返回）。
pub fn com_init() -> Result<(), String> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|e| format!("COM 初始化失败: {e}"))
    }
}

/// 已知文件夹路径（如桌面、开始菜单 Programs）。
pub fn known_folder(id: &windows::core::GUID) -> Option<String> {
    unsafe {
        let path = SHGetKnownFolderPath(id as *const windows::core::GUID, KNOWN_FOLDER_FLAG(0), None).ok()?;
        if path.is_null() {
            return None;
        }
        let s = path.to_string().ok();
        CoTaskMemFree(Some(path.as_ptr() as *const _));
        s
    }
}

pub fn desktop_dir() -> Option<String> {
    known_folder(&FOLDERID_Desktop)
}

/// 开始菜单“程序”文件夹（%APPDATA%\Microsoft\Windows\Start Menu\Programs）。
pub fn start_menu_programs_dir() -> Option<String> {
    known_folder(&FOLDERID_Programs)
}

/// 创建 .lnk 快捷方式。
pub fn create_shortcut(lnk_path: &str, target: &str, args: &str, icon: &str, desc: &str) -> Result<(), String> {
    com_init()?;
    unsafe {
        let shell_link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| format!("创建 ShellLink 失败: {e}"))?;
        shell_link
            .SetPath(&HSTRING::from(target))
            .map_err(|e| format!("SetPath 失败: {e}"))?;
        if !args.is_empty() {
            shell_link
                .SetArguments(&HSTRING::from(args))
                .map_err(|e| format!("SetArguments 失败: {e}"))?;
        }
        if !icon.is_empty() {
            shell_link
                .SetIconLocation(&HSTRING::from(icon), 0)
                .map_err(|e| format!("SetIconLocation 失败: {e}"))?;
        }
        if !desc.is_empty() {
            shell_link
                .SetDescription(&HSTRING::from(desc))
                .map_err(|e| format!("SetDescription 失败: {e}"))?;
        }
        let persist: IPersistFile = shell_link.cast().map_err(|e| format!("IPersistFile 失败: {e}"))?;
        persist
            .Save(&HSTRING::from(lnk_path), true)
            .map_err(|e| format!("保存快捷方式失败: {e}"))?;
    }
    Ok(())
}

/// 文件夹选择对话框（原生）。
pub fn pick_folder(title: &str) -> Option<String> {
    com_init().ok()?;
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM;
        dialog.SetOptions(opts).ok()?;
        dialog.SetTitle(&HSTRING::from(title)).ok()?;
        dialog.Show(None).ok()?;
        let item: IShellItem = dialog.GetResult().ok()?;
        let name: PWSTR = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = name.to_string().ok();
        CoTaskMemFree(Some(name.as_ptr() as *const _));
        s
    }
}

/// 注册表卸载项路径前缀。
pub fn uninstall_key_path(app_id: &str) -> String {
    format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{app_id}")
}

fn reg_sz_bytes(value: &str) -> Vec<u8> {
    let mut v: Vec<u8> = value.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    v.extend_from_slice(&[0, 0]);
    v
}

pub fn write_uninstall_entry(
    app_id: &str,
    display_name: &str,
    version: &str,
    install_dir: &str,
    display_icon: &str,
    uninstaller_path: &str,
) -> Result<(), String> {
    unsafe {
        let path = HSTRING::from(uninstall_key_path(app_id));
        let mut key: HKEY = HKEY::default();
        let rc = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &path,
            None,
            None,
            REG_OPEN_CREATE_OPTIONS(0),
            KEY_WRITE,
            None,
            &mut key,
            None,
        );
        if !rc.is_ok() {
            return Err(format!("创建注册表键失败: {}", rc.0));
        }

        let set = |name: &str, value: &str| -> Result<(), String> {
            let data = reg_sz_bytes(value);
            let rc = RegSetValueExW(key, &HSTRING::from(name), None, REG_SZ, Some(&data));
            if rc.is_ok() {
                Ok(())
            } else {
                Err(format!("写注册表值 {name} 失败: {}", rc.0))
            }
        };

        set("DisplayName", display_name)?;
        set("DisplayVersion", version)?;
        set("Publisher", "RG Packer")?;
        set("InstallLocation", install_dir)?;
        set("DisplayIcon", display_icon)?;
        set("UninstallString", uninstaller_path)?;
        // REG_DWORD 值：NoModify / NoRepair
        for n in ["NoModify", "NoRepair"] {
            let data = 1u32.to_le_bytes();
            let rc = RegSetValueExW(key, &HSTRING::from(n), None, REG_DWORD, Some(&data));
            if !rc.is_ok() {
                return Err(format!("写注册表值 {n} 失败: {}", rc.0));
            }
        }
        let _ = RegCloseKey(key);
    }
    Ok(())
}

pub fn remove_uninstall_entry(app_id: &str) -> Result<(), String> {
    unsafe {
        let path = HSTRING::from(uninstall_key_path(app_id));
        let rc = RegDeleteKeyW(HKEY_CURRENT_USER, &path);
        if rc.is_ok() {
            Ok(())
        } else {
            Err(format!("删除注册表键失败: {}", rc.0))
        }
    }
}

/// 确认对话框（是/否）。
pub fn confirm(text: &str, title: &str) -> bool {
    unsafe {
        let r = MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from(title),
            MESSAGEBOX_STYLE(MB_YESNO.0 | MB_ICONQUESTION.0),
        );
        r == IDYES
    }
}

/// 信息对话框。
pub fn info(text: &str, title: &str) {
    unsafe {
        let _ = MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from(title),
            MESSAGEBOX_STYLE(0x40), // MB_ICONINFORMATION
        );
    }
}
