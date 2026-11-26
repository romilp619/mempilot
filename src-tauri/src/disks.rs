//! Read-only disk information. Nothing in this module writes to a disk or changes a volume.

use serde::Serialize;
use std::os::windows::process::CommandExt;
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDriveStringsW, GetVolumeInformationW};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Volume {
    pub root: String,
    pub label: String,
    pub fs: String,
    pub kind: String,
    pub total: u64,
    pub free: u64,
}

pub fn volumes() -> Vec<Volume> {
    let mut buf = [0u16; 512];
    let n = unsafe { GetLogicalDriveStringsW(Some(&mut buf)) } as usize;
    let mut out = Vec::new();
    for root in buf[..n.min(buf.len())].split(|&c| c == 0).filter(|s| !s.is_empty()) {
        let mut r: Vec<u16> = root.to_vec();
        r.push(0);
        let p = PCWSTR(r.as_ptr());
        let kind = match unsafe { GetDriveTypeW(p) } {
            2 => "Removable",
            3 => "Fixed",
            4 => "Network",
            5 => "Optical",
            6 => "RAM disk",
            _ => continue,
        };
        let (mut free, mut total) = (0u64, 0u64);
        if unsafe { GetDiskFreeSpaceExW(p, None, Some(&mut total), Some(&mut free)) }.is_err() {
            continue; // empty card reader, disconnected share, ...
        }
        let mut label = [0u16; 261];
        let mut fs = [0u16; 261];
        let _ = unsafe { GetVolumeInformationW(p, Some(&mut label), None, None, None, Some(&mut fs)) };
        let s = |w: &[u16]| {
            let e = w.iter().position(|&c| c == 0).unwrap_or(w.len());
            String::from_utf16_lossy(&w[..e])
        };
        out.push(Volume { root: String::from_utf16_lossy(root), label: s(&label), fs: s(&fs), kind: kind.into(), total, free });
    }
    out
}

/// Physical disk health as Windows' Storage subsystem reports it (Get-PhysicalDisk).
pub fn physical_disks() -> Result<serde_json::Value, String> {
    let script = "Get-PhysicalDisk | Select-Object FriendlyName,MediaType,BusType,HealthStatus,OperationalStatus,Size | ConvertTo-Json -Compress";
    let out = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(text.trim()).map_err(|_| "could not read disk health".to_string())?;
    Ok(match v {
        serde_json::Value::Array(_) => v,
        other => serde_json::Value::Array(vec![other]),
    })
}
