#!/usr/bin/env bash
# Run the built exe from %TEMP%\mempilot-dev with an isolated APPDATA + WebView2 profile, then close it.
set -uo pipefail
cd "$(dirname "$0")/.."
WTEMP=$(cd /mnt/c && cmd.exe /c "echo %TEMP%" 2>/dev/null | tr -d '\r')
DEV="$(wslpath "$WTEMP")/mempilot-dev"
mkdir -p "$DEV/appdata"
stop() { (cd /mnt/c && powershell.exe -NoProfile -Command 'if (Test-Path "$env:TEMP\mempilot-dev\pid.txt") { $id = [int](Get-Content "$env:TEMP\mempilot-dev\pid.txt"); $p = Get-Process -Id $id -ErrorAction SilentlyContinue; if ($p -and $p.ProcessName -eq "mempilot") { Stop-Process -Id $id -Force }; Remove-Item "$env:TEMP\mempilot-dev\pid.txt" }' 2>&1 | tr -d '\r'); }
stop; sleep 1
cp src-tauri/target/x86_64-pc-windows-msvc/release/mempilot.exe "$DEV/"
cp scripts/cdp.mjs scripts/e2e-win.mjs "$DEV/"
(cd /mnt/c && powershell.exe -NoProfile -Command '$env:APPDATA="$env:TEMP\mempilot-dev\appdata"; $env:MEMPILOT_TEST_INSTANCE="1"; $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9344"; $env:WEBVIEW2_USER_DATA_FOLDER="$env:TEMP\mempilot-dev\webview"; $p = Start-Process -FilePath "$env:TEMP\mempilot-dev\mempilot.exe" -PassThru; $p.Id | Out-File -Encoding ascii "$env:TEMP\mempilot-dev\pid.txt"; Start-Sleep 3' 2>&1 | tr -d '\r')
PID_W=$(tr -d '\r\n\xef\xbb\xbf' < "$DEV/pid.txt")
[ "${NOTEST:-0}" = "1" ] || (cd "$DEV" && node.exe e2e-win.mjs 9344 . "$PID_W" 2>&1 | tr -d '\r' | cut -c1-500)
RC=$?
[ "${KEEP:-0}" = "1" ] || stop
echo "dev dir: $DEV"
exit $RC
