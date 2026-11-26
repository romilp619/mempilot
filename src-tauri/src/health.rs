//! Laptop health: battery wear, SSD wear, GPU use per app, hardware error log.
//! Everything here reads; the only write is the per-app GPU preference, which is the same
//! registry value Windows Settings > System > Display > Graphics writes.

use serde::Serialize;
use std::collections::HashMap;
use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W,
    PDH_FMT_DOUBLE, PDH_FMT_LARGE,
};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Registry::{RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::System::IO::DeviceIoControl;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn powershell(script: &str) -> Option<serde_json::Value> {
    let out = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).ok()
}

// ---------------------------------------------------------------- power / battery

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Power {
    pub on_ac: bool,
    pub has_battery: bool,
    pub percent: Option<u8>,
    pub saver: bool,
}

pub fn power() -> Power {
    let mut s = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut s) }.is_err() {
        return Power::default();
    }
    Power {
        on_ac: s.ACLineStatus == 1,
        has_battery: s.BatteryFlag != 128 && s.BatteryFlag != 255,
        percent: (s.BatteryLifePercent <= 100).then_some(s.BatteryLifePercent),
        saver: s.SystemStatusFlag == 1,
    }
}

/// Battery report, charging state, TRIM, power plan, WHEA errors and (admin only) SSD
/// reliability counters, in one PowerShell run.
pub fn system_report() -> serde_json::Value {
    let script = r#"
$ErrorActionPreference='SilentlyContinue'
$r=@{}
$f="$env:TEMP\mempilot-battery.xml"
powercfg /batteryreport /xml /output $f | Out-Null
if (Test-Path $f) { [xml]$x=Get-Content $f; $r.batteries=@($x.BatteryReport.Batteries.Battery | % { @{ id=$_.Id; manufacturer=$_.Manufacturer; chemistry=$_.Chemistry; design=[int64]$_.DesignCapacity; full=[int64]$_.FullChargeCapacity; cycles=[int64]$_.CycleCount } }); Remove-Item $f }
$s=Get-CimInstance -Namespace root/wmi -ClassName BatteryStatus | Select -First 1
if ($s) { $r.status=@{ powerOnline=$s.PowerOnline; charging=$s.Charging; discharging=$s.Discharging; chargeRate=$s.ChargeRate; dischargeRate=$s.DischargeRate; remaining=$s.RemainingCapacity; voltage=$s.Voltage } }
$r.trim = -not ((fsutil behavior query DisableDeleteNotify) -match 'NTFS DisableDeleteNotify = 1')
$p = powercfg /getactivescheme; if ($p -match '\((.+)\)') { $r.plan=$Matches[1] }
$r.whea30d = @(Get-WinEvent -FilterHashtable @{LogName='System';ProviderName='Microsoft-Windows-WHEA-Logger';StartTime=(Get-Date).AddDays(-30)}).Count
$r.unexpectedShutdowns30d = @(Get-WinEvent -FilterHashtable @{LogName='System';Id=41;ProviderName='Microsoft-Windows-Kernel-Power';StartTime=(Get-Date).AddDays(-30)}).Count
$r.disks=@(Get-PhysicalDisk | % { $c = $_ | Get-StorageReliabilityCounter; @{ id=$_.DeviceId; name=$_.FriendlyName; media="$($_.MediaType)"; bus="$($_.BusType)"; health="$($_.HealthStatus)"; size=$_.Size; wear=$c.Wear; temp=$c.Temperature; tempMax=$c.TemperatureMax; hours=$c.PowerOnHours; readErrors=$c.ReadErrorsTotal; writeErrors=$c.WriteErrorsTotal } })
$r | ConvertTo-Json -Depth 4 -Compress
"#;
    powershell(script).unwrap_or(serde_json::Value::Null)
}

// ---------------------------------------------------------------- NVMe SMART / health log

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NvmeHealth {
    pub drive: u32,
    pub critical_warning: u8,
    pub temp_c: i32,
    pub available_spare: u8,
    pub spare_threshold: u8,
    pub percent_used: u8,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub power_cycles: u64,
    pub power_on_hours: u64,
    pub unsafe_shutdowns: u64,
    pub media_errors: u64,
    pub warning_temp_minutes: u32,
    pub critical_temp_minutes: u32,
}

