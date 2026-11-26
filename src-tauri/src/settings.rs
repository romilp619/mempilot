use crate::mem::Area;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub areas: Vec<Area>,
    pub excluded: Vec<String>,
    pub skip_foreground: bool,
    /// Optimise automatically when available memory drops below this percent (0 = off).
    pub auto_below_pct: u32,
    /// Optimise every N hours (0 = off).
    pub auto_every_hours: u32,
    /// Minimum minutes between two automatic runs, so a busy PC is never cleaned in a loop.
    pub auto_cooldown_min: u32,
    /// Standby cleaner: purge only the standby list when it is at least `standby_min_mb`
    /// AND truly free memory (zero + free lists) is below `standby_free_below_mb`.
    pub standby_cleaner: bool,
    pub standby_min_mb: u32,
    pub standby_free_below_mb: u32,
    pub standby_cooldown_sec: u32,
    /// Purge only low-priority standby pages instead of the whole list.
    pub standby_low_only: bool,
    pub hotkey_enabled: bool,
    pub hotkey: String,
    pub close_to_tray: bool,
    pub start_minimized: bool,
    pub always_on_top: bool,
    pub low_priority: bool,
    pub tray_show_usage: bool,
    pub middle_click_optimize: bool,
    pub warn_pct: u32,
    pub danger_pct: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            // "Balanced" preset: frees memory without throwing away the whole file cache.
            areas: vec![Area::WorkingSets, Area::ModifiedPageList, Area::StandbyLow, Area::RegistryCache],
            // Trimming these makes audio crackle or the desktop hitch for a moment.
            excluded: vec!["audiodg.exe".into(), "dwm.exe".into()],
            skip_foreground: true,
            auto_below_pct: 0,
            auto_every_hours: 0,
            auto_cooldown_min: 10,
            standby_cleaner: false,
            standby_min_mb: 1024,
            standby_free_below_mb: 1024,
            standby_cooldown_sec: 60,
            standby_low_only: false,
            hotkey_enabled: false,
            hotkey: "Ctrl+Shift+M".into(),
            close_to_tray: true,
            start_minimized: false,
            always_on_top: false,
            low_priority: true,
            tray_show_usage: true,
            middle_click_optimize: true,
            warn_pct: 80,
            danger_pct: 90,
        }
    }
}

pub fn dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("MemPilot")
}

pub fn load() -> Settings {
    std::fs::read_to_string(dir().join("settings.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save(s: &Settings) -> Result<(), String> {
    let d = dir();
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    let tmp = d.join("settings.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s).unwrap()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, d.join("settings.json")).map_err(|e| e.to_string())
}
