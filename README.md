# MemPilot

A transparent RAM optimizer and laptop-care tool for Windows 10/11, built with Tauri 2 + Rust
and a plain TypeScript UI. No auto-update, no network access, no telemetry.

## Features
- **Memory**: live breakdown of RAM (in use, modified, standby cache, free), 2-minute graph, and
  Gentle / Balanced / Deep presets with before/after results for every step.
- **Standby cache**: standby list by priority, manual purge, and an automatic standby cleaner that
  only runs when the cache is large *and* truly free RAM is low (with a cooldown).
- **Processes**: top RAM users with per-app Trim and Exclude; the foreground app is never trimmed
  during hotkey, tray or automatic runs.
- **Junk cleaner**: temp files, crash dumps, update leftovers, browser and app caches, developer
  package caches (npm, pip, Yarn, NuGet, Gradle, Cargo, Go) and Windows component-store clean-up.
- **Duplicates**: exact duplicate finder (size, 64 KB hash, full xxh3 hash), byte-for-byte check
  before removal, removed copies go to the Recycle Bin. Filters for images, videos, music, documents.
- **Laptop health**: battery wear, NVMe SSD life (percentage used, data written, temperature, hours),
  per-app GPU use with a one-click switch to the power-saving GPU, NVIDIA sensors, WHEA hardware
  errors and unexpected shutdowns, TRIM status, free-space warnings.
- Tray icon with live memory %, middle-click to optimize, optional hotkey, start with Windows
  (elevated logon task), close to tray.

## Safety
| Area | Windows call | Effect |
|---|---|---|
| Working sets | `K32EmptyWorkingSet` per process (skips excluded + foreground app) | idle pages leave RAM, fault back in when touched |
| Modified page list | `NtSetSystemInformation(80, MemoryFlushModifiedList)` | dirty pages written to the page file |
| Standby (low / all) | `NtSetSystemInformation(80, MemoryPurge[LowPriority]StandbyList)` | cached pages become free |
| System file cache | `SetSystemFileCacheSize(-1, -1, 0)` | file cache flushed |
| Write file cache to disk | `FlushFileBuffers` on each fixed volume | pending writes hit disk now |
| Combine pages | `NtSetSystemInformation(130)` | identical pages merged |
| Registry cache | `NtSetSystemInformation(155)` | pending registry writes flushed |

Memory steps never stop a process or change a setting permanently. The junk cleaner only touches
its fixed list of folders, never follows links or junctions, keeps files changed in the last
24 hours, skips locked files and skips a browser's cache while that browser runs. Disk health is
read with query-only access (`IOCTL_STORAGE_QUERY_PROPERTY`). Settings live in
`%APPDATA%\MemPilot\settings.json`.

## Build (from WSL or Linux, no Visual Studio)
    rustup target add x86_64-pc-windows-msvc
    cargo install --locked cargo-xwin      # plus clang/LLVM >= 11 on PATH
    npm install
    npm run build:win    # -> src-tauri/target/x86_64-pc-windows-msvc/release/mempilot.exe

## Test
    bash scripts/win-e2e.sh            # runs the exe on Windows (isolated APPDATA) and checks it
    node scripts/ui-check.mjs shots    # screenshots the memory pages against a mock backend

Most clean-up steps need the app to run as administrator; the app offers "Restart as administrator".
