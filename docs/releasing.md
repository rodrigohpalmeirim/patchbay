# Releasing Patchbay

Releases are built by `.github/workflows/release.yml` with
[`tauri-apps/tauri-action`](https://github.com/tauri-apps/tauri-action) (pinned to `v1.0.0`).

## Cutting a release

1. Bump the version in `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` / workspace
   `Cargo.toml`, and `package.json` (keep them equal).
2. Commit, then tag and push: `git tag v0.2.0 && git push origin v0.2.0`.
3. The workflow creates (or reuses) a **draft** release for the tag, builds all platforms in
   parallel and uploads the bundles, the `.sig` updater signatures and `latest.json`.
4. Check the draft on GitHub, edit the notes, and **publish** it. The updater endpoint
   (`https://github.com/<owner>/patchbay/releases/latest/download/latest.json`) only sees
   published, non-prerelease releases.

Dry run: *Actions -> Release -> Run workflow* with an empty tag builds everything and
uploads the bundles as workflow artifacts only. With a tag, it creates the draft release
(and the tag at the current commit, if it does not exist yet).

## Build matrix

| Runner          | Target                       | Bundles               |
|-----------------|------------------------------|-----------------------|
| `macos-latest`  | `aarch64-apple-darwin`       | `.app` (+ `.app.tar.gz` for the updater), `.dmg` |
| `macos-latest`  | `x86_64-apple-darwin`        | same                  |
| `ubuntu-22.04`  | `x86_64-unknown-linux-gnu`   | `.AppImage`, `.deb`, `.rpm` |
| `windows-latest`| `x86_64-pc-windows-msvc`     | `.msi`, NSIS `-setup.exe` |

- **macOS: two per-arch builds instead of `universal-apple-darwin`.** Each download is half
  the size, the builds run in parallel, a failure on one arch doesn't block the other, and
  `latest.json` gets native `darwin-aarch64` / `darwin-x86_64` entries. The cost is two
  notarizations per release and two DMGs on the release page.
- **Linux on Ubuntu 22.04**: binaries link against the oldest glibc available on GitHub's
  runners, so they run on more distributions than a 24.04 build would.
- **Windows updater** uses the NSIS installer (`updaterJsonPreferNsis: true`).

## Secrets

Everything is optional: without secrets you get unsigned builds (macOS ad-hoc signed)
and **no updater artifacts**. The workflow only exports a variable when its secret is
non-empty, so missing secrets never break the build.

### Updater signing (all platforms)

| Secret | Value |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | contents of the private key file (or its path; contents recommended) |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | its password (may be empty) |

Generate a key pair once:

```sh
npm run tauri signer generate -- -w ~/.tauri/patchbay.key
```

Put the **public** key (`~/.tauri/patchbay.key.pub` contents) in
`src-tauri/tauri.conf.json` -> `plugins.updater.pubkey`, and make sure
`plugins.updater.endpoints` points at
`https://github.com/<owner>/<repo>/releases/latest/download/latest.json`.
Store the private key and password as repository secrets and keep a backup: losing the key
means existing installs can't verify future updates.

If `TAURI_SIGNING_PRIVATE_KEY` is missing, the workflow passes
`--config {"bundle":{"createUpdaterArtifacts":false}}` so the build still succeeds; no
`latest.json` is produced.

### macOS: Developer ID signing + notarization

| Secret | Value |
|---|---|
| `APPLE_CERTIFICATE` | base64 of the exported Developer ID Application `.p12` (`base64 -i cert.p12 \| pbcopy`) |
| `APPLE_CERTIFICATE_PASSWORD` | password used when exporting the `.p12` |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | Apple ID email used for notarization |
| `APPLE_PASSWORD` | an **app-specific password** for that Apple ID |
| `APPLE_TEAM_ID` | 10-character team id |

- Signing activates when `APPLE_CERTIFICATE` and `APPLE_CERTIFICATE_PASSWORD` are set;
  notarization additionally needs all of `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`.
- Without a certificate the workflow sets `APPLE_SIGNING_IDENTITY=-` (ad-hoc signature).
  Users then have to right-click -> Open (or `xattr -dr com.apple.quarantine Patchbay.app`).
- App Store Connect API keys (`APPLE_API_KEY`, `APPLE_API_ISSUER`, `APPLE_API_KEY_PATH`) are
  an alternative to Apple ID notarization; wire them into the "Configure signing" step the
  same way if preferred.

### Windows: code signing (hook, off by default)

Windows builds are unsigned (SmartScreen will warn). The workflow contains a ready hook for
[Azure Trusted Signing](https://v2.tauri.app/distribute/sign/windows/#azure-trusted-signing):

| Kind | Name |
|---|---|
| secret | `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_TENANT_ID` (service principal with the *Trusted Signing Certificate Profile Signer* role) |
| variable | `TRUSTED_SIGNING_ENDPOINT` (e.g. `https://weu.codesigning.azure.net`), `TRUSTED_SIGNING_ACCOUNT`, `TRUSTED_SIGNING_PROFILE` |

When all six are present, the workflow installs `trusted-signing-cli` and sets
`bundle.windows.signCommand` to
`trusted-signing-cli -e <endpoint> -a <account> -c <profile> -d Patchbay %1`, which Tauri runs for
every binary and installer it produces. For another signing service (e.g. a cloud HSM or
`signtool` with a certificate), replace that override in the "Configure signing" step with
your own `signCommand` (or `certificateThumbprint` + `timestampUrl`).
