//! Memory statistics and the individual optimisation steps.
//!
//! Every step here only asks Windows to do something its own memory manager already does on
//! its own (trim working sets, write dirty pages, drop cached pages). Nothing is deleted from
//! disk, no process is stopped, and no setting is changed permanently.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::c_void;
use std::mem::size_of;
use std::time::Instant;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_SUCCESS, GENERIC_READ, GENERIC_WRITE, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, GetTokenInformation, LookupPrivilegeValueW, TokenElevation, LUID_AND_ATTRIBUTES,
    SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_ELEVATION, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FlushFileBuffers, GetDriveTypeW, GetLogicalDriveStringsW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Memory::SetSystemFileCacheSize;
use windows::Win32::System::ProcessStatus::{
    GetPerformanceInfo, K32EmptyWorkingSet, K32GetProcessMemoryInfo, PERFORMANCE_INFORMATION, PROCESS_MEMORY_COUNTERS,
    PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SET_QUOTA,
};

#[link(name = "ntdll", kind = "raw-dylib")]
extern "system" {
    fn NtQuerySystemInformation(class: u32, info: *mut c_void, len: u32, ret: *mut u32) -> i32;
    fn NtSetSystemInformation(class: u32, info: *mut c_void, len: u32) -> i32;
}

const SYSTEM_MEMORY_LIST_INFORMATION: u32 = 80;
const SYSTEM_COMBINE_PHYSICAL_MEMORY_INFORMATION: u32 = 130;
const SYSTEM_REGISTRY_RECONCILIATION_INFORMATION: u32 = 155;

const MEMORY_FLUSH_MODIFIED_LIST: i32 = 3;
const MEMORY_PURGE_STANDBY_LIST: i32 = 4;
const MEMORY_PURGE_LOW_PRIORITY_STANDBY_LIST: i32 = 5;

const STATUS_PRIVILEGE_NOT_HELD: i32 = 0xC000_0061u32 as i32;
const STATUS_ACCESS_DENIED: i32 = 0xC000_0022u32 as i32;

#[repr(C)]
#[derive(Default)]
struct SystemMemoryListInformation {
    zero_page_count: usize,
    free_page_count: usize,
    modified_page_count: usize,
    modified_no_write_page_count: usize,
    bad_page_count: usize,
    page_count_by_priority: [usize; 8],
    repurposed_pages_by_priority: [usize; 8],
    modified_page_count_page_file: usize,
}

#[repr(C)]
struct MemoryCombineInformationEx {
    handle: isize,
    pages_combined: usize,
    flags: u32,
}

/// Breakdown of physical pages by list. Only readable from an elevated process.
#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Lists {
    pub zero: u64,
    pub free: u64,
    pub modified: u64,
    pub modified_no_write: u64,
    pub standby: u64,
    pub standby_low: u64,
    pub standby_by_priority: [u64; 8],
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub total: u64,
    pub available: u64,
    pub used: u64,
    pub used_pct: f64,
    pub commit_used: u64,
    pub commit_limit: u64,
    pub system_cache: u64,
    pub lists: Option<Lists>,
}

pub fn stats() -> Stats {
    let mut s = Stats::default();
    unsafe {
        let mut ms = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        if GlobalMemoryStatusEx(&mut ms).is_ok() {
            s.total = ms.ullTotalPhys;
            s.available = ms.ullAvailPhys;
            s.used = ms.ullTotalPhys.saturating_sub(ms.ullAvailPhys);
            s.used_pct = if s.total > 0 { s.used as f64 * 100.0 / s.total as f64 } else { 0.0 };
        }
        let mut pi = PERFORMANCE_INFORMATION { cb: size_of::<PERFORMANCE_INFORMATION>() as u32, ..Default::default() };
        if GetPerformanceInfo(&mut pi, pi.cb).is_ok() {
            let page = pi.PageSize as u64;
            s.commit_used = pi.CommitTotal as u64 * page;
            s.commit_limit = pi.CommitLimit as u64 * page;
            s.system_cache = pi.SystemCache as u64 * page;
        }
        let mut ml = SystemMemoryListInformation::default();
        let mut ret = 0u32;
        let st = NtQuerySystemInformation(
            SYSTEM_MEMORY_LIST_INFORMATION,
            &mut ml as *mut _ as *mut c_void,
            size_of::<SystemMemoryListInformation>() as u32,
            &mut ret,
        );
        if st >= 0 {
            let p = 4096u64;
            let mut by = [0u64; 8];
            for (i, v) in ml.page_count_by_priority.iter().enumerate() {
                by[i] = *v as u64 * p;
            }
            s.lists = Some(Lists {
                zero: ml.zero_page_count as u64 * p,
                free: ml.free_page_count as u64 * p,
                modified: ml.modified_page_count as u64 * p,
                modified_no_write: ml.modified_no_write_page_count as u64 * p,
                standby: by.iter().sum(),
                standby_low: by[0],
                standby_by_priority: by,
            });
        }
    }
    s
}