const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;

fn u64_at(b: &[u8], o: usize) -> u64 {
    // NVMe counters are 128-bit little-endian; the high half is zero for any real drive.
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn nvme_one(drive: u32) -> Option<NvmeHealth> {
    let path = HSTRING::from(format!("\\\\.\\PhysicalDrive{drive}"));
    // Access 0 = query only: the drive is never opened for reading or writing data.
    let h = unsafe {
        CreateFileW(&path, 0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None)
    }
    .ok()?;
    // STORAGE_PROPERTY_QUERY (8 bytes) + STORAGE_PROTOCOL_SPECIFIC_DATA (40) + 512-byte log
    let mut buf = vec![0u8; 8 + 40 + 512];
    let put = |b: &mut [u8], o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    put(&mut buf, 0, 50); // StorageDeviceProtocolSpecificProperty
    put(&mut buf, 4, 0); // PropertyStandardQuery
    put(&mut buf, 8, 3); // ProtocolTypeNvme
    put(&mut buf, 12, 2); // NVMeDataTypeLogPage
    put(&mut buf, 16, 2); // NVME_LOG_PAGE_HEALTH_INFO
    put(&mut buf, 20, 0);
    put(&mut buf, 24, 40); // ProtocolDataOffset
    put(&mut buf, 28, 512); // ProtocolDataLength
    let mut ret = 0u32;
    let len = buf.len() as u32;
    let ok = unsafe {
        DeviceIoControl(h, IOCTL_STORAGE_QUERY_PROPERTY, Some(buf.as_ptr() as *const c_void), len, Some(buf.as_mut_ptr() as *mut c_void), len, Some(&mut ret), None)
    }
    .is_ok();
    unsafe {
        let _ = CloseHandle(h);
    }
    if !ok || ret < 48 + 200 {
        return None;
    }
    // Output: STORAGE_PROTOCOL_DATA_DESCRIPTOR (Version, Size, then the 40-byte header).
    let off = u32::from_le_bytes(buf[8 + 16..8 + 20].try_into().unwrap()) as usize;
    let l = buf.get(8 + off..8 + off + 512)?;
    let unit = 512_000u64; // one NVMe "data unit" = 1000 x 512 bytes
    Some(NvmeHealth {
        drive,
        critical_warning: l[0],
        temp_c: u16::from_le_bytes([l[1], l[2]]) as i32 - 273,
        available_spare: l[3],
        spare_threshold: l[4],
        percent_used: l[5],
        bytes_read: u64_at(l, 32).saturating_mul(unit),
        bytes_written: u64_at(l, 48).saturating_mul(unit),
        power_cycles: u64_at(l, 112),
        power_on_hours: u64_at(l, 128),
        unsafe_shutdowns: u64_at(l, 144),
        media_errors: u64_at(l, 160),
        warning_temp_minutes: u32::from_le_bytes(l[192..196].try_into().unwrap()),
        critical_temp_minutes: u32::from_le_bytes(l[196..200].try_into().unwrap()),
    })
}

pub fn nvme() -> Vec<NvmeHealth> {
    (0..8).filter_map(nvme_one).collect()
}

// ---------------------------------------------------------------- GPU

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GpuApp {
    pub pid: u32,
    pub name: String,
    pub path: String,
    pub dedicated: u64,
    pub util: f64,
    /// Windows per-app GPU preference: 0 = let Windows decide, 1 = power saving, 2 = high performance.
    pub preference: u8,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Gpu {
    pub name: String,
    pub vendor: String,
    pub vram: u64,
    pub shared: u64,
    pub util: f64,
    pub dedicated_used: u64,
    pub discrete: bool,
    pub apps: Vec<GpuApp>,
}

fn pdh_array(counter_path: PCWSTR, two_samples: bool, large: bool) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    unsafe {
        let mut q = Default::default();
        if PdhOpenQueryW(PCWSTR::null(), 0, &mut q) != 0 {
            return out;
        }
        let mut c = Default::default();
        if PdhAddEnglishCounterW(q, counter_path, 0, &mut c) == 0 {
            PdhCollectQueryData(q);
            if two_samples {
                std::thread::sleep(std::time::Duration::from_millis(700));
                PdhCollectQueryData(q);
            }
            let fmt = if large { PDH_FMT_LARGE } else { PDH_FMT_DOUBLE };
            let (mut size, mut count) = (0u32, 0u32);
            let _ = PdhGetFormattedCounterArrayW(c, fmt, &mut size, &mut count, None);
            if size > 0 {
                let mut buf = vec![0u8; size as usize + 64];
                let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
                if PdhGetFormattedCounterArrayW(c, fmt, &mut size, &mut count, Some(items)) == 0 {
                    for i in 0..count as usize {
                        let it = &*items.add(i);
                        let name = it.szName.to_string().unwrap_or_default();
                        let v = if large { it.FmtValue.Anonymous.largeValue as f64 } else { it.FmtValue.Anonymous.doubleValue };
                        out.push((name, v));
                    }
                }
            }
        }
        PdhCloseQuery(q);
    }
    out
}

/// "pid_1234_luid_0x00000000_0x000106ec_phys_0_eng_3_engtype_3D" -> (1234, "0x00000000_0x000106ec", "eng_3")
fn parse_instance(s: &str) -> Option<(u32, String, String)> {
    let parts: Vec<&str> = s.split('_').collect();
    let pid = parts.get(1)?.parse().ok()?;
    let luid = format!("{}_{}", parts.get(3)?, parts.get(4)?).to_lowercase();
    let eng = parts.iter().position(|p| *p == "eng").and_then(|i| parts.get(i + 1)).map(|e| e.to_string()).unwrap_or_default();
    Some((pid, luid, eng))
}

fn exe_path(pid: u32) -> String {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        if r.is_ok() {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        }
    }
}

