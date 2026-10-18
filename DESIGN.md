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

- Host list = `~/.ssh/config` and its includes (live-reloaded; supports `Host` patterns,
  `Include`, `HostName`, `User`, `Port`, `IdentityFile`, `ProxyJump`, `LocalForward`/`RemoteForward`/`DynamicForward`),
  plus devices on the user's tailnet. There is no separate Patchbay host store: a host's name is its
  ssh alias (`cfg:<alias>` ids; renames are remembered so tabs, forwards and snippets follow).
- Patchbay edits the config in place, with line-level edits: only the `Host` line and the
  `HostName`/`User`/`Port`/`IdentityFile`/`ProxyJump` lines of the host's own block change (keeping
  their indentation and spelling); comments, other options and other blocks are untouched. Writes are
  atomic, follow symlinks (dotfile repos) and keep permissions. A host is editable when its first
  `Host` line names only it, isn't in a file included from inside a `Host`/`Match` block, and the file
  can be written; otherwise it is read-only and says why. New hosts go before the first block with
  patterns (`Host *`, `Host *.lan`, `Match`), since in ssh the first value wins. Values in a host's
  block that an earlier block overrides are pointed out in the editor. Renames update `ProxyJump`
  references in the files Patchbay can write.
- If `~/.ssh/config` can't be written (e.g. managed by Nix), new hosts go to `~/.ssh/patchbay.conf`,
  which Patchbay reads as if it were included at the top. Patchbay 0.1 kept all its hosts there
  (with an optional `Include` line); on load they are moved into `~/.ssh/config` and the file is kept
  as `patchbay.conf.bak`.
- Groups and tags, which ssh_config has no place for, are kept for every config host in the app
  config dir (by alias).
- Tailscale: `tailscale status --json` (polled) lists the tailnet's devices (`ts:<stable node id>`),
  except ones a config host already points at. Their name goes through ssh_config (`Host *.ts.net`
  rules apply); the user comes from ssh_config, else the last one that worked, else the user is asked.
  Connections go to the Tailscale IP, host keys are checked under the MagicDNS name, and keys
  advertised by Tailscale SSH are accepted without a prompt. Login banners are shown (Tailscale SSH
  "check" mode sends an approval link) and extend the authentication timeout.
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
