//! Temporary file, cache and package-cache cleaner.
//!
//! Safety rules, applied to every category:
//! - only the folders listed below are ever touched, never a path the UI sends;
//! - junctions and symbolic links are never followed, so a link inside a temp folder cannot
//!   lead the cleaner somewhere else on the disk;
//! - files changed in the last `min_age_hours` are left alone (something may still use them);
//! - files that are open or locked are skipped, not forced;
//! - browser and app caches are skipped while that browser or app is running.

use serde::Serialize;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use windows::core::PCWSTR;
use windows::Win32::UI::Shell::{SHEmptyRecycleBinW, SHQueryRecycleBinW, SHQUERYRBINFO};

const REPARSE_POINT: u32 = 0x400;
const DIRECTORY: u32 = 0x10;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Files,
    RecycleBin,
}

struct Category {
    id: &'static str,
    group: &'static str,
    name: &'static str,
    desc: &'static str,
    default_on: bool,
    needs_admin: bool,
    min_age_hours: u64,
    kind: Kind,
    /// Only files whose name starts with this (e.g. "thumbcache_").
    prefix: Option<&'static str>,
    /// Skip while this process is running (lower-case exe name).
    blocked_by: Option<&'static str>,
    roots: Vec<PathBuf>,
}

fn env(k: &str) -> PathBuf {
    std::env::var_os(k).map(PathBuf::from).unwrap_or_default()
}

/// `User Data\<every profile>\<sub>` for Chromium browsers.
fn chromium(base: PathBuf) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&base) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        for sub in ["Cache", "Code Cache", "GPUCache", "DawnGraphiteCache", "DawnWebGPUCache"] {
            let c = p.join(sub);
            if c.is_dir() {
                out.push(c);
            }
        }
    }
    out
}

fn firefox() -> Vec<PathBuf> {
    let base = env("LOCALAPPDATA").join(r"Mozilla\Firefox\Profiles");
    std::fs::read_dir(base)
        .map(|rd| rd.flatten().map(|e| e.path().join("cache2")).filter(|p| p.is_dir()).collect())
        .unwrap_or_default()
}

