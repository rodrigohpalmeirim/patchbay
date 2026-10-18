# syntax=docker/dockerfile:1
#
# Patchbay development container: Rust + Node 24 + Tauri v2 Linux deps + a headless
# X/D-Bus/Secret-Service stack for GUI testing. Not used for releases.
#
# Normally built and run through scripts/dev.sh and scripts/gui.sh (image tag: patchbay-dev).
# The container is run as the *host* uid/gid, so everything writable lives under /cache
# (named volumes) and HOME=/cache/home.

ARG RUST_IMAGE=rust:1-bookworm
ARG NODE_IMAGE=node:24-bookworm-slim

FROM ${NODE_IMAGE} AS node

FROM ${RUST_IMAGE}

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
        # Tauri v2 build/runtime deps
        libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev \
        libayatana-appindicator3-dev librsvg2-dev libdbus-1-dev pkg-config \
        # Headless GUI testing: X server, session bus, Secret Service, screenshots, input
        xvfb xauth dbus-x11 at-spi2-core gnome-keyring libsecret-tools \
        imagemagick xdotool x11-utils x11-apps xterm xfonts-base openbox \
        fonts-dejavu fonts-noto fonts-noto-cjk fonts-noto-color-emoji \
        # Windows cross builds (cargo-xwin + NSIS installer, see docs/dev-container.md)
        clang lld llvm nsis \
        # Misc
        openssh-client sshpass git ca-certificates procps less sudo jq \
    && rm -rf /var/lib/apt/lists/* \
    # Debian ships no unversioned clang-cl; clang picks its MSVC-compatible driver from argv[0].
    && ln -s clang /usr/bin/clang-cl

# Tauri's NSIS template includes Win\RestartManager.nsh (NSIS >= 3.09); bookworm ships 3.08.
# The header only contains !defines, so take it from upstream NSIS 3.11.
ADD --chmod=644 --checksum=sha256:67149978412da9b283e2e1abe48a360093df9caf6363373e2cabe463978b7c0e \
    https://raw.githubusercontent.com/kichik/nsis/v311/Include/Win/RestartManager.nsh \
    /usr/share/nsis/Include/Win/RestartManager.nsh

# Node 24 from the official image (avoids a third-party apt repo).
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
COPY --from=node /usr/local/include/node /usr/local/include/node
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
 && ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx \
 && ln -s ../lib/node_modules/corepack/dist/corepack.js /usr/local/bin/corepack \
 && node --version && npm --version

RUN rustup component add rustfmt clippy \
 && rustup target add x86_64-pc-windows-msvc \
 && cargo install --locked cargo-xwin \
 && rm -rf /usr/local/cargo/registry /usr/local/cargo/git \
 && chmod -R a+rX /usr/local/rustup /usr/local/cargo

# Writable locations for an arbitrary (host) uid. Named volumes mounted on these
# paths inherit the 1777 mode on first use.
RUN mkdir -p /tmp/.X11-unix && chmod 1777 /tmp/.X11-unix \
 && mkdir -p /cache/home /cache/target /cache/npm /cache/xwin /cache/cargo/registry /cache/cargo/git \
 && chmod -R 1777 /cache \
 # Let the entrypoint add a passwd/group entry for the host uid (ssh, git, etc. need one).
 && chmod 666 /etc/passwd /etc/group \
 # Passwordless sudo for that user (dev container only).
 && echo 'ALL ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/dev && chmod 440 /etc/sudoers.d/dev

# Keep the rustup toolchain from the image, but put registry/git caches in volumes.
# cargo binaries stay on PATH via /usr/local/cargo/bin (already in PATH).
ENV CARGO_HOME=/cache/cargo \
    CARGO_TARGET_DIR=/cache/target \
    npm_config_cache=/cache/npm \
    # MSVC CRT + Windows SDK downloaded by cargo-xwin (named volume patchbay-dev-xwin).
    XWIN_CACHE_DIR=/cache/xwin \
    HOME=/cache/home \
    PATH=/usr/local/cargo/bin:/cache/cargo/bin:$PATH \
    # Software rendering for WebKitGTK under Xvfb (no GPU in the container).
    LIBGL_ALWAYS_SOFTWARE=1 \
    WEBKIT_DISABLE_DMABUF_RENDERER=1 \
    WEBKIT_DISABLE_COMPOSITING_MODE=1 \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8

COPY entrypoint.sh /usr/local/bin/patchbay-dev-entrypoint
RUN chmod 755 /usr/local/bin/patchbay-dev-entrypoint

ENTRYPOINT ["/usr/local/bin/patchbay-dev-entrypoint"]
CMD ["bash"]
