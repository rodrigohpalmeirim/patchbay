#!/usr/bin/env bash
# Live dev build of the desktop app on a Windows machine, served from this (Linux) host.
#
#   scripts/windev.sh start [HOST]  start the dev server (HOST: address the Windows machine
#                                   uses to reach this host; default: this host's first IP)
#   scripts/windev.sh rebuild       rebuild the Windows exe now
#   scripts/windev.sh logs [-f]     vite + build output
#   scripts/windev.sh status
#   scripts/windev.sh stop
#
# A long-lived dev container runs Vite (UI with hot reload, ports 1420/1421), cross-compiles a
# Windows debug build whenever Rust code changes, and serves it on port 1422. On Windows, run
#
#   irm http://HOST:1422/run.ps1 | iex
#
# in PowerShell: it downloads the latest build, starts it and restarts it on every new build.
# UI changes show up instantly without a restart. The dev build is "Patchbay Dev": it has its
# own app data and runs next to an installed Patchbay, but edits the same ~/.ssh/config.
set -euo pipefail
# shellcheck source=scripts/lib/devenv.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/devenv.sh"

NAME="${PATCHBAY_WINDEV_NAME:-patchbay-windev}"

running() { [[ "$(docker inspect -f '{{.State.Running}}' "$NAME" 2>/dev/null)" == true ]]; }

case "${1:-}" in
    start)
        host="${2:-${PATCHBAY_WINDEV_HOST:-$(hostname -I | awk '{print $1}')}}"
        if running; then
            echo "already running; 'scripts/windev.sh stop' first" >&2
            exit 1
        fi
        docker rm -f "$NAME" >/dev/null 2>&1 || true
        patchbay_ensure_image
        patchbay_run_args
        docker run -d --name "$NAME" "${PATCHBAY_RUN_ARGS[@]}" -w "$PATCHBAY_REPO" \
            "$PATCHBAY_DEV_IMAGE" bash scripts/lib/windev-serve.sh "$host" >/dev/null
        echo "Started. The first build takes a few minutes; follow it with: scripts/windev.sh logs -f"
        echo
        echo "On Windows, run this in PowerShell:"
        echo
        echo "    irm http://$host:1422/run.ps1 | iex"
        ;;
    rebuild)
        running || { echo "not running" >&2; exit 1; }
        docker exec "$NAME" rm -f /cache/target/windev/stamp
        echo "Rebuild requested; see scripts/windev.sh logs -f"
        ;;
    logs)
        shift
        docker logs "$@" "$NAME"
        ;;
    status)
        if running; then
            echo "running"
            docker exec "$NAME" cat /cache/target/windev/out/build-id.txt 2>/dev/null | sed 's/^/build: /' || echo "build: none yet"
        else
            echo "stopped"
        fi
        ;;
    stop)
        docker rm -f "$NAME" >/dev/null 2>&1 && echo "stopped" || echo "not running"
        ;;
    *)
        sed -n '2,19p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
        exit 2
        ;;
esac
