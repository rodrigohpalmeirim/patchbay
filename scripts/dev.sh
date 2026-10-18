#!/usr/bin/env bash
# Run a command inside the Patchbay dev container (Rust + Node 24 + Tauri Linux deps).
#
#   scripts/dev.sh <cmd...>     e.g. scripts/dev.sh npm ci
#                                    scripts/dev.sh cargo build -p patchbay
#   scripts/dev.sh shell        interactive bash
#   scripts/dev.sh build-image  (re)build the patchbay-dev image
#
# The repo is mounted at the same absolute path; the command runs as your uid/gid with
# HOME=/cache/home, CARGO_TARGET_DIR=/cache/target (named volume, separate from ./target)
# and --network host (test sshd containers are reachable on 127.0.0.1:2201-2203).
# For GUI work (Xvfb, screenshots, xdotool) use scripts/gui.sh.
set -euo pipefail
# shellcheck source=scripts/lib/devenv.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/devenv.sh"

if [[ $# -eq 0 ]]; then
    sed -n '2,13p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 2
fi

case "$1" in
    build-image)
        PATCHBAY_DEV_REBUILD=1 patchbay_ensure_image
        exit 0 ;;
    shell)
        shift
        set -- bash "$@" ;;
esac

patchbay_ensure_image
patchbay_run_args

tty_args=(-i)
if [[ -t 0 && -t 1 ]]; then tty_args=(-it); fi

exec docker run --rm "${tty_args[@]}" "${PATCHBAY_RUN_ARGS[@]}" "$PATCHBAY_DEV_IMAGE" "$@"
