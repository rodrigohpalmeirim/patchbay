# Contributing to Patchbay

Thanks for your interest! Bug reports, ideas and pull requests are all welcome. For bigger
changes, please open an issue first so we can agree on the approach.

## Repository layout

```
crates/patchbay-core   SSH/SFTP/forwarding/transfers (no Tauri dependency)
src-tauri/             Tauri app (commands/events, tray, updater)
ui/                    Svelte 5 + TypeScript frontend
docker/dev/            Linux dev container (Tauri deps + headless GUI tooling)
docker/test/           Throwaway OpenSSH servers for integration tests
scripts/               dev.sh / gui.sh helpers for the dev container
docs/                  Maintainer docs (dev container, releasing)
```

## Development

Requirements: Rust stable >= 1.90 (with a C compiler, `ring` is built from source),
Node.js 24, npm.

### Core crate (any host)

```sh
cargo test -p patchbay-core
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

### Desktop app

On macOS and Windows install the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)
and run:

```sh
npm ci
npm run tauri dev
```

On Linux you can install the WebKitGTK prerequisites, or use the dev container, which
needs nothing but Docker:

```sh
scripts/dev.sh npm ci
scripts/dev.sh npm run check
scripts/dev.sh cargo build -p patchbay
scripts/gui.sh start                 # npm run tauri dev on a virtual X display
scripts/gui.sh wait-window Patchbay 900
scripts/gui.sh screenshot shot.png
scripts/gui.sh stop
```

See [docs/dev-container.md](docs/dev-container.md) for all commands (xdotool input,
keyring, logs, Windows cross builds, ...).

### UI only, in a browser

`npm run dev` serves the frontend at http://localhost:1420. Outside Tauri it uses a mock
backend (`ui/lib/mock.ts`) with sample hosts, files and forwards, which is handy for UI
work and screenshots.

### Integration tests

Throwaway OpenSSH servers (plain target, bastion, SFTP-only chroot) with test-only keys:

```sh
docker compose -f docker/test/compose.yml up -d --build --wait
PATCHBAY_DOCKER_TESTS=1 cargo test -p patchbay-core --tests
docker compose -f docker/test/compose.yml down
```

Addresses, users, keys and sample files are documented in
[docker/test/README.md](docker/test/README.md).

## Before opening a pull request

CI runs these on Linux, macOS and Windows, so running them locally saves a round trip:

```sh
npm run check
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Releases

Maintainers: see [docs/releasing.md](docs/releasing.md).
