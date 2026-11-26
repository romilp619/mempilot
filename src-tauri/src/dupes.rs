//! Exact-duplicate file finder.
//!
//! Files are grouped by size, then by a hash of their first 64 KB, then by a hash of the whole
//! file. Before anything is removed, each copy is compared byte for byte with the copy that is
//! kept, and removal goes to the Recycle Bin, so a mistake can always be undone.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;
use windows::core::PCWSTR;
use windows::Win32::UI::Shell::{SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW};
use xxhash_rust::xxh3::Xxh3;

pub static CANCEL: AtomicBool = AtomicBool::new(false);

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Filter {
    All,
    Images,
    Videos,
    Music,
    Documents,
    Archives,
}

const IMAGES: &[&str] = &["jpg", "jpeg", "png", "gif", "bmp", "webp", "heic", "heif", "tif", "tiff", "raw", "cr2", "nef", "arw", "dng", "psd", "svg", "ico"];
const VIDEOS: &[&str] = &["mp4", "mkv", "mov", "avi", "wmv", "webm", "m4v", "flv", "3gp", "ts"];
const MUSIC: &[&str] = &["mp3", "wav", "flac", "aac", "m4a", "ogg", "wma", "opus"];
const DOCS: &[&str] = &["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "rtf", "odt", "csv", "epub", "md"];
const ARCHIVES: &[&str] = &["zip", "rar", "7z", "tar", "gz", "iso", "exe", "msi", "apk"];

fn matches(filter: Filter, p: &Path) -> bool {
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let list = match filter {
        Filter::All => return true,
        Filter::Images => IMAGES,
        Filter::Videos => VIDEOS,
        Filter::Music => MUSIC,
        Filter::Documents => DOCS,
        Filter::Archives => ARCHIVES,
    };
    list.contains(&ext.as_str())
}

/// Folders never scanned even when they sit inside a chosen folder: app data and dependency
/// trees where "duplicates" are normal and required.
fn skip_dir(name: &str) -> bool {
    let n = name.to_lowercase();
    matches!(n.as_str(), "node_modules" | ".git" | "appdata" | "$recycle.bin" | "system volume information" | "windows" | "program files" | "program files (x86)" | "programdata" | ".cargo" | ".rustup" | ".gradle" | ".nuget" | "site-packages" | "venv" | ".venv" | "target")
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DupFile {
    pub path: String,
    pub modified: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DupGroup {
    pub size: u64,
    pub files: Vec<DupFile>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub phase: String,
    pub done: u64,
    pub total: u64,
}

fn collect(dir: &Path, filter: Filter, min: u64, depth: u32, out: &mut Vec<(PathBuf, u64, u64)>) {
    if depth > 48 || CANCEL.load(Ordering::Relaxed) {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Ok(md) = e.metadata() else { continue };
        let a = md.file_attributes();
        // reparse points (links, OneDrive placeholders), hidden and system files are skipped
        if a & (0x400 | 0x4 | 0x2) != 0 {
            continue;
        }
        let p = e.path();
        if a & 0x10 != 0 {
            if !skip_dir(&e.file_name().to_string_lossy()) {
                collect(&p, filter, min, depth + 1, out);
            }
            continue;
        }
        // FILE_ATTRIBUTE_OFFLINE / RECALL_ON_DATA_ACCESS: cloud-only files would be downloaded
        if a & (0x1000 | 0x40_0000) != 0 {
            continue;
        }
        if md.len() >= min && matches(filter, &p) {
            let m = md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0);
            out.push((p, md.len(), m));
        }
    }
}

fn hash(p: &Path, limit: Option<u64>) -> Option<u128> {
    let mut f = File::open(p).ok()?;
    let mut h = Xxh3::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut left = limit.unwrap_or(u64::MAX);
    while left > 0 {
        if CANCEL.load(Ordering::Relaxed) {
            return None;
        }
        let want = buf.len().min(left as usize);
        let n = f.read(&mut buf[..want]).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        left -= n as u64;
    }
    Some(h.digest128())
}

pub fn scan(roots: &[String], filter: Filter, min_bytes: u64, progress: &dyn Fn(Progress)) -> Vec<DupGroup> {
    CANCEL.store(false, Ordering::SeqCst);
    let mut files = Vec::new();
    for r in roots {
        progress(Progress { phase: format!("Listing {r}"), done: files.len() as u64, total: 0 });
        collect(Path::new(r), filter, min_bytes.max(1), 0, &mut files);
    }
    // de-duplicate paths when chosen folders overlap
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files.dedup_by(|a, b| a.0 == b.0);

    let mut by_size: HashMap<u64, Vec<(PathBuf, u64)>> = HashMap::new();
    for (p, size, m) in files {
        by_size.entry(size).or_default().push((p, m));
    }
    let candidates: Vec<(u64, Vec<(PathBuf, u64)>)> = by_size.into_iter().filter(|(_, v)| v.len() > 1).collect();
    let total: u64 = candidates.iter().map(|(_, v)| v.len() as u64).sum();
    let mut done = 0u64;
    let mut groups = Vec::new();
    for (size, list) in candidates {
        if CANCEL.load(Ordering::Relaxed) {
            break;
        }
        let mut by_head: HashMap<u128, Vec<(PathBuf, u64)>> = HashMap::new();
        for (p, m) in list {
            done += 1;
            if done % 25 == 0 {
                progress(Progress { phase: "Comparing files".into(), done, total });
            }
            if let Some(h) = hash(&p, Some(64 * 1024)) {
                by_head.entry(h).or_default().push((p, m));
            }
        }
        for (_, same_head) in by_head.into_iter().filter(|(_, v)| v.len() > 1) {
            let mut by_full: HashMap<u128, Vec<(PathBuf, u64)>> = HashMap::new();
            for (p, m) in same_head {
                let h = if size <= 64 * 1024 { Some(0) } else { hash(&p, None) };
                if let Some(h) = h {
                    by_full.entry(h).or_default().push((p, m));
                }
            }
            for (_, mut same) in by_full.into_iter().filter(|(_, v)| v.len() > 1) {
                // Oldest first (the UI keeps the first one); on a tie prefer the shorter name, so
                // "photo.jpg" is kept over "photo (1).jpg" or "photo - Copy.jpg".
                same.sort_by_key(|(p, m)| (*m, p.file_name().map(|f| f.len()).unwrap_or(0), p.as_os_str().len()));
                groups.push(DupGroup { size, files: same.into_iter().map(|(p, m)| DupFile { path: p.to_string_lossy().into(), modified: m }).collect() });
            }
        }
    }
    groups.sort_by(|a, b| (b.size * (b.files.len() as u64 - 1)).cmp(&(a.size * (a.files.len() as u64 - 1))));
    progress(Progress { phase: "Done".into(), done: total, total });
    groups
}

fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(mut fa), Ok(mut fb)) = (File::open(a), File::open(b)) else { return false };
    match (fa.metadata(), fb.metadata()) {
        (Ok(ma), Ok(mb)) if ma.len() == mb.len() => {}
        _ => return false,
    }
    let mut ba = vec![0u8; 1 << 20];
    let mut bb = vec![0u8; 1 << 20];
    loop {
        let Ok(na) = fa.read(&mut ba) else { return false };
        if na == 0 {
            return true;
        }
        if fb.read_exact(&mut bb[..na]).is_err() || ba[..na] != bb[..na] {
            return false;
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveRequest {
    pub keep: String,
    pub remove: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveResult {
    pub recycled: u64,
    pub bytes: u64,
    pub refused: Vec<String>,
}

/// Sends verified duplicates to the Recycle Bin.
pub fn remove(reqs: &[RemoveRequest]) -> RemoveResult {
    let mut ok_paths: Vec<(String, u64)> = Vec::new();
    let mut refused = Vec::new();
    for r in reqs {
        let keep = Path::new(&r.keep);
        if !keep.is_file() {
            refused.extend(r.remove.iter().cloned());
            continue;
        }
        for p in &r.remove {
            if p != &r.keep && same_bytes(keep, Path::new(p)) {
                let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                ok_paths.push((p.clone(), len));
            } else {
                refused.push(p.clone());
            }
        }
    }
    let mut recycled = 0;
    let mut bytes = 0;
    for chunk in ok_paths.chunks(200) {
        let mut from: Vec<u16> = Vec::new();
        for (p, _) in chunk {
            from.extend(p.encode_utf16());
            from.push(0);
        }
        from.push(0);
        let mut op = SHFILEOPSTRUCTW {
            wFunc: FO_DELETE,
            pFrom: PCWSTR(from.as_ptr()),
            fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16,
            ..Default::default()
        };
        let _ = unsafe { SHFileOperationW(&mut op) };
        for (p, len) in chunk {
            if Path::new(p).exists() {
                refused.push(p.clone());
            } else {
                recycled += 1;
                bytes += len;
            }
        }
    }
    RemoveResult { recycled, bytes, refused }
}
