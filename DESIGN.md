# Patchbay — design decisions

A cross-platform (Windows / macOS / Linux) GUI for working with machines over SSH:
browse and transfer files, forward ports, and open terminals when needed.
Open source, GPL-3.0.

## Stack

| Layer | Choice |
|---|---|
| Shell | Tauri 2 |
| Frontend | Svelte 5 + TypeScript + Vite |
| SSH | `russh` + `russh-sftp` (pure Rust, tokio) |
| Terminal | xterm.js (fit + webgl addons); local shells via `portable-pty` |
| Secrets | OS keychain via `keyring`, opt-in per secret |
| Look | Modern minimal: host sidebar + tabbed content area, light/dark follows OS |

## Workspace layout

```
crates/patchbay-core   SSH/SFTP/forwarding/transfers. No Tauri dependency, so it can
                    later be hosted by a headless daemon without rewrites.
src-tauri/          Tauri app: commands/events bridging core <-> UI, tray, updater.
ui/                 Svelte frontend.
docker/             OpenSSH test containers (target, bastion, SFTP-only, Windows-like paths where feasible).
```

## Hosts & connections

- Host list = `~/.ssh/config` (read-only, live-reloaded; supports `Host` patterns,
  `Include`, `HostName`, `User`, `Port`, `IdentityFile`, `ProxyJump`, `LocalForward`/`RemoteForward`/`DynamicForward`)
  **plus** app-managed hosts stored in the app config dir. Each host can be grouped and tagged.
- One SSH connection per host, shared by all channels (SFTP, shells, forwards). Keepalive + auto-reconnect.
- Auth: key files (+ passphrase), SSH agent (Unix socket, Windows OpenSSH named pipe, Pageant),
  password, keyboard-interactive (2FA/OTP prompts surfaced in the UI).
- ProxyJump / multi-hop bastions via `direct-tcpip` channel streams.
- Host keys: shared `~/.ssh/known_hosts` (read + append, hashed entries supported).
  Fingerprint dialog for unknown hosts; blocking warning for changed keys.
- Agentless only: nothing is installed on remotes.
- Remote capability detection on connect: POSIX shell / Windows shell / SFTP-only.
  Exec-based features (search, "terminal here", disk usage) degrade gracefully.

## Files

- Tabbed browser; each tab is a local or remote location. Two tabs can be shown side by side.
- Drag & drop between panes and from the OS into the app. Drag-out to the OS: local entries directly, remote ones
  via a background copy into a temp dir started when the drag begins (capped at 200 MiB).
- Transfer queue: progress, pause/cancel, parallelism, resume of interrupted transfers.
- Remote-to-remote copy streamed through the app.
- Conflicts: dialog with Overwrite / Skip / Rename / Overwrite-if-newer + "apply to all".
- Permissions (chmod/chown), image/text preview, search (remote `find` when a shell exists, SFTP walk otherwise).
- Edit remotely: download to temp, open in system editor, watch and auto-upload on save (conflict check against
  the remote mtime/size first; unsynced copies survive restarts).

## Terminal

- Tabs + split panes, remote and local shells.
- "Open terminal here" from the file browser.
- Snippets: saved commands, global or per host.

## Port forwarding

- Local (-L), remote (-R), dynamic/SOCKS5 (-D), and machine-to-machine
  (e.g. remote-forward on host A bridged to a `direct-tcpip` channel on host B, through the app).
- Saved, named rules toggled on/off, optional auto-start, auto-reconnect.
- App lives in the system tray: closing the window keeps sessions and forwards alive. Optional launch at login.

## Session restore

- Default: restore previous tabs, connect lazily when a tab is focused.
- Setting: reconnect everything eagerly on launch.

## Testing

- Unit tests in `patchbay-core`.
- Integration tests against Docker OpenSSH containers (incl. bastion + SFTP-only chroot).
- Manual testing against real hosts.

## CI / release (set up from M1)

- GitHub Actions matrix (Linux / macOS / Windows): fmt, clippy, tests, build.
- `tauri-action` release workflow: `.msi`/`.exe`, `.dmg`, `.AppImage`/`.deb`/`.rpm`.
- Tauri updater with signed update manifests.
- Code signing (Apple Developer ID + notarization, Windows signing) wired via secrets; builds stay unsigned until those secrets exist.

## Milestones

1. **Hosts**: workspace scaffold, CI + release pipeline, ssh_config parsing, host list UI, connect/auth/known_hosts/ProxyJump.
2. **Files**: SFTP browser, tabs/split view, transfer queue, conflicts, permissions, preview, search.
3. **Terminal**: xterm.js remote + local shells, tabs/splits, "terminal here", snippets.
4. **Forwards**: all forward types, saved rules, tray, autostart, auto-reconnect.
5. **Polish**: remote edit, remote-to-remote, drag-out, session restore settings, docs.
