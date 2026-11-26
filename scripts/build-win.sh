#!/usr/bin/env bash
# Cross-compile MemPilot for Windows from WSL (cargo-xwin, no Visual Studio).
set -euo pipefail
cd "$(dirname "$0")/.."
# cargo-xwin wants clang-cl / llvm-lib / llvm-rc by those names; link them from any LLVM >= 11.
mkdir -p .toolchain/bin
for t in clang-cl:clang llvm-lib:llvm-lib llvm-rc:llvm-rc; do
  name=${t%%:*}; src=${t##*:}
  [ -e ".toolchain/bin/$name" ] && continue
  found=$(command -v "$name" || ls /usr/bin/$src-[0-9]* 2>/dev/null | sort -V | tail -1 || true)
  [ -n "$found" ] && ln -sf "$found" ".toolchain/bin/$name"
done
export PATH="$PWD/.toolchain/bin:$PATH"
if [ "${1:-build}" = "check" ]; then
  cd src-tauri && exec cargo xwin check --target x86_64-pc-windows-msvc
fi
npx tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc --no-bundle 2>&1 \
  | grep -v -E "LNK4099|failed to load reference|^\s*$" || true
ls -la src-tauri/target/x86_64-pc-windows-msvc/release/mempilot.exe
