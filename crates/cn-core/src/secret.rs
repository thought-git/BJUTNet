//! 凭证存储：`secrets.dat` = `CNSE1` + DPAPI 密文。
//!
//! 为什么用 DPAPI 而不是自己搓 AES：
//! - DPAPI 已经包含"加密 + 完整性校验 + 机器/账户绑定"，自己再套一层只会多一个要保管的密钥。
//! - 服务以 LocalSystem 运行，`CryptProtectData`（CurrentUser 作用域）加密后，
//!   **同机其他用户即使拿到文件也解不开**（他们没有 SYSTEM 的 master key）。
//!
//! 附加熵：`CampusNet.v1|<计算机名>`，把密文拷到别的机器上同样解不开。

use std::path::Path;

use serde::{Deserialize, Serialize};
use windows::core::w;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
};

const MAGIC: &[u8; 5] = b"CNSE1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    pub account: String,
    pub password: String,
    /// ISO8601 本地时间，便于界面显示"最后修改"
    pub updated_at: String,
}

impl Credentials {
    pub fn new(account: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            account: account.into(),
            password: password.into(),
            updated_at: chrono::Local::now().to_rfc3339(),
        }
    }

    /// 账号打码，用于日志（`250****23`）
    pub fn masked_account(&self) -> String {
        mask(&self.account)
    }
}

pub fn mask(account: &str) -> String {
    let chars: Vec<char> = account.chars().collect();
    if chars.len() <= 4 {
        return "*".repeat(chars.len());
    }
    let head: String = chars.iter().take(3).collect();
    let tail: String = chars.iter().skip(chars.len() - 2).collect();
    format!("{head}****{tail}")
}

pub fn exists(path: &Path) -> bool {
    path.is_file()
}

/// 保存（原子写）
pub fn save(path: &Path, creds: &Credentials) -> std::io::Result<()> {
    let mut plain = serde_json::to_vec(creds)
        .map_err(|e| std::io::Error::other(format!("凭证序列化失败: {e}")))?;
    let blob = protect(&plain)?;
    wipe(&mut plain);

    let mut out = Vec::with_capacity(MAGIC.len() + blob.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&blob);

    if let Some(dir) = path.parent() {
        crate::paths::ensure_dir(dir)?;
    }
    let tmp = crate::config::tmp_sibling(path);
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&out)?;
        f.sync_all()?;
    }
    crate::config::replace(&tmp, path)
}

/// 读取；文件不存在返回 `Ok(None)`
pub fn load(path: &Path) -> std::io::Result<Option<Credentials>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if bytes.len() <= MAGIC.len() || &bytes[..MAGIC.len()] != MAGIC {
        return Err(std::io::Error::other("secrets.dat 格式不对（magic 不匹配）"));
    }
    let mut plain = unprotect(&bytes[MAGIC.len()..])?;
    let creds = serde_json::from_slice::<Credentials>(&plain)
        .map_err(|e| std::io::Error::other(format!("凭证反序列化失败: {e}")));
    wipe(&mut plain);
    creds.map(Some)
}

fn entropy() -> Vec<u8> {
    format!("CampusNet.v1|{}", crate::util::hostname()).into_bytes()
}

fn protect(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let ent = entropy();
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let ent_blob = CRYPT_INTEGER_BLOB {
        cbData: ent.len() as u32,
        pbData: ent.as_ptr() as *mut u8,
    };
    let mut out = CRYPT_INTEGER_BLOB::default();

    unsafe {
        CryptProtectData(&input, w!("CampusNet"), Some(&ent_blob), None, None, 0, &mut out)
            .map_err(|e| std::io::Error::other(format!("CryptProtectData 失败: {e}")))?;
        let result = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as *mut core::ffi::c_void)));
        Ok(result)
    }
}

fn unprotect(blob: &[u8]) -> std::io::Result<Vec<u8>> {
    let ent = entropy();
    let input = CRYPT_INTEGER_BLOB {
        cbData: blob.len() as u32,
        pbData: blob.as_ptr() as *mut u8,
    };
    let ent_blob = CRYPT_INTEGER_BLOB {
        cbData: ent.len() as u32,
        pbData: ent.as_ptr() as *mut u8,
    };
    let mut out = CRYPT_INTEGER_BLOB::default();

    unsafe {
        CryptUnprotectData(&input, None, Some(&ent_blob), None, None, 0, &mut out).map_err(|e| {
            std::io::Error::other(format!(
                "CryptUnprotectData 失败（换机器/换服务账户/文件损坏都会这样）: {e}"
            ))
        })?;
        let result = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as *mut core::ffi::c_void)));
        Ok(result)
    }
}

/// 尽力清零一块内存（编译器可能优化掉普通写，所以用 write_volatile）
pub fn wipe(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        unsafe { std::ptr::write_volatile(b, 0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_account() {
        assert_eq!(mask("20250000"), "202****00");
        assert_eq!(mask("ab"), "**");
    }

    #[test]
    fn dpapi_roundtrip() {
        let dir = std::env::temp_dir().join("campusnet-test-secret");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("secrets.dat");
        let _ = std::fs::remove_file(&path);

        let creds = Credentials::new("20250000", "p@ss中文");
        save(&path, &creds).expect("save");

        // 文件里不该出现明文
        let raw = std::fs::read(&path).unwrap();
        assert!(
            !raw.windows(7).any(|w| w == b"p@ss"),
            "密码明文出现在密文文件里"
        );
        assert_eq!(&raw[..5], MAGIC);

        let back = load(&path).expect("load").expect("some");
        assert_eq!(back.account, "20250000");
        assert_eq!(back.password, "p@ss中文");

        let _ = std::fs::remove_file(&path);
    }
}