const PREF_KEY: PCWSTR = w!("Software\\Microsoft\\DirectX\\UserGpuPreferences");

pub fn get_preference(path: &str) -> u8 {
    if path.is_empty() {
        return 0;
    }
    let mut buf = [0u16; 256];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, PREF_KEY, &HSTRING::from(path), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut c_void), Some(&mut size))
    };
    if r.is_err() {
        return 0;
    }
    let s = String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]);
    s.split(';').find_map(|kv| kv.strip_prefix("GpuPreference=")).and_then(|v| v.parse().ok()).unwrap_or(0)
}

/// Same setting as Windows Settings > Graphics. Takes effect the next time the app starts.
pub fn set_preference(path: &str, pref: u8) -> Result<(), String> {
    if !path.to_lowercase().ends_with(".exe") || !std::path::Path::new(path).is_file() {
        return Err("not an app path".into());
    }
    let name = HSTRING::from(path);
    let r = if pref == 0 {
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, PREF_KEY, &name) }
    } else {
        let data: Vec<u16> = format!("GpuPreference={pref};").encode_utf16().chain(std::iter::once(0)).collect();
        unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, PREF_KEY, &name, REG_SZ.0, Some(data.as_ptr() as *const c_void), (data.len() * 2) as u32) }
    };
    if r.is_ok() || pref == 0 {
        Ok(())
    } else {
        Err(format!("registry write failed ({})", r.0))
    }
}

