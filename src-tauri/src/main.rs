#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cleaner;
mod disks;
mod dupes;
mod health;
mod mem;
mod settings;
mod trayicon;

use mem::{Area, AreaResult, Stats};
use parking_lot::Mutex;
use serde::Serialize;
use settings::Settings;
use std::collections::HashSet;
use std::os::windows::process::CommandExt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, SW_SHOWNORMAL};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TASK_NAME: &str = "MemPilot";

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Report {
    at: u64,
    trigger: String,
    ms: u64,
    results: Vec<AreaResult>,
    before: Stats,
    after: Stats,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct StandbyStatus {
    runs: u32,
    released: u64,
    last_at: u64,
    last_released: u64,
}

struct AppState {
    settings: Mutex<Settings>,
    history: Mutex<Vec<Report>>,
    busy: AtomicBool,
    last_fg: AtomicU32,
    last_auto: Mutex<Option<Instant>>,
    last_scheduled: Mutex<Instant>,
    last_standby: Mutex<Option<Instant>>,
    standby: Mutex<StandbyStatus>,
    tray_pct: AtomicU32,
    standby_released_total: AtomicU64,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn foreground_pid() -> u32 {
    unsafe {
        let hwnd = GetForegroundWindow();
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid
    }
}

/// Runs the selected areas in the safe order and records a before/after report.
fn optimize(app: &AppHandle, areas: Vec<Area>, trigger: &str) -> Result<Report, String> {
    let st = app.state::<AppState>();
    if st.busy.swap(true, Ordering::SeqCst) {
        return Err("An optimization is already running".into());
    }
    let t = Instant::now();
    mem::enable_privileges();
    let (excluded, skip_pid) = {
        let s = st.settings.lock();
        let ex: HashSet<String> = s.excluded.iter().map(|e| e.trim().to_lowercase()).collect();
        // A manual click comes from MemPilot itself, so the app to protect is the one used before it.
        let fg = st.last_fg.load(Ordering::Relaxed);
        (ex, if s.skip_foreground && fg != 0 { Some(fg) } else { None })
    };
    let before = mem::stats();
    let full_standby = areas.contains(&Area::Standby);
    let results: Vec<AreaResult> = mem::ORDER
        .iter()
        .filter(|a| areas.contains(a))
        .filter(|a| !(full_standby && **a == Area::StandbyLow))
        .map(|a| mem::run_area(*a, &excluded, skip_pid))
        .collect();
    std::thread::sleep(Duration::from_millis(400));
    let after = mem::stats();
    let report = Report { at: now_ms(), trigger: trigger.into(), ms: t.elapsed().as_millis() as u64, results, before, after };
    {
        let mut h = st.history.lock();
        h.insert(0, report.clone());
        h.truncate(50);
    }
    st.busy.store(false, Ordering::SeqCst);
    let _ = app.emit("optimized", &report);
    Ok(report)
}

fn optimize_in_background(app: &AppHandle, trigger: &'static str) {
    let app = app.clone();
    std::thread::spawn(move || {
        let areas = app.state::<AppState>().settings.lock().areas.clone();
        let _ = optimize(&app, areas, trigger);
    });
}

// ---------------------------------------------------------------- tray

fn update_tray(app: &AppHandle, stats: &Stats, force: bool) {
    let st = app.state::<AppState>();
    let (show, warn, danger) = {
        let s = st.settings.lock();
        (s.tray_show_usage, s.warn_pct, s.danger_pct)
    };
    let pct = stats.used_pct.round() as u32;
    let key = if show { pct } else { 1000 };
    if !force && st.tray_pct.swap(key, Ordering::Relaxed) == key {
        return;
    }
    st.tray_pct.store(key, Ordering::Relaxed);
    let Some(tray) = app.tray_by_id("main") else { return };
    let gb = |b: u64| b as f64 / 1_073_741_824.0;
    let _ = tray.set_tooltip(Some(format!(
        "MemPilot: {pct}% used ({:.1} of {:.1} GB)",
        gb(stats.used),
        gb(stats.total)
    )));
    let icon = if show {
        Image::new_owned(trayicon::render(pct, warn, danger), trayicon::SIZE, trayicon::SIZE)
    } else {
        app.default_window_icon().cloned().unwrap_or_else(|| Image::new_owned(vec![0; 4], 1, 1))
    };
    let _ = tray.set_icon(Some(icon));
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

// ---------------------------------------------------------------- background loop

fn sampler(app: AppHandle) {
    let me = unsafe { GetCurrentProcessId() };
    loop {
        let st = app.state::<AppState>();
        let fg = foreground_pid();
        if fg != 0 && fg != me {
            st.last_fg.store(fg, Ordering::Relaxed);
        }
        let stats = mem::stats();
        let _ = app.emit("stats", &stats);
        update_tray(&app, &stats, false);

        let s = st.settings.lock().clone();
        let busy = st.busy.load(Ordering::Relaxed);

        // Standby cleaner: only the standby list, only when it is big AND free memory is short.
        if s.standby_cleaner && !busy {
            if let Some(l) = &stats.lists {
                let truly_free = l.zero + l.free;
                let standby = if s.standby_low_only { l.standby_low } else { l.standby };
                let mb = 1_048_576u64;
                let cooled = st
                    .last_standby
                    .lock()
                    .map_or(true, |t| t.elapsed() >= Duration::from_secs(s.standby_cooldown_sec.max(5) as u64));
                if standby >= s.standby_min_mb as u64 * mb && truly_free < s.standby_free_below_mb as u64 * mb && cooled {
                    *st.last_standby.lock() = Some(Instant::now());
                    mem::enable_privileges();
                    let area = if s.standby_low_only { Area::StandbyLow } else { Area::Standby };
                    let r = mem::run_area(area, &HashSet::new(), None);
                    if r.ok {
                        let after = mem::stats();
                        let freed = after.lists.as_ref().map_or(0, |a| (a.zero + a.free).saturating_sub(truly_free));
                        st.standby_released_total.fetch_add(freed, Ordering::Relaxed);
                        let status = {
                            let mut sb = st.standby.lock();
                            sb.runs += 1;
                            sb.released += freed;
                            sb.last_at = now_ms();
                            sb.last_released = freed;
                            sb.clone()
                        };
                        let _ = app.emit("standby", &status);
                    }
                }
            }
        }

        // Full optimisation when available memory is low, with a cooldown.
        if s.auto_below_pct > 0 && !busy && stats.total > 0 {
            let avail_pct = stats.available as f64 * 100.0 / stats.total as f64;
            let cooled = st
                .last_auto
                .lock()
                .map_or(true, |t| t.elapsed() >= Duration::from_secs(s.auto_cooldown_min.max(1) as u64 * 60));
            if avail_pct < s.auto_below_pct as f64 && cooled {
                *st.last_auto.lock() = Some(Instant::now());
                optimize_in_background(&app, "auto (low memory)");
            }
        }

        // Scheduled optimisation every N hours.
        if s.auto_every_hours > 0 && !busy {
            let due = st.last_scheduled.lock().elapsed() >= Duration::from_secs(s.auto_every_hours as u64 * 3600);
            if due {
                *st.last_scheduled.lock() = Instant::now();
                optimize_in_background(&app, "scheduled");
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

// ---------------------------------------------------------------- applying settings

fn apply_settings(app: &AppHandle, s: &Settings) -> Result<(), String> {
    unsafe {
        let _ = SetPriorityClass(
            GetCurrentProcess(),
            if s.low_priority { BELOW_NORMAL_PRIORITY_CLASS } else { NORMAL_PRIORITY_CLASS },
        );
    }
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_always_on_top(s.always_on_top);
    }
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    if s.hotkey_enabled {
        gs.register(s.hotkey.as_str()).map_err(|e| format!("Hotkey {} could not be registered: {e}", s.hotkey))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- commands

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    settings: Settings,
    elevated: bool,
    history: Vec<Report>,
    standby: StandbyStatus,
    stats: Stats,
    version: String,
}

#[tauri::command]
fn get_snapshot(app: AppHandle, st: State<AppState>) -> Snapshot {
    Snapshot {
        settings: st.settings.lock().clone(),
        elevated: mem::is_elevated(),
        history: st.history.lock().clone(),
        standby: st.standby.lock().clone(),
        stats: mem::stats(),
        version: app.package_info().version.to_string(),
    }
}

#[tauri::command]
async fn run_optimize(app: AppHandle, areas: Vec<Area>) -> Result<Report, String> {
    tauri::async_runtime::spawn_blocking(move || optimize(&app, areas, "manual"))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_processes() -> Vec<mem::ProcInfo> {
    tauri::async_runtime::spawn_blocking(|| mem::processes(60)).await.unwrap_or_default()
}

#[tauri::command]
fn trim_process(pid: u32) -> Result<(), String> {
    mem::enable_privileges();
    mem::trim_one(pid)
}

#[tauri::command]
fn save_settings(app: AppHandle, st: State<AppState>, settings: Settings) -> Result<(), String> {
    settings::save(&settings)?;
    *st.settings.lock() = settings.clone();
    update_tray(&app, &mem::stats(), true);
    apply_settings(&app, &settings)
}

#[tauri::command]
fn clear_history(st: State<AppState>) {
    st.history.lock().clear();
}

#[tauri::command]
fn get_volumes() -> Vec<disks::Volume> {
    disks::volumes()
}

#[tauri::command]
async fn get_physical_disks() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(disks::physical_disks).await.map_err(|e| e.to_string())?
}

#[tauri::command]
fn restart_as_admin(app: AppHandle) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let r = unsafe { ShellExecuteW(None, w!("runas"), &HSTRING::from(exe.as_os_str()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    if r.0 as isize <= 32 {
        return Err("Administrator restart was cancelled".into());
    }
    app.exit(0);
    Ok(())
}

// ---------------------------------------------------------------- cleaner

#[tauri::command]
async fn cleaner_scan() -> Vec<cleaner::CategoryScan> {
    tauri::async_runtime::spawn_blocking(cleaner::scan).await.unwrap_or_default()
}

#[tauri::command]
async fn cleaner_clean(ids: Vec<String>) -> Vec<cleaner::CleanResult> {
    tauri::async_runtime::spawn_blocking(move || cleaner::clean(&ids)).await.unwrap_or_default()
}

#[tauri::command]
async fn component_store(analyze: bool) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || cleaner::component_cleanup(analyze)).await.map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------- duplicates

#[tauri::command]
fn dupes_default_roots() -> Vec<String> {
    let home = std::path::PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default());
    let mut v = Vec::new();
    for base in [home.clone(), home.join("OneDrive")] {
        for d in ["Desktop", "Documents", "Downloads", "Pictures", "Videos", "Music"] {
            let p = base.join(d);
            if p.is_dir() {
                v.push(p.to_string_lossy().to_string());
            }
        }
    }
    v
}

#[tauri::command]
async fn dupes_scan(app: AppHandle, roots: Vec<String>, filter: dupes::Filter, min_bytes: u64) -> Vec<dupes::DupGroup> {
    tauri::async_runtime::spawn_blocking(move || {
        let a = app.clone();
        dupes::scan(&roots, filter, min_bytes, &move |p| {
            let _ = a.emit("dupes-progress", &p);
        })
    })
    .await
    .unwrap_or_default()
}

#[tauri::command]
fn dupes_cancel() {
    dupes::CANCEL.store(true, Ordering::SeqCst);
}

#[tauri::command]
async fn dupes_remove(requests: Vec<dupes::RemoveRequest>) -> dupes::RemoveResult {
    tauri::async_runtime::spawn_blocking(move || dupes::remove(&requests))
        .await
        .unwrap_or(dupes::RemoveResult { recycled: 0, bytes: 0, refused: vec![] })
}

#[tauri::command]
async fn pick_folder(app: AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder().map(|p| p.to_string()))
        .await
        .ok()
        .flatten()
}

/// Opens Explorer with the file selected. Only existing paths are passed on.
#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    if !std::path::Path::new(&path).exists() {
        return Err("file no longer exists".into());
    }
    std::process::Command::new("explorer.exe").arg(format!("/select,{path}")).spawn().map(|_| ()).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- health

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthReport {
    power: health::Power,
    report: serde_json::Value,
    nvme: Vec<health::NvmeHealth>,
    volumes: Vec<disks::Volume>,
}

#[tauri::command]
async fn health_report() -> HealthReport {
    tauri::async_runtime::spawn_blocking(|| HealthReport {
        power: health::power(),
        report: health::system_report(),
        nvme: health::nvme(),
        volumes: disks::volumes(),
    })
    .await
    .unwrap_or(HealthReport { power: Default::default(), report: serde_json::Value::Null, nvme: vec![], volumes: vec![] })
}

#[tauri::command]
fn power_status() -> health::Power {
    health::power()
}

#[tauri::command]
async fn gpu_status() -> Vec<health::Gpu> {
    tauri::async_runtime::spawn_blocking(health::gpus).await.unwrap_or_default()
}

#[tauri::command]
async fn nvidia_sensors() -> Result<std::collections::HashMap<String, String>, String> {
    tauri::async_runtime::spawn_blocking(health::nvidia_smi).await.map_err(|e| e.to_string())?
}

#[tauri::command]
fn set_gpu_preference(path: String, preference: u8) -> Result<(), String> {
    health::set_preference(&path, preference.min(2))
}

fn schtasks(args: &[&str]) -> std::io::Result<std::process::Output> {
    std::process::Command::new("schtasks.exe").args(args).creation_flags(CREATE_NO_WINDOW).output()
}

#[tauri::command]
fn get_startup() -> bool {
    schtasks(&["/Query", "/TN", TASK_NAME]).map(|o| o.status.success()).unwrap_or(false)
}

/// Start with Windows through a logon task that runs elevated, so there is no UAC prompt at boot.
#[tauri::command]
fn set_startup(enabled: bool) -> Result<(), String> {
    let out = if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let tr = format!("\"{}\" --minimized", exe.display());
        schtasks(&["/Create", "/TN", TASK_NAME, "/TR", &tr, "/SC", "ONLOGON", "/RL", "HIGHEST", "/F"])
    } else {
        schtasks(&["/Delete", "/TN", TASK_NAME, "/F"])
    }
    .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn main() {
    let settings = settings::load();
    let minimized = std::env::args().any(|a| a == "--minimized") || settings.start_minimized;
    if mem::is_elevated() {
        mem::enable_privileges();
    }

    let mut builder = tauri::Builder::default();
    // Test runs (scripts/win-e2e.sh) must not hand over to, or bring up, the user's own copy.
    if std::env::var_os("MEMPILOT_TEST_INSTANCE").is_none() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)));
    }
    builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, ev| {
                    if ev.state == ShortcutState::Pressed {
                        optimize_in_background(app, "hotkey");
                    }
                })
                .build(),
        )
        .manage(AppState {
            settings: Mutex::new(settings.clone()),
            history: Mutex::new(Vec::new()),
            busy: AtomicBool::new(false),
            last_fg: AtomicU32::new(0),
            last_auto: Mutex::new(None),
            last_scheduled: Mutex::new(Instant::now()),
            last_standby: Mutex::new(None),
            standby: Mutex::new(StandbyStatus::default()),
            tray_pct: AtomicU32::new(u32::MAX),
            standby_released_total: AtomicU64::new(0),
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            run_optimize,
            get_processes,
            trim_process,
            save_settings,
            clear_history,
            get_volumes,
            get_physical_disks,
            restart_as_admin,
            get_startup,
            set_startup,
            cleaner_scan,
            cleaner_clean,
            component_store,
            dupes_default_roots,
            dupes_scan,
            dupes_cancel,
            dupes_remove,
            pick_folder,
            reveal,
            health_report,
            power_status,
            gpu_status,
            nvidia_sensors,
            set_gpu_preference
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let mut wb = tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::default())
                .title("MemPilot")
                .inner_size(1080.0, 760.0)
                .min_inner_size(880.0, 620.0)
                .visible(false);
            // Test/demo runs only: WebView2 ignores its WEBVIEW2_* variables in an elevated
            // process, so the debug port and a separate profile are set here instead.
            if std::env::var_os("MEMPILOT_TEST_INSTANCE").is_some() {
                if let Ok(port) = std::env::var("MEMPILOT_CDP_PORT") {
                    if port.parse::<u16>().is_ok() {
                        wb = wb.additional_browser_args(&format!(
                            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --remote-debugging-port={port}"
                        ));
                    }
                }
                if let Some(dir) = std::env::var_os("MEMPILOT_WEBVIEW_DIR") {
                    wb = wb.data_directory(dir.into());
                }
            }
            wb.build()?;
            let open = MenuItem::with_id(app, "open", "Open MemPilot", true, None::<&str>)?;
            let opt = MenuItem::with_id(app, "optimize", "Optimize now", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &opt, &quit])?;
            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().cloned().unwrap())
                .tooltip("MemPilot")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id().as_ref() {
                    "open" => show_main(app),
                    "optimize" => optimize_in_background(app, "tray"),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click { button, button_state: MouseButtonState::Up, .. } = ev {
                        let app = tray.app_handle();
                        match button {
                            MouseButton::Left => show_main(app),
                            MouseButton::Middle => {
                                if app.state::<AppState>().settings.lock().middle_click_optimize {
                                    optimize_in_background(app, "tray");
                                }
                            }
                            _ => {}
                        }
                    }
                })
                .build(app)?;

            let s = handle.state::<AppState>().settings.lock().clone();
            if let Err(e) = apply_settings(&handle, &s) {
                eprintln!("{e}");
            }
            if !minimized {
                show_main(&handle);
            }
            std::thread::spawn(move || sampler(handle));
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                if app.state::<AppState>().settings.lock().close_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                } else {
                    app.exit(0);
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running MemPilot");
}
