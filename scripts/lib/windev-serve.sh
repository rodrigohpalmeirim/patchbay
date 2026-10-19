#!/usr/bin/env bash
# Runs inside the patchbay-windev container (see scripts/windev.sh): Vite for the UI, a
# file server for the Windows build, and a loop that rebuilds it when Rust sources change.
set -euo pipefail

host="$1"
base="http://$host:1422"
work=/cache/target/windev
out="$work/out"
stamp="$work/stamp"
mkdir -p "$out"

export CARGO_TARGET_DIR="$work"
export CARGO_PROFILE_DEV_DEBUG=0 # smaller exe, faster links
# Separate identity, so the dev build has its own app data and runs next to an installed
# Patchbay (the single-instance lock is per identifier).
export TAURI_CONFIG="{\"identifier\":\"io.github.rodrigohpalmeirim.patchbay.dev\",\"productName\":\"Patchbay Dev\",\"build\":{\"devUrl\":\"http://$host:1420\"}}"

sed "s|__BASE__|$base|" scripts/lib/windev-run.ps1 > "$out/run.ps1"

TAURI_DEV_HOST="$host" npx vite 2>&1 | sed -u 's/^/[vite] /' &
python3 -m http.server 1422 --bind "$host" --directory "$out" 2>&1 | sed -u 's/^/[http] /' &

sources=(Cargo.toml Cargo.lock crates src-tauri/Cargo.toml src-tauri/build.rs src-tauri/tauri.conf.json
    src-tauri/src src-tauri/capabilities src-tauri/icons)

build() {
    echo "[build] building..."
    local start=$SECONDS
    if cargo xwin build -p patchbay --target x86_64-pc-windows-msvc 2>&1 | sed -u 's/^/[build] /'; then
        cp "$work/x86_64-pc-windows-msvc/debug/patchbay.exe" "$out/patchbay-dev.exe.part"
        mv -f "$out/patchbay-dev.exe.part" "$out/patchbay-dev.exe"
        date +%Y%m%d-%H%M%S > "$out/build-id.txt"
        echo "[build] done in $((SECONDS - start))s: build $(cat "$out/build-id.txt")"
    else
        echo "[build] FAILED; fix the error and save again"
    fi
}

while true; do
    if [[ ! -e "$stamp" ]] || [[ -n "$(find "${sources[@]}" -newer "$stamp" -type f -print -quit 2>/dev/null)" ]]; then
        sleep 1 # let a burst of saves settle
        touch "$stamp"
        build
    fi
    sleep 2
done