fn categories() -> Vec<Category> {
    let local = env("LOCALAPPDATA");
    let roaming = env("APPDATA");
    let home = env("USERPROFILE");
    let win = env("SystemRoot");
    let pdata = env("ProgramData");
    let c = |id, group, name, desc, default_on, needs_admin, min_age_hours, blocked_by, roots| Category {
        id,
        group,
        name,
        desc,
        default_on,
        needs_admin,
        min_age_hours,
        kind: Kind::Files,
        prefix: None,
        blocked_by,
        roots,
    };
    let mut v = vec![
        c("userTemp", "Windows", "Your temporary files", "Leftovers from installers and apps in %TEMP%. Files from the last 24 hours are kept.", true, false, 24, None, vec![env("TEMP")]),
        c("winTemp", "Windows", "Windows temporary files", "C:\\Windows\\Temp. Files from the last 24 hours are kept.", true, true, 24, None, vec![win.join("Temp")]),
        c("crashDumps", "Windows", "Crash dumps and error reports", "Memory dumps and reports saved after app or system crashes.", true, false, 0, None,
          vec![local.join("CrashDumps"), win.join("Minidump"), pdata.join(r"Microsoft\Windows\WER\ReportArchive"), pdata.join(r"Microsoft\Windows\WER\ReportQueue"), local.join(r"Microsoft\Windows\WER\ReportArchive"), local.join(r"Microsoft\Windows\WER\ReportQueue")]),
        c("deliveryOpt", "Windows", "Delivery Optimization cache", "Update pieces Windows keeps to share with other PCs.", true, true, 0, None,
          vec![win.join(r"ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization\Cache")]),
        c("winUpdate", "Windows", "Downloaded Windows updates", "Already-installed update downloads. A pending update would download again.", false, true, 72, None,
          vec![win.join(r"SoftwareDistribution\Download")]),
        Category { prefix: Some("thumbcache_"), ..c("thumbnails", "Windows", "Thumbnail cache", "Explorer's picture previews. Rebuilt as you browse folders.", false, false, 0, None, vec![local.join(r"Microsoft\Windows\Explorer")]) },
        c("shaderCache", "Windows", "GPU shader caches", "DirectX and NVIDIA shader caches. Games stutter once while they rebuild them.", false, false, 0, None,
          vec![local.join("D3DSCache"), local.join(r"NVIDIA\DXCache"), local.join(r"NVIDIA\GLCache"), local.join(r"AMD\DxCache")]),
        Category { kind: Kind::RecycleBin, ..c("recycleBin", "Windows", "Recycle Bin", "Permanently removes what is in the Recycle Bin.", false, false, 0, None, vec![]) },
        c("chrome", "Browsers", "Chrome cache", "Cached pages and scripts. Logins, history and passwords are not touched.", true, false, 0, Some("chrome.exe"), chromium(local.join(r"Google\Chrome\User Data"))),
        c("edge", "Browsers", "Edge cache", "Cached pages and scripts. Logins, history and passwords are not touched.", true, false, 0, Some("msedge.exe"), chromium(local.join(r"Microsoft\Edge\User Data"))),
        c("brave", "Browsers", "Brave cache", "Cached pages and scripts. Logins, history and passwords are not touched.", true, false, 0, Some("brave.exe"), chromium(local.join(r"BraveSoftware\Brave-Browser\User Data"))),
        c("firefox", "Browsers", "Firefox cache", "Cached pages and scripts. Logins, history and passwords are not touched.", true, false, 0, Some("firefox.exe"), firefox()),
        c("discord", "Apps", "Discord cache", "Cached images and app code.", true, false, 0, Some("discord.exe"),
          ["Cache", "Code Cache", "GPUCache"].iter().map(|s| roaming.join("discord").join(s)).collect()),
        c("vscode", "Apps", "VS Code cache", "Cached app code and data.", true, false, 0, Some("code.exe"),
          ["Cache", "CachedData", "Code Cache", "GPUCache"].iter().map(|s| roaming.join("Code").join(s)).collect()),
        c("npm", "Developer packages", "npm cache", "Downloaded npm packages. Re-downloaded on the next install.", false, false, 0, Some("node.exe"), vec![local.join("npm-cache")]),
        c("pip", "Developer packages", "pip cache", "Downloaded Python packages. Re-downloaded on the next install.", false, false, 0, None, vec![local.join(r"pip\Cache")]),
        c("yarn", "Developer packages", "Yarn cache", "Downloaded Yarn packages.", false, false, 0, Some("node.exe"), vec![local.join(r"Yarn\Cache")]),
        c("nuget", "Developer packages", "NuGet download cache", "HTTP and v3 caches only; the packages folder your builds use is kept.", false, false, 0, None,
          vec![local.join(r"NuGet\v3-cache"), local.join(r"NuGet\http-cache"), local.join(r"NuGet\plugins-cache")]),
        c("gradle", "Developer packages", "Gradle caches", "Android/Java build downloads. The next build downloads them again (can be large).", false, false, 0, Some("java.exe"), vec![home.join(r".gradle\caches")]),
        c("cargo", "Developer packages", "Cargo download cache", "Downloaded .crate archives; extracted sources are kept.", false, false, 0, Some("cargo.exe"), vec![home.join(r".cargo\registry\cache")]),
        c("go", "Developer packages", "Go build cache", "Compiled Go packages. Rebuilt on the next build.", false, false, 0, Some("go.exe"), vec![local.join("go-build")]),
    ];
    for cat in &mut v {
        cat.roots.retain(|r| !r.as_os_str().is_empty() && r.is_absolute());
    }
    v
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryScan {
    pub id: &'static str,
    pub group: &'static str,
    pub name: &'static str,
    pub desc: &'static str,
    pub default_on: bool,
    pub needs_admin: bool,
    pub bytes: u64,
    pub files: u64,
    pub blocked: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanResult {
    pub id: &'static str,
    pub name: &'static str,
    pub freed: u64,
    pub deleted: u64,
    pub skipped: u64,
    pub note: Option<String>,
}

fn is_reparse(attrs: u32) -> bool {
    attrs & REPARSE_POINT != 0
}

/// Walks `dir` without following links and calls `f` for every eligible file.
fn walk(dir: &Path, cutoff: Option<SystemTime>, prefix: Option<&str>, depth: u32, f: &mut dyn FnMut(&Path, u64)) {
    if depth > 64 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Ok(md) = e.metadata() else { continue }; // DirEntry::metadata does not follow links on Windows
        let attrs = md.file_attributes();
        if is_reparse(attrs) {
            continue;
        }
        let p = e.path();
        if attrs & DIRECTORY != 0 {
            if prefix.is_none() {
                walk(&p, cutoff, prefix, depth + 1, f);
            }
            continue;
        }
        if let Some(pre) = prefix {
            if !e.file_name().to_string_lossy().to_lowercase().starts_with(pre) {
                continue;
            }
        }
        if let (Some(cut), Ok(m)) = (cutoff, md.modified()) {
            if m > cut {
                continue;
            }
        }
        f(&p, md.len());
    }
}

/// Removes empty folders below `root` (never `root` itself), bottom-up, without following links.
fn prune_empty(dir: &Path, depth: u32) {
    if depth > 64 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Ok(md) = e.metadata() else { continue };
        if md.file_attributes() & DIRECTORY != 0 && !is_reparse(md.file_attributes()) {
            let p = e.path();
            prune_empty(&p, depth + 1);
            let _ = std::fs::remove_dir(&p); // fails harmlessly unless empty
        }
    }
}