// ---------------------------------------------------------------- privileges

pub fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut el = TOKEN_ELEVATION::default();
        let mut ret = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut el as *mut _ as *mut c_void),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && el.TokenIsElevated != 0
    }
}

fn enable_privilege(name: PCWSTR) -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut luid = LUID::default();
        let mut ok = LookupPrivilegeValueW(PCWSTR::null(), name, &mut luid).is_ok();
        if ok {
            let tp = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }],
            };
            ok = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None).is_ok() && GetLastError() == ERROR_SUCCESS;
        }
        let _ = CloseHandle(token);
        ok
    }
}

/// Turn on the privileges the optimisation calls need. They exist only in an elevated token.
pub fn enable_privileges() {
    enable_privilege(w!("SeProfileSingleProcessPrivilege"));
    enable_privilege(w!("SeIncreaseQuotaPrivilege"));
    enable_privilege(w!("SeDebugPrivilege"));
}

// ---------------------------------------------------------------- processes

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    pub working_set: u64,
    pub private: u64,
}

fn wide_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

pub fn for_each_process(mut f: impl FnMut(u32, String)) {
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return };
        let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                f(e.th32ProcessID, wide_to_string(&e.szExeFile));
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
}

pub fn processes(limit: usize) -> Vec<ProcInfo> {
    let mut out = Vec::new();
    for_each_process(|pid, name| {
        if pid == 0 {
            return;
        }
        unsafe {
            let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return };
            let mut c = PROCESS_MEMORY_COUNTERS_EX { cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
            if K32GetProcessMemoryInfo(h, &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS, c.cb).as_bool() {
                out.push(ProcInfo { pid, name, working_set: c.WorkingSetSize as u64, private: c.PrivateUsage as u64 });
            }
            let _ = CloseHandle(h);
        }
    });
    out.sort_by(|a, b| b.working_set.cmp(&a.working_set));
    out.truncate(limit);
    out
}

fn trim_pid(pid: u32) -> bool {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_QUOTA, false, pid) else { return false };
        let ok = K32EmptyWorkingSet(h).as_bool();
        let _ = CloseHandle(h);
        ok
    }
}

pub fn trim_one(pid: u32) -> Result<(), String> {
    if trim_pid(pid) {
        Ok(())
    } else {
        Err("Windows refused (protected process, or needs administrator)".into())
    }
}

// ---------------------------------------------------------------- optimisation areas

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Area {
    WorkingSets,
    SystemFileCache,
    ModifiedFileCache,
    ModifiedPageList,
    StandbyLow,
    Standby,
    CombinePages,
    RegistryCache,
}

/// The order matters: trimming moves pages to the modified/standby lists, flushing modified
/// pages moves them to standby, and purging standby finally turns them into free pages.
pub const ORDER: [Area; 8] = [
    Area::WorkingSets,
    Area::SystemFileCache,
    Area::ModifiedFileCache,
    Area::ModifiedPageList,
    Area::Standby,
    Area::StandbyLow,
    Area::CombinePages,
    Area::RegistryCache,
];

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AreaResult {
    pub area: Area,
    pub ok: bool,
    pub detail: String,
    pub ms: u64,
}

fn nt_status_msg(st: i32) -> String {
    match st {
        STATUS_PRIVILEGE_NOT_HELD | STATUS_ACCESS_DENIED => "needs administrator".into(),
        _ => format!("Windows returned 0x{:08X}", st as u32),
    }
}

fn memory_list_command(cmd: i32) -> Result<String, String> {
    let mut c = cmd;
    let st = unsafe { NtSetSystemInformation(SYSTEM_MEMORY_LIST_INFORMATION, &mut c as *mut _ as *mut c_void, 4) };
    if st >= 0 {
        Ok("done".into())
    } else {
        Err(nt_status_msg(st))
    }
}

