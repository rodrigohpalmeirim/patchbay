# Dev container (Linux GUI builds, headless GUI testing)

`patchbay-core` builds and tests fine on any host with a Rust toolchain. The Tauri app on
Linux needs WebKitGTK 4.1 and friends; instead of installing those on the host, use the
dev container (`docker/dev/Dockerfile`, image tag `patchbay-dev`, Debian bookworm +
Rust stable + Node 24 + Tauri deps + Xvfb/D-Bus/gnome-keyring/xdotool/ImageMagick,
plus `cargo-xwin`/clang/lld/NSIS for [Windows cross builds](#windows-cross-builds)).

## scripts/dev.sh - run a command in the container

```sh
scripts/dev.sh npm ci
scripts/dev.sh npm run check
scripts/dev.sh cargo clippy --workspace --all-targets -- -D warnings
scripts/dev.sh cargo build -p patchbay
scripts/dev.sh shell            # interactive bash
scripts/dev.sh build-image      # force a rebuild of patchbay-dev (also: PATCHBAY_DEV_REBUILD=1)
```

- The image is built automatically on first use.
- The repo is bind-mounted at the **same absolute path**, and the working directory is
  carried over, so paths in compiler output match the host.
- Runs as your uid/gid (a passwd entry named `dev` is added on the fly), `HOME=/cache/home`.
- `--network host`: the test sshd containers are reachable at `127.0.0.1:2201-2203`,
  and the Vite dev server (port 1420) is reachable from the host.
- Named volumes: `patchbay-dev-cargo-registry`, `patchbay-dev-cargo-git`, `patchbay-dev-npm`,
  `patchbay-dev-home`, `patchbay-dev-xwin` (MSVC CRT + Windows SDK for cargo-xwin, ~1.1 GB once
  used), and `patchbay-dev-target` mounted as `CARGO_TARGET_DIR=/cache/target`
  (container builds never touch the host's `./target`). Container build artifacts live
  in that volume: e.g. `scripts/dev.sh ls /cache/target/debug`.
- `node_modules/` lives in the repo and is shared with the host (both are linux-x64-gnu).
- Passed through when set: `RUST_LOG`, `RUST_BACKTRACE`, `PATCHBAY_DOCKER_TESTS`, `CARGO_TERM_COLOR`.

Remove all caches with `docker volume rm patchbay-dev-{cargo-registry,cargo-git,target,npm,home,xwin}`.

## Windows cross builds

The image can build the Windows x64 app (`x86_64-pc-windows-msvc`) and its NSIS installer
without a Windows machine, following Tauri's
[cross-platform compilation](https://v2.tauri.app/distribute/windows-installer/#build-windows-apps-on-linux-and-macos)
recipe: `cargo-xwin` (downloads the MSVC CRT and Windows SDK into the `patchbay-dev-xwin`
volume on first use; their license is accepted on your behalf), clang/lld/llvm-rc and
`makensis`. Use a separate target directory so the build doesn't wait for (or invalidate)
Linux builds:

```sh
# Release exe + NSIS installer (no updater signatures without TAURI_SIGNING_PRIVATE_KEY):
scripts/dev.sh bash -c 'CARGO_TARGET_DIR=/cache/target/windows npm run tauri build -- \
    --runner cargo-xwin --target x86_64-pc-windows-msvc --bundles nsis \
    --config "{\"bundle\":{\"createUpdaterArtifacts\":false}}"'
# -> /cache/target/windows/x86_64-pc-windows-msvc/release/{patchbay.exe,bundle/nsis/Patchbay_<ver>_x64-setup.exe}

# Lints and test executables for Windows (run the .exe files on a Windows machine):
scripts/dev.sh bash -c 'CARGO_TARGET_DIR=/cache/target/windows cargo xwin clippy \
    --target x86_64-pc-windows-msvc --workspace --all-targets -- -D warnings'
scripts/dev.sh bash -c 'CARGO_TARGET_DIR=/cache/target/windows cargo xwin test --no-run \
    --target x86_64-pc-windows-msvc -p patchbay-core --tests'
```

Copy artifacts out of the volume with e.g.
`docker run --rm -v patchbay-dev-target:/t -v "$PWD":/o --entrypoint cp patchbay-dev /t/windows/x86_64-pc-windows-msvc/release/patchbay.exe /o/`.

Notes:

- Only the NSIS installer can be built on Linux; `.msi` (WiX) needs Windows. Builds are
  unsigned (see [releasing.md](releasing.md) for signing in CI).
- Debian bookworm ships NSIS 3.08; Tauri's installer template also needs
  `Include/Win/RestartManager.nsh` from NSIS 3.09+, which the Dockerfile adds (checksummed).
- The test executables have the Linux path of the repo compiled in
  (`CARGO_MANIFEST_DIR`): for the Docker integration tests copy `docker/test` to
  `C:\home\<you>\...\docker\test` (same path, on the drive the tests run from), run them with
  `PATCHBAY_DOCKER_TESTS=1`, and make `127.0.0.1:2201-2203` on the Windows machine reach the
  test containers (e.g. `ssh -N -R 2201:127.0.0.1:2201 -R 2202:... -R 2203:... <windows-host>`).

## scripts/gui.sh - headless GUI session

`scripts/gui.sh start` launches a long-lived container (`patchbay-gui`) running
Xvfb `:99` (1440x900x24) + openbox + a D-Bus session bus + gnome-keyring (Secret Service,
login keyring unlocked with password `patchbay`) + the AT-SPI bus, then the given command
(default `npm run tauri dev`) in the background. Everything else talks to that container.

```sh
scripts/dev.sh npm ci                       # once
scripts/gui.sh start                        # npm run tauri dev (first compile takes minutes)
scripts/gui.sh wait-window Patchbay 900        # blocks until a window titled /Patchbay/ is visible; prints its id
scripts/gui.sh screenshot shot.png          # whole display -> shot.png (any host path)
scripts/gui.sh xdotool mousemove 400 300 click 1
scripts/gui.sh xdotool type --delay 30 'hello'
scripts/gui.sh xdotool key ctrl+t Return
scripts/gui.sh xdotool search --name Patchbay  # window ids
scripts/gui.sh logs -f                      # app/cargo/vite output
scripts/gui.sh status                       # running? exit status? visible windows
scripts/gui.sh exec secret-tool search --all service patchbay   # any command inside the session
scripts/gui.sh shell
scripts/gui.sh stop
```

Other commands: `scripts/gui.sh start <cmd...>` (e.g. a prebuilt binary:
`scripts/gui.sh start /cache/target/debug/patchbay`), or just `scripts/gui.sh <cmd...>`.

Tips for automated agents:

- After `start`, always `wait-window` before interacting; it fails early if the command exits
  (then read `scripts/gui.sh logs`).
- Coordinates are absolute screen pixels on the 1440x900 display; take a screenshot to
  locate things. For window-relative clicks use
  `scripts/gui.sh xdotool mousemove --window <id> X Y click 1`.
- openbox is running, so `xdotool windowactivate --sync <id>` and keyboard focus behave
  like a normal desktop. Typing goes to the focused widget - click the input first.
- Screenshot without an argument goes to `.cache/screenshots/gui-<timestamp>.png` (gitignored).
- The command's exit status is in `status`; the container stays up after the app exits so
  you can still screenshot/inspect. `stop` removes it.

Environment knobs: `PATCHBAY_GUI_NAME` (container name, default `patchbay-gui`),
`PATCHBAY_GUI_DISPLAY` (default `:99`), `PATCHBAY_GUI_SCREEN` (default `1440x900x24`),
`PATCHBAY_KEYRING_PASSWORD` (default `patchbay`), `PATCHBAY_GUI_WM` (`openbox` or `none`).
Run several sessions side by side by giving each its own `PATCHBAY_GUI_NAME` (they share
the host network, so only one can run `tauri dev` on port 1420 at a time).

WebKitGTK runs with software rendering (`LIBGL_ALWAYS_SOFTWARE=1`,
`WEBKIT_DISABLE_DMABUF_RENDERER=1`, `WEBKIT_DISABLE_COMPOSITING_MODE=1`, set in the image).