pub fn gpus() -> Vec<Gpu> {
    let mut adapters: Vec<(String, Gpu)> = Vec::new();
    unsafe {
        if let Ok(f) = CreateDXGIFactory1::<IDXGIFactory1>() {
            let mut i = 0;
            while let Ok(a) = f.EnumAdapters1(i) {
                i += 1;
                let Ok(d) = a.GetDesc1() else { continue };
                if d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                    continue;
                }
                let luid = format!("0x{:08x}_0x{:08x}", d.AdapterLuid.HighPart as u32, d.AdapterLuid.LowPart);
                if adapters.iter().any(|(l, _)| *l == luid) {
                    continue;
                }
                let end = d.Description.iter().position(|&c| c == 0).unwrap_or(d.Description.len());
                let vendor = match d.VendorId {
                    0x10DE => "NVIDIA",
                    0x8086 => "Intel",
                    0x1002 => "AMD",
                    _ => "Other",
                };
                adapters.push((
                    luid,
                    Gpu {
                        name: String::from_utf16_lossy(&d.Description[..end]),
                        vendor: vendor.into(),
                        vram: d.DedicatedVideoMemory as u64,
                        shared: d.SharedSystemMemory as u64,
                        util: 0.0,
                        dedicated_used: 0,
                        discrete: vendor == "NVIDIA" || (vendor == "AMD" && d.DedicatedVideoMemory > 1 << 30),
                        apps: Vec::new(),
                    },
                ));
            }
        }
    }
    let names: HashMap<u32, String> = {
        let mut m = HashMap::new();
        crate::mem::for_each_process(|pid, name| {
            m.insert(pid, name);
        });
        m
    };
    // Per-process dedicated memory on each adapter: having allocations on the NVIDIA GPU is what
    // keeps it awake (and draining the battery) on a hybrid-graphics laptop.
    let mem = pdh_array(w!("\\GPU Process Memory(*)\\Dedicated Usage"), false, true);
    let util = pdh_array(w!("\\GPU Engine(*)\\Utilization Percentage"), true, false);
    let mut per_app: HashMap<(String, u32), (u64, f64)> = HashMap::new();
    for (inst, v) in &mem {
        if let Some((pid, luid, _)) = parse_instance(inst) {
            per_app.entry((luid, pid)).or_default().0 += *v as u64;
        }
    }
    let mut per_engine: HashMap<(String, String), f64> = HashMap::new();
    for (inst, v) in &util {
        if let Some((pid, luid, eng)) = parse_instance(inst) {
            *per_engine.entry((luid.clone(), eng)).or_default() += v;
            let e = per_app.entry((luid, pid)).or_default();
            e.1 = e.1.max(*v);
        }
    }
    for (luid, g) in adapters.iter_mut() {
        g.util = per_engine.iter().filter(|((l, _), _)| l == luid).map(|(_, v)| *v).fold(0.0, f64::max).min(100.0);
        let mut apps: Vec<GpuApp> = per_app
            .iter()
            .filter(|((l, pid), (bytes, u))| l == luid && *pid != 0 && (*bytes > 0 || *u > 0.05))
            .map(|((_, pid), (bytes, u))| {
                let path = exe_path(*pid);
                GpuApp { pid: *pid, name: names.get(pid).cloned().unwrap_or_else(|| format!("pid {pid}")), preference: get_preference(&path), path, dedicated: *bytes, util: *u }
            })
            .collect();
        g.dedicated_used = apps.iter().map(|a| a.dedicated).sum();
        apps.sort_by(|a, b| b.dedicated.cmp(&a.dedicated));
        g.apps = apps;
    }
    adapters.into_iter().map(|(_, g)| g).collect()
}

/// NVIDIA sensors. Running nvidia-smi wakes a sleeping laptop GPU, so the UI calls this only
/// on request or while plugged in.
pub fn nvidia_smi() -> Result<HashMap<String, String>, String> {
    let fields = "name,driver_version,temperature.gpu,utilization.gpu,power.draw,power.limit,memory.used,memory.total,pstate,clocks.gr,clocks.max.gr,fan.speed";
    let exe = std::path::Path::new(&std::env::var("SystemRoot").unwrap_or("C:\\Windows".into())).join("System32\\nvidia-smi.exe");
    let out = std::process::Command::new(exe)
        .args([&format!("--query-gpu={fields}"), "--format=csv,noheader,nounits"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|_| "nvidia-smi not found".to_string())?;
    let line = String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or_default().to_string();
    if !out.status.success() || line.is_empty() {
        return Err("NVIDIA driver did not answer".into());
    }
    Ok(fields.split(',').zip(line.split(',').map(|s| s.trim().to_string())).map(|(k, v)| (k.to_string(), v)).collect())
}
