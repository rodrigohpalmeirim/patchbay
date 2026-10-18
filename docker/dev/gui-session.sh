#!/usr/bin/env bash
# Runs INSIDE the patchbay-dev container (started by scripts/gui.sh start).
# Brings up a headless desktop session and runs the given command in it:
#   Xvfb ($DISPLAY, default :99, 1440x900x24) + openbox WM + D-Bus session bus
#   + gnome-keyring (Secret Service, unlocked) + AT-SPI bus, then "$@".
# The container stays alive after the command exits so it can still be inspected.
set -uo pipefail

export DISPLAY="${DISPLAY:-:99}"
GUI_DIR=/tmp/patchbay-gui
SCREEN="${PATCHBAY_GUI_SCREEN:-1440x900x24}"
KEYRING_PASSWORD="${PATCHBAY_KEYRING_PASSWORD:-patchbay}"
mkdir -p "$GUI_DIR" "$HOME/.local/share/keyrings"
rm -f "$GUI_DIR"/ready "$GUI_DIR"/app.exit

log() { echo "[gui-session] $*"; }

# 1. X server
Xvfb "$DISPLAY" -screen 0 "$SCREEN" -nolisten tcp -nolisten local -ac +extension RANDR +extension GLX \
    >"$GUI_DIR/xvfb.log" 2>&1 &
for _ in $(seq 1 100); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.1; done
if ! xdpyinfo >/dev/null 2>&1; then
    log "Xvfb failed to start on $DISPLAY:"; cat "$GUI_DIR/xvfb.log"; exit 1
fi
log "Xvfb up on $DISPLAY ($SCREEN)"

# 2. D-Bus session bus at a fixed path, so `docker exec` callers can find it.
export DBUS_SESSION_BUS_ADDRESS="unix:path=$GUI_DIR/bus"
dbus-daemon --session --address="$DBUS_SESSION_BUS_ADDRESS" --nofork --nopidfile \
    >"$GUI_DIR/dbus.log" 2>&1 &
for _ in $(seq 1 50); do [[ -S "$GUI_DIR/bus" ]] && break; sleep 0.1; done
log "D-Bus session bus: $DBUS_SESSION_BUS_ADDRESS"

# 3. Accessibility bus (silences GTK warnings; lets tools inspect the UI tree).
if [[ -x /usr/libexec/at-spi-bus-launcher ]]; then
    /usr/libexec/at-spi-bus-launcher --launch-immediately >"$GUI_DIR/at-spi.log" 2>&1 &
fi

# 4. Secret Service (for the `keyring` crate): login keyring, unlocked with a known
#    password (created on first run, persisted in the patchbay-dev-home volume).
printf '%s' "$KEYRING_PASSWORD" | gnome-keyring-daemon --replace --unlock --daemonize \
    --components=secrets >"$GUI_DIR/keyring.env" 2>"$GUI_DIR/keyring.log"
# shellcheck disable=SC1090
. <(sed 's/^/export /' "$GUI_DIR/keyring.env")
log "gnome-keyring unlocked (password: '$KEYRING_PASSWORD')"

# 5. Window manager (gives focus/activation semantics that xdotool relies on).
if [[ "${PATCHBAY_GUI_WM:-openbox}" != none ]]; then
    "${PATCHBAY_GUI_WM:-openbox}" >"$GUI_DIR/wm.log" 2>&1 &
    # Windows mapped while the WM is still starting up can end up unmanaged/unmapped,
    # so wait until it advertises itself (EWMH) before launching anything.
    for _ in $(seq 1 100); do
        xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null | grep -q 'window id' && break
        sleep 0.1
    done
    sleep 0.3
fi

cat >"$GUI_DIR/env" <<ENV
export DISPLAY=$DISPLAY
export DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS
ENV
touch "$GUI_DIR/ready"

if [[ $# -eq 0 ]]; then set -- npm run tauri dev; fi
log "running: $*"
"$@" 2>&1 | tee "$GUI_DIR/app.log" &
app_pid=$!
trap 'kill -TERM $app_pid 2>/dev/null; exit 0' TERM INT
wait "$app_pid"
rc=$?
echo "$rc" >"$GUI_DIR/app.exit"
log "command exited with status $rc (container kept alive; scripts/gui.sh stop to remove)"
sleep infinity &
wait $!
