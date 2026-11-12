//! 给 exe 嵌入图标与版本信息（用 Windows SDK 的 rc.exe，经 embed-resource 调用）。
//!
//! 图标源：`assets/icon.ico`（仓库内，保证可重复构建）。
//! 版本号：`CAMPUSNET_VERSION`（bundle.sh 注入）→ 回落 crate 版本。

fn main() {
    // 版本号是构建时注入的：声明后，bundle.sh 改版本会触发重编（否则复用旧缓存）
    println!("cargo:rerun-if-env-changed=CAMPUSNET_VERSION");
    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest_dir.join("../../assets/icon.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    if !icon.is_file() {
        println!("cargo:warning=assets/icon.ico 不存在，跳过图标嵌入");
        return;
    }

    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc_path = out_dir.join("app.rc");

    let ver = std::env::var("CAMPUSNET_VERSION")
        .or_else(|_| std::env::var("CARGO_PKG_VERSION"))
        .unwrap_or_else(|_| "0.0.0".to_string());
    let mut it = ver.split('.').map(|x| x.parse::<u32>().unwrap_or(0));
    let (a, b, c) = (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    );

    let pkg = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
    let (desc, exe) = match pkg.as_str() {
        "cn-svc" => ("BJUTNet 校园网自动登录服务", "BJUTNetSvc.exe"),
        "cn-gui" => ("BJUTNet 校园网自动登录面板", "BJUTNet.exe"),
        "cn-installer" => ("BJUTNet 安装程序", "BJUTNetSetup.exe"),
        "cn-uninstaller" => ("BJUTNet 卸载程序", "BJUTNetUninstall.exe"),
        _ => ("BJUTNet", "BJUTNet.exe"),
    };

    // rc.exe 接受正斜杠路径
    let ico = icon.display().to_string().replace('\\', "/");
    let text = format!(
        r#"1 ICON "{ico}"

1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "BJUTNet"
      VALUE "FileDescription", "{desc}"
      VALUE "FileVersion", "{ver}"
      VALUE "ProductName", "BJUTNet 校园网自动登录"
      VALUE "ProductVersion", "{ver}"
      VALUE "OriginalFilename", "{exe}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    // 注意：rc.exe 对**无 BOM 的 UTF-8** 会按 ANSI 读，中文描述会把引号解析错
    // （RC2133: unexpected value in value data）。所以写成带 BOM 的 UTF-16LE。
    let mut bytes = vec![0xFFu8, 0xFE];
    for u in text.encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    if let Err(e) = std::fs::write(&rc_path, bytes) {
        println!("cargo:warning=写 rc 失败: {e}");
        return;
    }

    let res = embed_resource::compile(&rc_path, embed_resource::NONE);
    if let Err(e) = res.clone().manifest_optional() {
        println!("cargo:warning=图标/版本资源编译失败: {e}");
    }
}
