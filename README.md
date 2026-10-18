<p align="center"><img src="assets/logo.svg" width="128" alt="Patchbay logo"></p>

<h1 align="center">Patchbay</h1>

<p align="center">A friendly, cross-platform GUI for working with machines over SSH.</p>

Patchbay is an open-source desktop app (Windows, macOS, Linux) for browsing and transferring
files, forwarding ports and opening terminals on your machines over SSH. It is built with
[Tauri 2](https://tauri.app), Rust ([`russh`](https://github.com/Eugeny/russh)) and Svelte 5.
Nothing is installed on the remote side.

**Status: pre-release.** Hosts, files, terminals and port forwarding all work and are
covered by unit and Docker integration tests, but there has been no public release yet and
things may still change. See [DESIGN.md](DESIGN.md) for the design decisions and milestones.

## Features

- **Hosts**: your `~/.ssh/config` is the host list (patterns, `Include`, `ProxyJump`,
  forwards, ...), so `ssh` and Patchbay always agree. Adding, editing and deleting hosts
  changes just that host's lines and keeps your comments and formatting; hosts Patchbay
  can't safely edit (`Host a b`, read-only files) say why. Groups and tags for every host.
  One shared connection per host with keepalive and auto-reconnect.
- **Tailscale**: devices on your tailnet are listed automatically (when Tailscale is
  installed), with online state; Patchbay asks once which user to log in as and remembers
  it. Tailscale SSH works, including "check" mode's approval link, and its advertised host
  keys are trusted.
- **Auth**: key files (+ passphrase), SSH agent (Unix socket, Windows OpenSSH, Pageant),
  password, keyboard-interactive / 2FA; secrets optionally stored in the OS keychain.
- **Host keys**: shares `~/.ssh/known_hosts` (hashed entries supported), fingerprint prompt
  for new hosts, hard warning for changed keys. Multi-hop ProxyJump bastions.
- **Files**: tabbed SFTP browser with side-by-side panes, drag & drop, transfer queue with
  pause/resume, conflict handling, permissions, previews, search, edit-in-place with
  auto-upload, remote-to-remote copy. Works on SFTP-only servers.
- **Terminal**: xterm.js tabs and splits for remote and local shells, "open terminal here",
  snippets.
- **Port forwarding**: local, remote, dynamic (SOCKS5) and machine-to-machine, saved as
  named rules with auto-start and auto-reconnect; keeps running in the system tray.
- **Session restore**, auto-updates with signed manifests.

## Repository layout

```
crates/patchbay-core   SSH/SFTP/forwarding/transfers (no Tauri dependency)
src-tauri/          Tauri app (commands/events, tray, updater)
ui/                 Svelte 5 + TypeScript frontend
docker/dev/         Linux dev container (Tauri deps + headless GUI tooling)
docker/test/        Throwaway OpenSSH servers for integration tests
scripts/            dev.sh / gui.sh helpers for the dev container
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
keyring, logs, ...).

### Integration tests

Throwaway OpenSSH servers (plain target, bastion, SFTP-only chroot) with test-only keys:

```sh
docker compose -f docker/test/compose.yml up -d --build --wait
PATCHBAY_DOCKER_TESTS=1 cargo test -p patchbay-core --tests
docker compose -f docker/test/compose.yml down
```

Addresses, users, keys and sample files are documented in
[docker/test/README.md](docker/test/README.md).

## CI and releases

- `.github/workflows/ci.yml`: on every push and PR, on Linux/macOS/Windows: `npm run check`,
  `npm run build`, `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`; plus an
  integration job against the Docker SSH servers.
- `.github/workflows/release.yml`: pushing a `v*` tag builds `.msi`/NSIS `.exe`, `.dmg`/`.app`
  (Apple Silicon and Intel), `.AppImage`/`.deb`/`.rpm`, and uploads them with the updater's
  `latest.json` to a **draft** GitHub release.

Signing is optional and driven by repository secrets; without them builds are unsigned and
the updater manifest is skipped:

| Purpose | Secrets |
|---|---|
| Updater signatures | `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` |
| macOS signing + notarization | `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` |
| Windows signing (optional hook) | `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_TENANT_ID` + variables `TRUSTED_SIGNING_*` |

The updater public key is already in `src-tauri/tauri.conf.json` (`plugins.updater.pubkey`);
put the matching private key + password in the secrets above. To rotate it, generate a new
pair with `npm run tauri signer generate -- -w ~/.tauri/patchbay.key` and replace the public key
(existing installs then can't verify updates signed with the new key). Details: [docs/releasing.md](docs/releasing.md).

## License

Copyright (C) the Patchbay contributors.

Patchbay is free software: you can redistribute it and/or modify it under the terms of the
GNU General Public License as published by the Free Software Foundation, either version 3
of the License, or (at your option) any later version. See [LICENSE](LICENSE).