fn running_set() -> std::collections::HashSet<String> {
    let mut s = std::collections::HashSet::new();
    crate::mem::for_each_process(|_, name| {
        s.insert(name.to_lowercase());
    });
    s
}

fn cutoff(c: &Category) -> Option<SystemTime> {
    (c.min_age_hours > 0).then(|| SystemTime::now() - Duration::from_secs(c.min_age_hours * 3600))
}

fn recycle_bin_size() -> (u64, u64) {
    let mut info = SHQUERYRBINFO { cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
    if unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut info) }.is_ok() {
        (info.i64Size as u64, info.i64NumItems as u64)
    } else {
        (0, 0)
    }
}

pub fn scan() -> Vec<CategoryScan> {
    let running = running_set();
    categories()
        .into_iter()
        .map(|c| {
            let (mut bytes, mut files) = (0u64, 0u64);
            if c.kind == Kind::RecycleBin {
                (bytes, files) = recycle_bin_size();
            } else {
                let cut = cutoff(&c);
                for r in &c.roots {
                    walk(r, cut, c.prefix, 0, &mut |_, len| {
                        bytes += len;
                        files += 1;
                    });
                }
            }
            let blocked = c.blocked_by.filter(|b| running.contains(*b)).map(|b| format!("Close {b} to clean this"));
            CategoryScan { id: c.id, group: c.group, name: c.name, desc: c.desc, default_on: c.default_on, needs_admin: c.needs_admin, bytes, files, blocked }
        })
        .collect()
}

pub fn clean(ids: &[String]) -> Vec<CleanResult> {
    let running = running_set();
    let mut out = Vec::new();
    for c in categories().into_iter().filter(|c| ids.iter().any(|i| i == c.id)) {
        if let Some(b) = c.blocked_by.filter(|b| running.contains(*b)) {
            out.push(CleanResult { id: c.id, name: c.name, freed: 0, deleted: 0, skipped: 0, note: Some(format!("skipped: {b} is running")) });
            continue;
        }
        if c.kind == Kind::RecycleBin {
            let (bytes, n) = recycle_bin_size();
            // SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND
            let ok = unsafe { SHEmptyRecycleBinW(None, PCWSTR::null(), 0x1 | 0x2 | 0x4) }.is_ok();
            out.push(CleanResult { id: c.id, name: c.name, freed: if ok { bytes } else { 0 }, deleted: if ok { n } else { 0 }, skipped: 0, note: (!ok && n > 0).then(|| "Windows refused".into()) });
            continue;
        }
        let (mut freed, mut deleted, mut skipped) = (0u64, 0u64, 0u64);
        let cut = cutoff(&c);
        for r in &c.roots {
            walk(r, cut, c.prefix, 0, &mut |p, len| {
                if std::fs::remove_file(p).is_ok() {
                    freed += len;
                    deleted += 1;
                } else {
                    skipped += 1;
                }
            });
            if c.prefix.is_none() {
                prune_empty(r, 0);
            }
        }
        let note = (skipped > 0).then(|| {
            if c.needs_admin && deleted == 0 {
                "needs administrator".to_string()
            } else {
                format!("{skipped} in use or protected, left alone")
            }
        });
        out.push(CleanResult { id: c.id, name: c.name, freed, deleted, skipped, note });
    }
    out
}

/// Windows component store ("WinSxS") clean-up of superseded update packages through DISM.
/// The same task Windows runs on its own schedule; /ResetBase is NOT used, so installed updates
/// can still be uninstalled.
pub fn component_cleanup(analyze_only: bool) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    let args: &[&str] = if analyze_only {
        &["/Online", "/Cleanup-Image", "/AnalyzeComponentStore"]
    } else {
        &["/Online", "/Cleanup-Image", "/StartComponentCleanup"]
    };
    let out = std::process::Command::new(env("SystemRoot").join(r"System32\Dism.exe"))
        .args(args)
        .args(["/English"])
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        let line = text.lines().rev().find(|l| l.contains("Error") || l.contains("elevat")).unwrap_or("DISM failed");
        return Err(if text.contains("elevat") { "needs administrator".into() } else { line.trim().to_string() });
    }
    if analyze_only {
        let pick = |key: &str| text.lines().find(|l| l.contains(key)).and_then(|l| l.split(':').nth(1)).map(|s| s.trim().to_string()).unwrap_or_default();
        Ok(format!(
            "Component store {}; shared with Windows {}; reclaimable packages {}; cleanup recommended: {}",
            pick("Actual Size of Component Store"),
            pick("Shared with Windows"),
            pick("Number of Reclaimable Packages"),
            pick("Component Store Cleanup Recommended")
        ))
    } else {
        Ok("Superseded update packages removed".into())
    }
}
