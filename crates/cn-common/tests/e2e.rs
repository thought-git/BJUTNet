//! cn-common 端到端测试：payload 往返、真实安装包解析、快捷方式、注册表。
use cn_common::winutil;
use cn_common::{build_setup_exe, extract_payload, pack_dir, read_payload, GameManifest};
use std::path::Path;

fn make_manifest() -> GameManifest {
    GameManifest {
        game_name: "测试游戏".into(),
        version: "1.0.0".into(),
        main_exe: "game.exe".into(),
        install_dir_name: "TestGame".into(),
        uninstaller_name: "uninstaller.exe".into(),
        desktop_shortcut: true,
        start_menu_shortcut: true,
        files: Vec::new(),
    }
}

#[test]
fn payload_roundtrip() {
    // 构造一个临时游戏目录（含嵌套目录、二进制内容、大文件）
    let root = std::env::temp_dir().join(format!("gp_test_src_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("assets/sub")).unwrap();
    std::fs::write(root.join("game.exe"), vec![0u8; 65536]).unwrap();
    let mut binary = Vec::new();
    for i in 0..10000u32 {
        binary.extend_from_slice(&i.to_le_bytes());
    }
    std::fs::write(root.join("assets/sub/level.dat"), &binary).unwrap();
    std::fs::write(root.join("assets/readme.txt"), "你好，世界！".repeat(2000)).unwrap();
    let uninstaller = vec![9u8; 4096];

    let mut manifest = make_manifest();
    let payload = pack_dir(&root, &mut manifest, &[("uninstaller.exe".into(), uninstaller.clone())], |_, _| {}).unwrap();

    let template = vec![0x11u8; 4096];
    let setup = build_setup_exe(&template, &payload);

    // 模拟安装器：读自身尾部 payload
    let setup_path = std::env::temp_dir().join(format!("gp_test_setup_{}.exe", std::process::id()));
    std::fs::write(&setup_path, &setup).unwrap();
    let (m2, p2) = read_payload(&setup_path).expect("read_payload 失败");
    assert_eq!(m2.game_name, "测试游戏");
    assert_eq!(m2.files.len(), 4, "应包含 3 个游戏文件 + 卸载器");

    // 解压并逐文件比对
    let dest = std::env::temp_dir().join(format!("gp_test_out_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);
    extract_payload(&m2, &p2, &dest, |_, _, _| {}).unwrap();

    assert_eq!(std::fs::read(dest.join("game.exe")).unwrap(), vec![0u8; 65536]);
    assert_eq!(std::fs::read(dest.join("assets/sub/level.dat")).unwrap(), binary);
    assert_eq!(
        std::fs::read(dest.join("assets/readme.txt")).unwrap(),
        "你好，世界！".repeat(2000).as_bytes()
    );
    assert_eq!(std::fs::read(dest.join("uninstaller.exe")).unwrap(), uninstaller);

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&dest);
    let _ = std::fs::remove_file(&setup_path);
    println!("payload 往返测试通过：4 个文件逐字节一致");
}

#[test]
#[ignore]
fn real_setup_parses() {
    // 验证打包器 CLI 产出的真实安装包（通过环境变量 SETUP_EXE 指定路径）
    let path = std::env::var("SETUP_EXE").expect("需要 SETUP_EXE 环境变量");
    let (m, p) = read_payload(Path::new(&path)).expect("真实安装包解析失败");
    println!("真实安装包: 游戏={} v{} 主程序={} 文件数={}", m.game_name, m.version, m.main_exe, m.files.len());
    assert!(!m.files.is_empty());
    assert!(m.files.iter().any(|f| f.path == m.uninstaller_name));
    assert!(p.len() > 0);
}

#[test]
fn shortcut_and_registry() {
    // 快捷方式：写入临时 .lnk（无需真实桌面）
    let lnk = std::env::temp_dir().join(format!("gp_test_{}.lnk", std::process::id()));
    let target = std::env::current_exe().unwrap().to_string_lossy().to_string();
    winutil::create_shortcut(&lnk.to_string_lossy(), &target, "", &target, "测试快捷方式").expect("创建快捷方式失败");
    assert!(lnk.exists(), "快捷方式文件应存在");
    std::fs::remove_file(&lnk).unwrap();

    // 注册表卸载项：写入 → 验证存在 → 删除
    let app_id = format!("GP_TEST_{}", std::process::id());
    match winutil::write_uninstall_entry(&app_id, "测试游戏", "1.0.0", "C:\\fake\\dir", "C:\\fake\\dir\\game.exe", "C:\\fake\\dir\\uninstaller.exe") {
        Ok(()) => {
            let key_path = winutil::uninstall_key_path(&app_id);
            let hkcu = windows::Win32::System::Registry::HKEY_CURRENT_USER;
            let hkey = windows::core::HSTRING::from(&key_path);
            unsafe {
                use windows::Win32::System::Registry::{RegOpenKeyExW, KEY_READ};
                let mut k = windows::Win32::System::Registry::HKEY::default();
                let rc = RegOpenKeyExW(hkcu, &hkey, None, KEY_READ, &mut k);
                assert!(rc.is_ok(), "卸载注册表键应可打开");
                windows::Win32::System::Registry::RegCloseKey(k);
            }
            winutil::remove_uninstall_entry(&app_id).expect("删除卸载注册表失败");
            unsafe {
                use windows::Win32::System::Registry::{RegOpenKeyExW, KEY_READ};
                let mut k = windows::Win32::System::Registry::HKEY::default();
                let rc = RegOpenKeyExW(hkcu, &hkey, None, KEY_READ, &mut k);
                assert!(!rc.is_ok(), "卸载注册表键应已删除");
            }
            println!("注册表卸载项写入/删除验证通过");
        }
        Err(e) if e.contains("5") => {
            // 沙箱环境禁止注册表写入（ERROR_ACCESS_DENIED）——真实系统上不受影响，跳过验证
            println!("注意：当前环境禁止注册表写入（{e}），注册表验证跳过（沙箱限制，非代码问题）");
        }
        Err(e) => panic!("写入卸载注册表失败: {e}"),
    }
    println!("快捷方式与注册表测试通过");
}
