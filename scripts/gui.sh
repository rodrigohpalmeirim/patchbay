#!/usr/bin/env bash
# Headless GUI session for Patchbay in the dev container (Xvfb + D-Bus + gnome-keyring + openbox).
# Designed for automated agents: start the app, wait for its window, screenshot, click/type.
#
#   scripts/gui.sh start [cmd...]        start session in background (default cmd: npm run tauri dev)
#   scripts/gui.sh wait-window [regex] [timeout_s]
#                                        wait until a visible window whose title matches regex
#                                        exists (default: Patchbay, 600s); prints its window id
#   scripts/gui.sh screenshot [out.png]  capture the whole virtual display (import -window root)
#   scripts/gui.sh xdotool <args...>     run xdotool on the virtual display
#   scripts/gui.sh exec <cmd...>         run any command in the session (DISPLAY/DBUS set)
#   scripts/gui.sh shell                 interactive shell in the session
#   scripts/gui.sh logs [-f]             command output (docker logs)
#   scripts/gui.sh status                running? command exit code? windows?
#   scripts/gui.sh stop                  stop and remove the session container
#   scripts/gui.sh <cmd...>              shorthand for: start <cmd...>
#
# Env: PATCHBAY_GUI_NAME (container name, default patchbay-gui), PATCHBAY_GUI_DISPLAY (default :99),
#      PATCHBAY_GUI_SCREEN (default 1440x900x24), PATCHBAY_KEYRING_PASSWORD (default patchbay),
#      PATCHBAY_GUI_WM (openbox | none).
set -euo pipefail
# shellcheck source=scripts/lib/devenv.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/devenv.sh"

NAME="${PATCHBAY_GUI_NAME:-patchbay-gui}"
DISPLAY_NUM="${PATCHBAY_GUI_DISPLAY:-:99}"
GUI_ENV=(-e "DISPLAY=$DISPLAY_NUM" -e "DBUS_SESSION_BUS_ADDRESS=unix:path=/tmp/patchbay-gui/bus")

usage() { sed -n '2,25p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

running() { [[ "$(docker inspect -f '{{.State.Running}}' "$NAME" 2>/dev/null)" == true ]]; }

require_running() {
    running || { echo "gui session '$NAME' is not running (scripts/gui.sh start)" >&2; exit 1; }
}

in_session() { # in_session [docker exec flags...] -- cmd...
    local flags=()
    while [[ $# -gt 0 && "$1" != -- ]]; do flags+=("$1"); shift; done
    shift
    local wd="$PWD"
    case "$wd" in "$PATCHBAY_REPO"|"$PATCHBAY_REPO"/*) ;; *) wd="$PATCHBAY_REPO" ;; esac
    docker exec "${flags[@]}" "${GUI_ENV[@]}" -w "$wd" "$NAME" "$@"
}

cmd_start() {
    if running; then
        echo "gui session '$NAME' already running; use 'scripts/gui.sh stop' first" >&2; exit 1
    fi
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    if [[ $# -eq 0 ]]; then
        set -- npm run tauri dev
        if [[ ! -d "$PATCHBAY_REPO/node_modules" ]]; then
            echo "node_modules missing: run 'scripts/dev.sh npm ci' first" >&2; exit 1
        fi
    fi
    patchbay_ensure_image
    patchbay_run_args
    local extra=()
    local v
    for v in PATCHBAY_GUI_SCREEN PATCHBAY_KEYRING_PASSWORD PATCHBAY_GUI_WM; do
        if [[ -n "${!v:-}" ]]; then extra+=(-e "$v=${!v}"); fi
    done
    docker run -d --name "$NAME" --init "${PATCHBAY_RUN_ARGS[@]}" "${extra[@]}" \
        -e "DISPLAY=$DISPLAY_NUM" "$PATCHBAY_DEV_IMAGE" \
        bash "$PATCHBAY_REPO/docker/dev/gui-session.sh" "$@" >/dev/null
    for _ in $(seq 1 150); do
        if docker exec "$NAME" test -e /tmp/patchbay-gui/ready 2>/dev/null; then
            echo "gui session '$NAME' up on display $DISPLAY_NUM, running: $*"
            echo "next: scripts/gui.sh wait-window | screenshot out.png | logs -f | stop"
            return 0
        fi
        if ! running; then break; fi
        sleep 0.2
    done
    echo "gui session failed to start:" >&2
    docker logs "$NAME" >&2 || true
    exit 1
}

cmd_wait_window() {
    require_running
    local pattern="${1:-Patchbay}" timeout="${2:-600}" start=$SECONDS id
    while (( SECONDS - start < timeout )); do
        id=$(in_session -- xdotool search --onlyvisible --name "$pattern" 2>/dev/null | head -n1 || true)
        if [[ -n "$id" ]]; then echo "$id"; return 0; fi
        if docker exec "$NAME" test -e /tmp/patchbay-gui/app.exit 2>/dev/null; then
            echo "command exited (status $(docker exec "$NAME" cat /tmp/patchbay-gui/app.exit)) before a window matching '$pattern' appeared" >&2
            return 1
        fi
        sleep 1
    done
    echo "timed out after ${timeout}s waiting for window '$pattern'" >&2
    return 1
}

cmd_screenshot() {
    require_running
    local out="${1:-$PATCHBAY_REPO/.cache/screenshots/gui-$(date +%Y%m%d-%H%M%S).png}"
    mkdir -p "$(dirname "$out")"
    in_session -- import -window root /tmp/patchbay-gui/screenshot.png
    docker cp -q "$NAME:/tmp/patchbay-gui/screenshot.png" "$out"
    echo "$out"
}

cmd_status() {
    if ! running; then echo "gui session '$NAME': not running"; return 1; fi
    echo "gui session '$NAME': running on $DISPLAY_NUM"
    if docker exec "$NAME" test -e /tmp/patchbay-gui/app.exit; then
        echo "command exited with status $(docker exec "$NAME" cat /tmp/patchbay-gui/app.exit)"
    else
        echo "command still running"
    fi
    echo "visible windows:"
    # shellcheck disable=SC2016  # expanded by the inner sh
    in_session -- sh -c 'for w in $(xdotool search --onlyvisible --name "." 2>/dev/null); do printf "  %s  %s\n" "$w" "$(xdotool getwindowname "$w")"; done' || true
}

sub="${1:-}"
case "$sub" in
    ""|-h|--help|help) usage ;;
    start) shift; cmd_start "$@" ;;
    wait-window) shift; cmd_wait_window "$@" ;;
    screenshot) shift; cmd_screenshot "$@" ;;
    xdotool) shift; require_running; in_session -- xdotool "$@" ;;
    exec) shift; require_running
          if [[ -t 0 && -t 1 ]]; then in_session -it -- "$@"; else in_session -i -- "$@"; fi ;;
    shell) require_running; in_session -it -- bash ;;
    logs) shift; docker logs "$@" "$NAME" ;;
    status) cmd_status ;;
    stop) docker rm -f "$NAME" >/dev/null 2>&1 && echo "stopped $NAME" || echo "$NAME not running" ;;
    *) cmd_start "$@" ;;
esac
