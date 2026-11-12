//! payload 归档：打包（压缩）与解包。
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use walkdir::WalkDir;

pub const MAGIC: &[u8; 4] = b"RGMP";
pub const FOOTER_MAGIC: &[u8; 8] = b"RGPLPAY1";
pub const FOOTER_LEN: usize = 8 + 8 + 8; // offset(8) + len(8) + magic(8)

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileEntry {
    pub path: String,       // 相对路径（正斜杠）
    pub size: u64,          // 原始大小
    pub comp_size: u64,     // 压缩后大小
    pub compressed: bool,   // 是否 deflate 压缩
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GameManifest {
    pub game_name: String,
    pub version: String,
    pub main_exe: String,
    pub install_dir_name: String,
    pub uninstaller_name: String,
    pub desktop_shortcut: bool,
    pub start_menu_shortcut: bool,
    pub files: Vec<FileEntry>,
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

fn inflate(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut dec = DeflateDecoder::new(data);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)?;
    Ok(out)
}

/// 打包一个目录，返回 (payload 字节, manifest)。
/// `extra` 为附加文件（如卸载器 exe）：(相对路径, 字节)。
/// `progress(done, total)`：done 为已处理文件数。
pub fn pack_dir(
    root: &Path,
    manifest: &mut GameManifest,
    extra: &[(String, Vec<u8>)],
    mut progress: impl FnMut(usize, usize),
) -> std::io::Result<Vec<u8>> {
    let mut items: Vec<(String, Vec<u8>)> = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let full = root.join(rel.replace('/', "\\"));
        let raw = std::fs::read(&full)?;
        items.push((rel, raw));
    }
    items.extend(extra.iter().cloned());
    items.sort_by(|a, b| a.0.cmp(&b.0));

    manifest.files.clear();
    let total = items.len();
    let mut blobs: Vec<Vec<u8>> = Vec::with_capacity(total);
    for (i, (rel, raw)) in items.iter().enumerate() {
        let comp = deflate(raw);
        let compressed = comp.len() < raw.len();
        let stored = if compressed { comp } else { raw.clone() };
        manifest.files.push(FileEntry {
            path: rel.clone(),
            size: raw.len() as u64,
            comp_size: stored.len() as u64,
            compressed,
        });
        blobs.push(stored);
        progress(i + 1, total);
    }

    let manifest_json = serde_json::to_vec(manifest)?;
    let mut payload: Vec<u8> = Vec::new();
    payload.extend_from_slice(MAGIC);
    payload.extend_from_slice(&(manifest_json.len() as u32).to_le_bytes());
    payload.extend_from_slice(&manifest_json);
    for b in blobs {
        payload.extend_from_slice(&b);
    }
    Ok(payload)
}

/// 从 setup exe 读取 payload，返回 (manifest, payload 数据)。
pub fn read_payload(exe_path: &Path) -> Option<(GameManifest, Vec<u8>)> {
    let data = fs::read(exe_path).ok()?;
    if data.len() < FOOTER_LEN + 8 {
        return None;
    }
    let tail = &data[data.len() - FOOTER_LEN..];
    if &tail[16..] != FOOTER_MAGIC {
        return None;
    }
    let offset = u64::from_le_bytes(tail[0..8].try_into().ok()?) as usize;
    let len = u64::from_le_bytes(tail[8..16].try_into().ok()?) as usize;
    if offset + len > data.len() {
        return None;
    }
    let payload = &data[offset..offset + len];
    if &payload[0..4] != MAGIC {
        return None;
    }
    let mlen = u32::from_le_bytes(payload[4..8].try_into().ok()?) as usize;
    let manifest: GameManifest = serde_json::from_slice(&payload[8..8 + mlen]).ok()?;
    Some((manifest, payload[8 + mlen..].to_vec()))
}

/// 解压 payload 到 dest。
/// `progress(done, total, current_path)`。
pub fn extract_payload(
    manifest: &GameManifest,
    payload: &[u8],
    dest: &Path,
    mut progress: impl FnMut(usize, usize, &str),
) -> std::io::Result<()> {
    let mut cursor = 0usize;
    let total = manifest.files.len();
    for (i, f) in manifest.files.iter().enumerate() {
        let comp = &payload[cursor..cursor + f.comp_size as usize];
        cursor += f.comp_size as usize;
        let bytes = if f.compressed {
            inflate(comp)?
        } else {
            comp.to_vec()
        };
        let rel = f.path.replace('/', "\\");
        let out = dest.join(&rel);
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&out, &bytes)?;
        progress(i + 1, total, &f.path);
    }
    Ok(())
}

/// 把 payload 追加到模板 exe，生成最终 setup exe。
pub fn build_setup_exe(template: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut out = template.to_vec();
    let offset = out.len() as u64;
    out.extend_from_slice(payload);
    let len = payload.len() as u64;
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(FOOTER_MAGIC);
    out
}