fn trim_working_sets(excluded: &HashSet<String>, skip_pid: Option<u32>) -> Result<String, String> {
    let me = unsafe { GetCurrentProcessId() };
    let (mut ok, mut skipped, mut denied) = (0u32, 0u32, 0u32);
    for_each_process(|pid, name| {
        if pid == 0 || pid == 4 || pid == me {
            return;
        }
        if excluded.contains(&name.to_lowercase()) || Some(pid) == skip_pid {
            skipped += 1;
            return;
        }
        if trim_pid(pid) {
            ok += 1;
        } else {
            denied += 1;
        }
    });
    if ok == 0 {
        return Err("no process could be trimmed (needs administrator)".into());
    }
    Ok(format!("{ok} processes trimmed, {skipped} skipped, {denied} protected"))
}

fn flush_system_file_cache() -> Result<String, String> {
    unsafe { SetSystemFileCacheSize(usize::MAX, usize::MAX, 0) }
        .map(|_| "done".into())
        .map_err(|_| "needs administrator".into())
}

/// Write each fixed volume's dirty cached data to disk (the same thing Windows does a few
/// seconds later on its own). Nothing is removed.
fn flush_volumes() -> Result<String, String> {
    let mut buf = [0u16; 512];
    let n = unsafe { GetLogicalDriveStringsW(Some(&mut buf)) } as usize;
    let mut flushed = Vec::new();
    for root in buf[..n.min(buf.len())].split(|&c| c == 0).filter(|s| !s.is_empty()) {
        let mut r: Vec<u16> = root.to_vec();
        r.push(0);
        // DRIVE_FIXED = 3
        if unsafe { GetDriveTypeW(PCWSTR(r.as_ptr())) } != 3 {
            continue;
        }
        let letter = String::from_utf16_lossy(&root[..2]);
        let path: Vec<u16> = format!("\\\\.\\{letter}").encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            if let Ok(h) = CreateFileW(
                PCWSTR(path.as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            ) {
                if FlushFileBuffers(h).is_ok() {
                    flushed.push(letter);
                }
                let _ = CloseHandle(h);
            }
        }
    }
    if flushed.is_empty() {
        Err("needs administrator".into())
    } else {
        Ok(format!("flushed {}", flushed.join(" ")))
    }
}

fn combine_pages() -> Result<String, String> {
    let mut info = MemoryCombineInformationEx { handle: 0, pages_combined: 0, flags: 0 };
    let st = unsafe {
        NtSetSystemInformation(
            SYSTEM_COMBINE_PHYSICAL_MEMORY_INFORMATION,
            &mut info as *mut _ as *mut c_void,
            size_of::<MemoryCombineInformationEx>() as u32,
        )
    };
    if st >= 0 {
        Ok(format!("{} identical pages merged", info.pages_combined))
    } else {
        Err(nt_status_msg(st))
    }
}

fn flush_registry() -> Result<String, String> {
    let st = unsafe { NtSetSystemInformation(SYSTEM_REGISTRY_RECONCILIATION_INFORMATION, std::ptr::null_mut(), 0) };
    if st >= 0 {
        Ok("done".into())
    } else {
        Err(nt_status_msg(st))
    }
}

pub fn run_area(area: Area, excluded: &HashSet<String>, skip_pid: Option<u32>) -> AreaResult {
    let t = Instant::now();
    let r = match area {
        Area::WorkingSets => trim_working_sets(excluded, skip_pid),
        Area::SystemFileCache => flush_system_file_cache(),
        Area::ModifiedFileCache => flush_volumes(),
        Area::ModifiedPageList => memory_list_command(MEMORY_FLUSH_MODIFIED_LIST),
        Area::Standby => memory_list_command(MEMORY_PURGE_STANDBY_LIST),
        Area::StandbyLow => memory_list_command(MEMORY_PURGE_LOW_PRIORITY_STANDBY_LIST),
        Area::CombinePages => combine_pages(),
        Area::RegistryCache => flush_registry(),
    };
    let ms = t.elapsed().as_millis() as u64;
    match r {
        Ok(detail) => AreaResult { area, ok: true, detail, ms },
        Err(detail) => AreaResult { area, ok: false, detail, ms },
    }
}
