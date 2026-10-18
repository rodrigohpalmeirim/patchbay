# shellcheck shell=bash
# Shared helpers for scripts/dev.sh and scripts/gui.sh (sourced, not executed).

PATCHBAY_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
PATCHBAY_DEV_IMAGE="${PATCHBAY_DEV_IMAGE:-patchbay-dev}"
PATCHBAY_VOLUME_PREFIX="${PATCHBAY_VOLUME_PREFIX:-patchbay-dev}"

# Build the dev image if missing (or when PATCHBAY_DEV_REBUILD=1).
patchbay_ensure_image() {
    if [[ "${PATCHBAY_DEV_REBUILD:-0}" == 1 ]] || ! docker image inspect "$PATCHBAY_DEV_IMAGE" >/dev/null 2>&1; then
        echo ">> building $PATCHBAY_DEV_IMAGE image (docker/dev/Dockerfile)..." >&2
        docker build -t "$PATCHBAY_DEV_IMAGE" "$PATCHBAY_REPO/docker/dev" >&2
    fi
}

# Common `docker run` arguments: host uid/gid, repo at the same absolute path,
# host network, named volumes for caches/target/home.
patchbay_run_args() {
    local workdir="$PWD"
    case "$workdir" in
        "$PATCHBAY_REPO"|"$PATCHBAY_REPO"/*) ;;
        *) workdir="$PATCHBAY_REPO" ;;
    esac
    PATCHBAY_RUN_ARGS=(
        --user "$(id -u):$(id -g)"
        --network host
        --ipc host
        --shm-size 1g
        --security-opt seccomp=unconfined
        -v "$PATCHBAY_REPO:$PATCHBAY_REPO"
        -v "$PATCHBAY_VOLUME_PREFIX-cargo-registry:/cache/cargo/registry"
        -v "$PATCHBAY_VOLUME_PREFIX-cargo-git:/cache/cargo/git"
        -v "$PATCHBAY_VOLUME_PREFIX-target:/cache/target"
        -v "$PATCHBAY_VOLUME_PREFIX-npm:/cache/npm"
        -v "$PATCHBAY_VOLUME_PREFIX-home:/cache/home"
        -v "$PATCHBAY_VOLUME_PREFIX-xwin:/cache/xwin"
        -w "$workdir"
        -e TERM="${TERM:-xterm-256color}"
    )
    # Pass through a few optional variables when set on the host.
    local v
    for v in RUST_LOG RUST_BACKTRACE PATCHBAY_DOCKER_TESTS CARGO_TERM_COLOR; do
        if [[ -n "${!v:-}" ]]; then PATCHBAY_RUN_ARGS+=(-e "$v=${!v}"); fi
    done
}
