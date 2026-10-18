// File contents for the mock filesystem: generated text (code, configs, logs),
// binary files with realistic magic bytes, and real decodable images rendered
// on demand with a canvas (PNG/JPEG/WebP natively; GIF/BMP/ICO via the tiny
// encoders below). Everything is deterministic per seed and cached.

export type ImageFormat = "png" | "jpeg" | "webp" | "gif" | "bmp" | "ico";
export type LineKind = "nginx" | "syslog" | "app" | "auth" | "csv" | "postgres" | "build";

export type ContentSpec =
  | { t: "text"; text: string }
  /** Generated log-like lines, filling the node's listed size. */
  | { t: "lines"; kind: LineKind; seed: number }
  | { t: "binary"; magic: number[]; seed: number }
  | { t: "image"; fmt: ImageFormat; w: number; h: number; label: string; seed: number }
  | { t: "bytes"; data: Uint8Array };

// ------------------------------------------------------------------ random

/** mulberry32: small, fast, deterministic PRNG. */
export function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export function hash(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

const pick = <T>(r: () => number, xs: readonly T[]): T => xs[Math.floor(r() * xs.length)];
const enc = new TextEncoder();

// ------------------------------------------------------------- extensions

const TEXT_EXT = new Set([
  "txt", "md", "json", "yaml", "yml", "toml", "ts", "js", "mjs", "rs", "py", "sh", "bash", "zsh",
  "css", "html", "svelte", "conf", "cfg", "ini", "log", "csv", "sql", "xml", "plist", "env", "lock",
  "gitignore", "service", "properties", "ps1", "bat", "cmd", "go", "c", "h", "rb", "tf", "pub",
]);

export function extOf(name: string): string {
  const i = name.lastIndexOf(".");
  return i <= 0 ? "" : name.slice(i + 1).toLowerCase();
}

export function isTextName(name: string): boolean {
  const ext = extOf(name);
  if (TEXT_EXT.has(ext)) return true;
  // Dotfiles and extension-less config files are text.
  return ext === "" && !/^(ntuser|.*\.dat$)/i.test(name);
}

const MAGIC: Record<string, number[]> = {
  pdf: [...enc.encode("%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")],
  zip: [0x50, 0x4b, 0x03, 0x04, 0x14, 0x00, 0x00, 0x00, 0x08, 0x00],
  docx: [0x50, 0x4b, 0x03, 0x04, 0x14, 0x00, 0x06, 0x00, 0x08, 0x00],
  xlsx: [0x50, 0x4b, 0x03, 0x04, 0x14, 0x00, 0x06, 0x00, 0x08, 0x00],
  ipa: [0x50, 0x4b, 0x03, 0x04, 0x14, 0x00, 0x00, 0x00, 0x08, 0x00],
  jar: [0x50, 0x4b, 0x03, 0x04, 0x14, 0x00, 0x08, 0x08, 0x08, 0x00],
  gz: [0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03],
  tgz: [0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03],
  zst: [0x28, 0xb5, 0x2f, 0xfd, 0x24, 0x00],
  xz: [0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00],
  deb: [...enc.encode("!<arch>\ndebian-binary   1728000000  0     0     100644  4         `\n2.0\n")],
  iso: [0x45, 0x52, 0x08, 0x00, 0x00, 0x00, 0x90, 0x90],
  dmg: [0x78, 0x01, 0x73, 0x0d, 0x62, 0x62, 0x60],
  mp4: [0x00, 0x00, 0x00, 0x20, ...enc.encode("ftypisom"), 0x00, 0x00, 0x02, 0x00, ...enc.encode("isomiso2avc1mp41")],
  mkv: [0x1a, 0x45, 0xdf, 0xa3, 0x9f, 0x42, 0x86, 0x81, 0x01],
  webm: [0x1a, 0x45, 0xdf, 0xa3, 0x9f, 0x42, 0x86, 0x81, 0x01],
  mov: [0x00, 0x00, 0x00, 0x14, ...enc.encode("ftypqt  ")],
  mp3: [...enc.encode("ID3"), 0x04, 0x00, 0x00, 0x00, 0x00, 0x1f, 0x76],
  flac: [...enc.encode("fLaC"), 0x00, 0x00, 0x00, 0x22],
  exe: [0x4d, 0x5a, 0x90, 0x00, 0x03, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0xff, 0xff],
  dll: [0x4d, 0x5a, 0x90, 0x00, 0x03, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0xff, 0xff],
  msi: [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1],
  dat: [...enc.encode("regf"), 0x9a, 0x1c, 0x00, 0x00, 0x9a, 0x1c, 0x00, 0x00],
  sqlite: [...enc.encode("SQLite format 3\0")],
  db: [...enc.encode("SQLite format 3\0")],
  wasm: [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00],
  psd: [...enc.encode("8BPS"), 0x00, 0x01],
  part: [0x00, 0x00, 0x00, 0x20, ...enc.encode("ftypisom")],
};

/** ELF header for extension-less executables. */
export const ELF = [
  0x7f, 0x45, 0x4c, 0x46, 0x02, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03,
  0x00, 0x3e, 0x00, 0x01, 0x00, 0x00, 0x00,
];

export function magicFor(name: string): number[] {
  return MAGIC[extOf(name)] ?? [0x00, 0x01, 0x02, 0x03];
}

const IMAGE_EXT: Record<string, ImageFormat> = {
  png: "png",
  jpg: "jpeg",
  jpeg: "jpeg",
  webp: "webp",
  gif: "gif",
  bmp: "bmp",
  ico: "ico",
};

export function imageFormatOf(name: string): ImageFormat | null {
  return IMAGE_EXT[extOf(name)] ?? null;
}

// --------------------------------------------------------- text templates

const WORDS =
  "deploy release backup config metrics cluster ingress worker queue cache schema migration index replica shard upstream gateway session token handler router service volume snapshot pipeline artifact".split(
    " ",
  );

function title(name: string): string {
  return name.replace(/\.[^.]+$/, "").replace(/[-_]+/g, " ").replace(/^\w/, (c) => c.toUpperCase());
}

/** Plausible text for a file, chosen by name/extension. */
export function textFor(name: string, seed = hash(name)): string {
  const r = rng(seed);
  const ext = extOf(name);
  const base = name.toLowerCase();
  const known = KNOWN[base];
  if (known) return known;
  switch (ext) {
    case "md": {
      const t = title(name);
      const items = Array.from({ length: 4 + Math.floor(r() * 5) }, () => `- [${r() > 0.5 ? "x" : " "}] ${pick(r, WORDS)} ${pick(r, WORDS)} ${pick(r, WORDS)}`);
      return `# ${t}\n\n_Last updated ${new Date(Date.now() - r() * 9e9).toISOString().slice(0, 10)}_\n\n## Summary\n\nNotes about the ${pick(r, WORDS)} ${pick(r, WORDS)} rollout and what is left to do before the next ${pick(r, WORDS)} window.\n\n## Checklist\n\n${items.join("\n")}\n\n## Commands\n\n\`\`\`sh\nssh ${pick(r, ["bastion", "prod-web-01", "db-internal"])} 'sudo systemctl status ${pick(r, WORDS)}'\n\`\`\`\n\n> Remember to update the runbook afterwards.\n`;
    }
    case "json":
      return JSON.stringify(
        {
          name: base.replace(/\.json$/, ""),
          version: `${1 + Math.floor(r() * 3)}.${Math.floor(r() * 12)}.${Math.floor(r() * 30)}`,
          enabled: r() > 0.3,
          retries: Math.floor(r() * 5),
          endpoints: Array.from({ length: 3 }, (_, i) => ({ id: i + 1, url: `https://${pick(r, WORDS)}.example.com/v${i + 1}`, timeoutMs: 500 + Math.floor(r() * 4500) })),
          tags: [pick(r, WORDS), pick(r, WORDS)],
        },
        null,
        2,
      ) + "\n";
    case "yaml":
    case "yml":
      return `# ${title(name)}\nversion: "3.9"\nservices:\n  app:\n    image: ghcr.io/example/${pick(r, WORDS)}:${Math.floor(r() * 40)}\n    restart: unless-stopped\n    ports:\n      - "8080:8080"\n    environment:\n      RUST_LOG: info\n      DATABASE_URL: postgres://app@db/app\n    volumes:\n      - ./data:/var/lib/app\n  db:\n    image: postgres:16\n    volumes:\n      - pgdata:/var/lib/postgresql/data\nvolumes:\n  pgdata: {}\n`;
    case "toml":
      return `[package]\nname = "${pick(r, WORDS)}"\nversion = "0.${Math.floor(r() * 9)}.0"\nedition = "2021"\n\n[dependencies]\nserde = { version = "1", features = ["derive"] }\ntokio = { version = "1", features = ["full"] }\nanyhow = "1"\n`;
    case "ts":
    case "js":
    case "mjs":
      return `// ${name}\nimport { readFile } from "node:fs/promises";\n\nexport interface ${title(name).replace(/\W/g, "")}Options {\n  retries: number;\n  timeoutMs: number;\n}\n\nconst DEFAULTS: ${title(name).replace(/\W/g, "")}Options = { retries: ${Math.floor(r() * 5)}, timeoutMs: ${1000 + Math.floor(r() * 9000)} };\n\nexport async function load(path: string, opts = DEFAULTS) {\n  for (let attempt = 0; attempt <= opts.retries; attempt++) {\n    try {\n      return JSON.parse(await readFile(path, "utf8"));\n    } catch (err) {\n      if (attempt === opts.retries) throw err;\n      await new Promise((r) => setTimeout(r, 2 ** attempt * 100));\n    }\n  }\n}\n`;
    case "rs":
      return `use anyhow::Result;\nuse std::time::Duration;\n\n/// ${title(name)} helpers.\npub struct ${title(name).replace(/\W/g, "")} {\n    retries: u32,\n    timeout: Duration,\n}\n\nimpl ${title(name).replace(/\W/g, "")} {\n    pub fn new() -> Self {\n        Self { retries: ${Math.floor(r() * 5)}, timeout: Duration::from_secs(${5 + Math.floor(r() * 25)}) }\n    }\n\n    pub async fn run(&self) -> Result<()> {\n        for attempt in 0..=self.retries {\n            tracing::info!(attempt, "running");\n            tokio::time::sleep(self.timeout / 10).await;\n        }\n        Ok(())\n    }\n}\n`;
    case "py":
      return `#!/usr/bin/env python3\n"""${title(name)}."""\nimport time\nfrom pathlib import Path\n\nOUT = Path.home() / "${pick(r, WORDS)}"\n\n\ndef main() -> None:\n    OUT.mkdir(exist_ok=True)\n    while True:\n        stamp = time.strftime("%Y-%m-%d-%H%M%S")\n        (OUT / f"{stamp}.txt").write_text(stamp)\n        time.sleep(${10 + Math.floor(r() * 50)})\n\n\nif __name__ == "__main__":\n    main()\n`;
    case "sh":
    case "bash":
      return `#!/usr/bin/env bash\n# ${title(name)}\nset -euo pipefail\n\nTARGET="\${1:-/var/backups}"\nSTAMP="$(date +%Y%m%d-%H%M%S)"\n\necho "==> ${pick(r, WORDS)} $STAMP"\nmkdir -p "$TARGET"\ntar --zstd -cf "$TARGET/${pick(r, WORDS)}-$STAMP.tar.zst" /etc /srv/app\nfind "$TARGET" -name '*.tar.zst' -mtime +${7 + Math.floor(r() * 23)} -delete\necho "done"\n`;
    case "ps1":
      return `# ${title(name)}\n$ErrorActionPreference = "Stop"\n$Out = Join-Path $env:USERPROFILE "builds"\nNew-Item -ItemType Directory -Force -Path $Out | Out-Null\ncargo build --release\nCopy-Item target\\release\\patchbay.exe $Out\nWrite-Host "Built to $Out"\n`;
    case "css":
      return `:root {\n  --bg: #0b0d12;\n  --fg: #e8eaf0;\n  --accent: #3b7cf6;\n}\n\nbody {\n  margin: 0;\n  font: 16px/1.6 system-ui, sans-serif;\n  background: var(--bg);\n  color: var(--fg);\n}\n\n.hero {\n  display: grid;\n  place-items: center;\n  min-height: 70vh;\n}\n\n.hero h1 {\n  font-size: clamp(2rem, 6vw, 4.5rem);\n  letter-spacing: -0.03em;\n}\n`;
    case "html":
      return `<!doctype html>\n<html lang="en">\n  <head>\n    <meta charset="utf-8" />\n    <meta name="viewport" content="width=device-width, initial-scale=1" />\n    <title>${title(name)}</title>\n    <link rel="stylesheet" href="/css/site.css" />\n  </head>\n  <body>\n    <main class="hero">\n      <h1>Ship files, not excuses.</h1>\n      <p>Built with care on ${new Date().toDateString()}.</p>\n    </main>\n    <script type="module" src="/js/app.js"></script>\n  </body>\n</html>\n`;
    case "svelte":
      return `<script lang="ts">\n  let count = $state(0);\n  const doubled = $derived(count * 2);\n</script>\n\n<button onclick={() => count++}>\n  Clicked {count} times ({doubled})\n</button>\n\n<style>\n  button {\n    padding: 8px 14px;\n    border-radius: 8px;\n  }\n</style>\n`;
    case "conf":
    case "cfg":
    case "ini":
      return `# ${name}\n[main]\nenabled = true\nlisten = 0.0.0.0:${1000 + Math.floor(r() * 9000)}\nworkers = ${1 + Math.floor(r() * 16)}\nlog_level = info\n\n[storage]\npath = /var/lib/${pick(r, WORDS)}\nmax_size = ${Math.floor(r() * 500)}G\n`;
    case "sql":
      return `-- ${name}\nBEGIN;\nCREATE TABLE IF NOT EXISTS ${pick(r, WORDS)}s (\n  id bigserial PRIMARY KEY,\n  name text NOT NULL,\n  created_at timestamptz NOT NULL DEFAULT now()\n);\nCREATE INDEX ON ${pick(r, WORDS)}s (created_at);\nCOMMIT;\n`;
    case "pub":
      return `ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI${b64ish(r, 43)} ${pick(r, ["rodrigo@laptop", "ops@bastion", "ci@build-mac"])}\n`;
    case "csv":
      return "timestamp,sensor,value\n" + Array.from({ length: 40 }, (_, i) => `2026-10-${String(1 + (i % 16)).padStart(2, "0")}T${String(i % 24).padStart(2, "0")}:00:00Z,${pick(r, ["cpu", "gpu", "ambient"])},${(30 + r() * 30).toFixed(1)}`).join("\n") + "\n";
    case "txt":
    case "":
    default: {
      const lines = Array.from({ length: 6 + Math.floor(r() * 12) }, () => `${pick(r, WORDS)} ${pick(r, WORDS)} — ${pick(r, WORDS)} ${Math.floor(r() * 100)}`);
      return `${title(name)}\n${"=".repeat(Math.min(40, title(name).length))}\n\n${lines.join("\n")}\n`;
    }
  }
}

function b64ish(r: () => number, n: number): string {
  const a = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  return Array.from({ length: n }, () => pick(r, [...a])).join("");
}

const KNOWN: Record<string, string> = {
  ".bashrc": `# ~/.bashrc: executed by bash(1) for non-login shells.\ncase $- in\n    *i*) ;;\n      *) return;;\nesac\n\nHISTCONTROL=ignoreboth\nHISTSIZE=10000\nHISTFILESIZE=20000\nshopt -s histappend checkwinsize\n\nexport EDITOR=nvim\nexport PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"\n\nalias ll='ls -alF --color=auto'\nalias gs='git status -sb'\nalias k='kubectl'\n\nif [ -f ~/.bash_aliases ]; then\n    . ~/.bash_aliases\nfi\n\neval "$(starship init bash)"\n`,
  ".profile": `# ~/.profile: executed by the command interpreter for login shells.\nif [ -n "$BASH_VERSION" ]; then\n    if [ -f "$HOME/.bashrc" ]; then\n        . "$HOME/.bashrc"\n    fi\nfi\n\nif [ -d "$HOME/bin" ] ; then\n    PATH="$HOME/bin:$PATH"\nfi\n`,
  ".zshrc": `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$PATH"\nexport LANG=en_US.UTF-8\nautoload -Uz compinit && compinit\nalias ll='ls -lah'\nalias xcb='xcodebuild -scheme Patchbay -configuration Release'\n`,
  ".gitconfig": `[user]\n\tname = Rodrigo\n\temail = rodrigo@example.com\n[init]\n\tdefaultBranch = main\n[pull]\n\trebase = true\n[alias]\n\tlg = log --graph --oneline --decorate\n\tst = status -sb\n[core]\n\teditor = nvim\n`,
  ".gitignore": `/target\n/node_modules\n/dist\n*.log\n.DS_Store\n.env\n`,
  config: `Host bastion\n    HostName bastion.example.net\n    User ops\n    IdentityFile ~/.ssh/id_ed25519\n\nHost db-internal\n    HostName 10.0.4.12\n    User postgres\n    ProxyJump bastion\n\nHost *\n    ServerAliveInterval 30\n    AddKeysToAgent yes\n`,
  id_ed25519: `-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW\nQyNTUxOQAAACBmZXJyeS1tb2NrLWtleS1ub3QtYS1yZWFsLWtleS0tLS0tAAAAoMp4Zk3K\neGZNAAAAC3NzaC1lZDI1NTE5AAAAIGZlcnJ5LW1vY2sta2V5LW5vdC1hLXJlYWwta2V5LS\n0tLS0AAAAEDl0bW9jay1wcml2YXRlLWtleS1kYXRhLWZvci1kZW1vLW9ubHktLS0tLS0tLS\n-----END OPENSSH PRIVATE KEY-----\n`,
  known_hosts: `|1|K9r2Cb0x1m3dY2xh0k=|aGVsbG8td29ybGQtbW9jaw== ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOq7Xv4mJ0r2nLkA9cWbE5tZ1yHf3uPd8sGo6iKe2RjN\nbastion.example.net ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBq9xLm1VcR7sNw2YkPa5eHu8gDf4jTo0iKb6QvXcZt3\n[nas.lan]:2222 ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQC7mock\n`,
  authorized_keys: `ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGZlcnJ5LW1vY2sta2V5LW5vdC1hLXJlYWwta2V5 rodrigo@laptop\n`,
  "readme.md": `# Patchbay\n\nA friendly, cross-platform GUI for SSH: browse and transfer files, forward ports\nand open terminals on your machines.\n\n## Features\n\n- Host list from \`~/.ssh/config\` plus app-managed hosts\n- SFTP file browser with split view and drag & drop\n- Transfer queue with pause, resume and conflict handling\n- Port forwarding (local, remote, dynamic)\n\n## Development\n\n\`\`\`sh\nnpm install\nnpm run tauri dev\n\`\`\`\n\n## License\n\nGPL-3.0-or-later\n`,
  "cargo.toml": `[workspace]\nmembers = ["crates/patchbay-core", "src-tauri"]\nresolver = "2"\n\n[workspace.package]\nversion = "0.1.0"\nedition = "2021"\nlicense = "GPL-3.0-or-later"\n\n[workspace.dependencies]\nrussh = "0.45"\nrussh-sftp = "2"\ntokio = { version = "1", features = ["full"] }\nserde = { version = "1", features = ["derive"] }\nthiserror = "1"\n`,
  "package.json": `{\n  "name": "patchbay",\n  "private": true,\n  "version": "0.1.0",\n  "type": "module",\n  "scripts": {\n    "dev": "vite",\n    "build": "vite build",\n    "check": "svelte-check --tsconfig ./tsconfig.json"\n  },\n  "dependencies": {\n    "@tauri-apps/api": "^2.1.0"\n  },\n  "devDependencies": {\n    "svelte": "^5.0.0",\n    "vite": "^6.0.0"\n  }\n}\n`,
  "main.rs": `// Prevents an extra console window on Windows in release.\n#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]\n\nfn main() {\n    tracing_subscriber::fmt()\n        .with_env_filter("patchbay=debug,russh=info")\n        .init();\n    patchbay_lib::run();\n}\n`,
  "lib.rs": `mod commands;\nmod events;\n\nuse patchbay_core::Core;\nuse tauri::Manager;\n\n#[cfg_attr(mobile, tauri::mobile_entry_point)]\npub fn run() {\n    tauri::Builder::default()\n        .plugin(tauri_plugin_dialog::init())\n        .plugin(tauri_plugin_opener::init())\n        .setup(|app| {\n            let core = Core::new(app.path().app_config_dir()?)?;\n            events::forward(app.handle().clone(), core.subscribe());\n            app.manage(core);\n            Ok(())\n        })\n        .invoke_handler(tauri::generate_handler![\n            commands::list_hosts,\n            commands::connect,\n            commands::fs_list,\n            commands::transfer_start,\n        ])\n        .run(tauri::generate_context!())\n        .expect("error while running Patchbay");\n}\n`,
  hosts: `127.0.0.1\tlocalhost\n127.0.1.1\tlaptop\n\n# The following lines are desirable for IPv6 capable hosts\n::1     ip6-localhost ip6-loopback\nff02::1 ip6-allnodes\nff02::2 ip6-allrouters\n\n10.0.4.12   db-internal\n192.168.1.40 raspberrypi\n`,
  "nginx.conf": `user www-data;\nworker_processes auto;\npid /run/nginx.pid;\n\nevents {\n    worker_connections 1024;\n}\n\nhttp {\n    sendfile on;\n    tcp_nopush on;\n    types_hash_max_size 2048;\n    include /etc/nginx/mime.types;\n    default_type application/octet-stream;\n\n    ssl_protocols TLSv1.2 TLSv1.3;\n    access_log /var/log/nginx/access.log;\n    error_log /var/log/nginx/error.log;\n\n    gzip on;\n    include /etc/nginx/conf.d/*.conf;\n    include /etc/nginx/sites-enabled/*;\n}\n`,
  default: `server {\n    listen 80 default_server;\n    listen [::]:80 default_server;\n    server_name _;\n    return 301 https://$host$request_uri;\n}\n\nserver {\n    listen 443 ssl http2;\n    server_name example.com www.example.com;\n    root /var/www/site;\n    index index.html;\n\n    ssl_certificate     /etc/letsencrypt/live/example.com/fullchain.pem;\n    ssl_certificate_key /etc/letsencrypt/live/example.com/privkey.pem;\n\n    location / {\n        try_files $uri $uri/ =404;\n    }\n\n    location /api/ {\n        proxy_pass http://127.0.0.1:8080;\n        proxy_set_header Host $host;\n    }\n}\n`,
  sshd_config: `Include /etc/ssh/sshd_config.d/*.conf\nPort 22\nPermitRootLogin no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM yes\nX11Forwarding no\nPrintMotd no\nAcceptEnv LANG LC_*\nSubsystem sftp /usr/lib/openssh/sftp-server\n`,
  "postgresql.conf": `listen_addresses = '10.0.4.12'\nport = 5432\nmax_connections = 200\nshared_buffers = 4GB\neffective_cache_size = 12GB\nwork_mem = 32MB\nwal_level = replica\nmax_wal_size = 4GB\narchive_mode = on\narchive_command = 'test ! -f /var/lib/postgresql/wal-archive/%f && cp %p /var/lib/postgresql/wal-archive/%f'\nlog_min_duration_statement = 250ms\n`,
  "pg_hba.conf": `# TYPE  DATABASE        USER            ADDRESS                 METHOD\nlocal   all             postgres                                peer\nlocal   all             all                                     peer\nhost    all             all             10.0.0.0/16             scram-sha-256\nhost    replication     replicator      10.0.4.13/32            scram-sha-256\n`,
  "config.txt": `# Raspberry Pi config\ndtparam=audio=on\ncamera_auto_detect=1\ndisplay_auto_detect=1\ndtoverlay=vc4-kms-v3d\nmax_framebuffers=2\narm_64bit=1\n[pi4]\narm_boost=1\n`,
  "robots.txt": `User-agent: *\nAllow: /\nSitemap: https://example.com/sitemap.xml\n`,
  ".env": `DATABASE_URL=postgres://app:s3cr3t@10.0.4.12/app\nREDIS_URL=redis://127.0.0.1:6379/0\nSECRET_KEY_BASE=8f2c1e0b5a7d4c3e9f6a2b1d0c8e7f6a\nRUST_LOG=info\n`,
  "日本語のメモ.txt": `買い物リスト\n============\n\n- 牛乳\n- 卵\n- コーヒー豆（深煎り）\n\n明日の予定:\n  10:00 サーバーのバックアップ確認\n  14:00 デプロイ作業 (prod-web-01)\n  18:00 ジム\n\nメモ: patchbay の転送キューで一時停止と再開をテストすること。\n`,
  "emoji 🚀 launch notes.md": `# 🚀 Launch notes\n\n## ✅ Done\n\n- 📦 Release builds for all platforms\n- 🔐 Code signing (macOS, Windows)\n- 🌍 Website copy\n\n## ⏳ Pending\n\n- 📝 Changelog\n- 🐛 Fix drag & drop on Wayland\n\n> Ship it! 🎉\n`,
};

// -------------------------------------------------------- generated lines

const IPS = ["203.0.113.7", "198.51.100.23", "192.0.2.144", "203.0.113.88", "198.51.100.5", "10.0.2.15"];
const PATHS = ["/", "/api/v1/files", "/api/v1/hosts", "/static/app.js", "/static/site.css", "/login", "/favicon.ico", "/images/hero.jpg", "/api/v1/transfers?limit=50", "/healthz"];
const AGENTS = [
  "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0 Safari/537.36",
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_6) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15",
  "curl/8.5.0",
  "Prometheus/2.54.1",
];
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

function lineFor(kind: LineKind, r: () => number, t: Date, i: number): string {
  const p2 = (n: number) => String(n).padStart(2, "0");
  const hms = `${p2(t.getUTCHours())}:${p2(t.getUTCMinutes())}:${p2(t.getUTCSeconds())}`;
  const sys = `${MONTHS[t.getUTCMonth()]} ${String(t.getUTCDate()).padStart(2, " ")} ${hms}`;
  switch (kind) {
    case "nginx": {
      const status = r() < 0.9 ? 200 : pick(r, [301, 304, 404, 499, 502]);
      return `${pick(r, IPS)} - - [${p2(t.getUTCDate())}/${MONTHS[t.getUTCMonth()]}/${t.getUTCFullYear()}:${hms} +0000] "${r() < 0.85 ? "GET" : "POST"} ${pick(r, PATHS)} HTTP/2.0" ${status} ${Math.floor(r() * 48000)} "-" "${pick(r, AGENTS)}"`;
    }
    case "syslog":
      return `${sys} host ${pick(r, ["systemd[1]", "kernel", "sshd[2211]", "CRON[9921]", "dockerd[811]"])}: ${pick(r, ["Started Daily apt download activities.", "Accepted publickey for ops from 203.0.113.7 port 51022 ssh2: ED25519 SHA256:q7Xv4mJ0", "pam_unix(cron:session): session opened for user root", "EXT4-fs (sda1): re-mounted. Opts: errors=remount-ro", "Finished Rotate log files.", "container 3fa1c2 health_status: healthy"])}`;
    case "auth":
      return `${sys} host sshd[${1000 + Math.floor(r() * 9000)}]: ${pick(r, ["Accepted publickey for ops from 203.0.113.7 port 51022 ssh2", "Invalid user admin from 198.51.100.23 port 40122", "Disconnected from authenticating user root 192.0.2.144 port 58812 [preauth]", "pam_unix(sshd:session): session opened for user ops(uid=1000) by (uid=0)"])}`;
    case "app":
      return `${t.toISOString()} ${pick(r, ["INFO ", "INFO ", "INFO ", "DEBUG", "WARN ", "ERROR"])} ${pick(r, ["http", "worker", "db", "cache", "auth"])}: ${pick(r, ["request completed", "job finished", "cache miss", "slow query detected", "retrying upstream", "user signed in"])} id=${(i * 7919) % 100000} latency_ms=${Math.floor(r() * 900)}`;
    case "postgres":
      return `${t.toISOString().replace("T", " ").slice(0, 19)} UTC [${2000 + Math.floor(r() * 3000)}] ${pick(r, ["LOG:  checkpoint starting: time", "LOG:  checkpoint complete: wrote 1532 buffers (0.3%)", "LOG:  duration: 312.442 ms  statement: SELECT * FROM events WHERE created_at > now() - interval '1 day'", "LOG:  automatic vacuum of table \"app.public.sessions\"", "ERROR:  duplicate key value violates unique constraint \"users_email_key\""])}`;
    case "build":
      return `[${hms}] ${pick(r, ["   Compiling", "   Compiling", "    Checking", "     Running", "    Finished"])} ${pick(r, ["russh v0.45.0", "tokio v1.40.0", "patchbay-core v0.1.0", "tauri v2.1.0", "serde v1.0.210", "release [optimized] target(s) in 2m 11s"])}`;
    case "csv":
      return `${t.toISOString()},${pick(r, ["cpu", "gpu", "ambient", "humidity"])},${(20 + r() * 50).toFixed(2)}`;
  }
}

function generateLines(kind: LineKind, seed: number, bytes: number): Uint8Array {
  const r = rng(seed);
  const parts: string[] = kind === "csv" ? ["timestamp,sensor,value"] : [];
  let len = parts.length ? parts[0].length + 1 : 0;
  // Start some days ago and walk forward in time.
  let ms = Date.now() - 3 * 86400_000 - (seed % 1000) * 60_000;
  for (let i = 0; len < bytes; i++) {
    ms += Math.floor(r() * 4000);
    const line = lineFor(kind, r, new Date(ms), i);
    parts.push(line);
    len += line.length + 1;
  }
  const out = enc.encode(parts.join("\n") + "\n");
  return out.subarray(0, bytes);
}

function generateBinary(magic: number[], seed: number, bytes: number): Uint8Array {
  const out = new Uint8Array(bytes);
  out.set(magic.slice(0, bytes));
  const r = rng(seed);
  for (let i = magic.length; i < bytes; i++) out[i] = Math.floor(r() * 256);
  return out;
}

// ----------------------------------------------------------------- images

const imageCache = new Map<string, Promise<Uint8Array>>();

type Ctx2D = CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D;

function makeCanvas(w: number, h: number): { ctx: Ctx2D; toBlob: (type: string, q?: number) => Promise<Blob> } {
  if (typeof OffscreenCanvas !== "undefined") {
    const c = new OffscreenCanvas(w, h);
    const ctx = c.getContext("2d");
    if (!ctx) throw new Error("no 2d context");
    return { ctx, toBlob: (type, quality) => c.convertToBlob({ type, quality }) };
  }
  const c = document.createElement("canvas");
  c.width = w;
  c.height = h;
  const ctx = c.getContext("2d");
  if (!ctx) throw new Error("no 2d context");
  return {
    ctx,
    toBlob: (type, quality) =>
      new Promise((resolve, reject) => c.toBlob((b) => (b ? resolve(b) : reject(new Error("encode failed"))), type, quality)),
  };
}

function paint(ctx: Ctx2D, w: number, h: number, label: string, seed: number) {
  const r = rng(seed);
  const hue = Math.floor(r() * 360);
  const g = ctx.createLinearGradient(0, 0, w, h);
  g.addColorStop(0, `hsl(${hue} 70% 62%)`);
  g.addColorStop(1, `hsl(${(hue + 60 + Math.floor(r() * 80)) % 360} 65% 38%)`);
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, w, h);
  if (w >= 96) {
    // Sun + layered hills.
    ctx.fillStyle = `hsla(${(hue + 30) % 360} 95% 85% / 0.85)`;
    ctx.beginPath();
    ctx.arc(w * (0.2 + r() * 0.6), h * (0.22 + r() * 0.15), Math.min(w, h) * 0.12, 0, Math.PI * 2);
    ctx.fill();
    for (let layer = 0; layer < 3; layer++) {
      ctx.fillStyle = `hsla(${(hue + 180 + layer * 20) % 360} 35% ${30 - layer * 8}% / ${0.55 + layer * 0.15})`;
      ctx.beginPath();
      ctx.moveTo(0, h);
      const base = h * (0.55 + layer * 0.12);
      for (let x = 0; x <= w; x += w / 8) ctx.lineTo(x, base - r() * h * 0.18);
      ctx.lineTo(w, h);
      ctx.closePath();
      ctx.fill();
    }
    const fs = Math.max(12, Math.round(Math.min(w / 18, h / 10)));
    ctx.font = `600 ${fs}px system-ui, sans-serif`;
    ctx.fillStyle = "rgba(255,255,255,0.92)";
    ctx.shadowColor = "rgba(0,0,0,0.35)";
    ctx.shadowBlur = fs / 3;
    ctx.fillText(label, fs, h - fs);
    ctx.shadowBlur = 0;
  } else {
    ctx.fillStyle = "rgba(255,255,255,0.9)";
    ctx.beginPath();
    ctx.arc(w / 2, h / 2, w * 0.3, 0, Math.PI * 2);
    ctx.fill();
  }
}

async function renderImage(fmt: ImageFormat, w: number, h: number, label: string, seed: number): Promise<Uint8Array> {
  const { ctx, toBlob } = makeCanvas(w, h);
  paint(ctx, w, h, label, seed);
  if (fmt === "png" || fmt === "jpeg" || fmt === "webp") {
    const blob = await toBlob(`image/${fmt}`, 0.86);
    return new Uint8Array(await blob.arrayBuffer());
  }
  const pixels = ctx.getImageData(0, 0, w, h).data;
  if (fmt === "bmp") return encodeBmp(pixels, w, h);
  if (fmt === "gif") return encodeGif(pixels, w, h);
  // ICO wrapping a PNG (supported since Vista and by every browser).
  const png = new Uint8Array(await (await toBlob("image/png")).arrayBuffer());
  const out = new Uint8Array(6 + 16 + png.length);
  const v = new DataView(out.buffer);
  v.setUint16(2, 1, true);
  v.setUint16(4, 1, true);
  out[6] = w >= 256 ? 0 : w;
  out[7] = h >= 256 ? 0 : h;
  v.setUint16(10, 1, true);
  v.setUint16(12, 32, true);
  v.setUint32(14, png.length, true);
  v.setUint32(18, 22, true);
  out.set(png, 22);
  return out;
}

/** 24-bit bottom-up BMP. */
function encodeBmp(px: Uint8ClampedArray, w: number, h: number): Uint8Array {
  const row = Math.ceil((w * 3) / 4) * 4;
  const size = 54 + row * h;
  const out = new Uint8Array(size);
  const v = new DataView(out.buffer);
  out[0] = 0x42;
  out[1] = 0x4d;
  v.setUint32(2, size, true);
  v.setUint32(10, 54, true);
  v.setUint32(14, 40, true);
  v.setInt32(18, w, true);
  v.setInt32(22, h, true);
  v.setUint16(26, 1, true);
  v.setUint16(28, 24, true);
  v.setUint32(34, row * h, true);
  for (let y = 0; y < h; y++) {
    const dst = 54 + (h - 1 - y) * row;
    for (let x = 0; x < w; x++) {
      const s = (y * w + x) * 4;
      out[dst + x * 3] = px[s + 2];
      out[dst + x * 3 + 1] = px[s + 1];
      out[dst + x * 3 + 2] = px[s];
    }
  }
  return out;
}

/**
 * GIF with a 128-colour palette and "uncompressed" LZW: with a minimum code
 * size of 7 every code is 8 bits wide as long as the table never grows past
 * 256 entries, so a clear code is emitted every 100 pixels and each code is
 * exactly one byte.
 */
function encodeGif(px: Uint8ClampedArray, w: number, h: number): Uint8Array {
  const bytes: number[] = [];
  const push = (...xs: number[]) => bytes.push(...xs);
  const u16 = (n: number) => push(n & 0xff, (n >> 8) & 0xff);
  push(...enc.encode("GIF89a"));
  u16(w);
  u16(h);
  push(0xf6, 0, 0); // global colour table, 128 entries (2^(6+1))
  // Palette: 4 levels R x 8 levels G x 4 levels B.
  for (let i = 0; i < 128; i++) {
    push(Math.round(((i >> 5) & 3) * 85), Math.round(((i >> 2) & 7) * (255 / 7)), Math.round((i & 3) * 85));
  }
  push(0x2c);
  u16(0);
  u16(0);
  u16(w);
  u16(h);
  push(0);
  push(7); // LZW minimum code size
  const codes: number[] = [];
  const CLEAR = 128;
  const EOI = 129;
  for (let i = 0; i < w * h; i++) {
    if (i % 100 === 0) codes.push(CLEAR);
    const s = i * 4;
    const ri = Math.min(3, Math.round(px[s] / 85));
    const gi = Math.min(7, Math.round(px[s + 1] / (255 / 7)));
    const bi = Math.min(3, Math.round(px[s + 2] / 85));
    codes.push((ri << 5) | (gi << 2) | bi);
  }
  codes.push(EOI);
  for (let i = 0; i < codes.length; i += 255) {
    const chunk = codes.slice(i, i + 255);
    push(chunk.length, ...chunk);
  }
  push(0, 0x3b);
  return new Uint8Array(bytes);
}

// ------------------------------------------------------------------ read

/** Reads up to `maxBytes` of a file whose listed size is `size`. */
export async function readContent(spec: ContentSpec | null, name: string, size: number, maxBytes: number): Promise<Uint8Array> {
  const n = Math.max(0, Math.min(size, maxBytes));
  const s: ContentSpec = spec ?? defaultContent(name, hash(name));
  switch (s.t) {
    case "text": {
      const b = enc.encode(s.text);
      return b.subarray(0, Math.min(b.length, maxBytes));
    }
    case "bytes":
      return s.data.slice(0, Math.min(s.data.length, maxBytes));
    case "lines":
      return generateLines(s.kind, s.seed, n);
    case "binary":
      return generateBinary(s.magic, s.seed, n);
    case "image": {
      const key = `${s.fmt}:${s.w}x${s.h}:${s.label}:${s.seed}`;
      let p = imageCache.get(key);
      if (!p) {
        p = renderImage(s.fmt, s.w, s.h, s.label, s.seed);
        imageCache.set(key, p);
        p.catch(() => imageCache.delete(key));
      }
      const data = await p;
      return data.slice(0, Math.min(data.length, maxBytes));
    }
  }
}

/** Content for a file with no explicit spec, inferred from its name. */
export function defaultContent(name: string, seed: number): ContentSpec {
  const img = imageFormatOf(name);
  if (img) {
    const icon = img === "ico";
    return { t: "image", fmt: img, w: icon ? 64 : 960, h: icon ? 64 : 640, label: name, seed };
  }
  if (extOf(name) === "svg") return { t: "text", text: svgFor(name, seed) };
  if (extOf(name) === "log") return { t: "lines", kind: /access/.test(name) ? "nginx" : "app", seed };
  if (isTextName(name)) return { t: "text", text: textFor(name, seed) };
  return { t: "binary", magic: magicFor(name), seed };
}

export function svgFor(name: string, seed: number): string {
  const r = rng(seed);
  const hue = Math.floor(r() * 360);
  return `<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" viewBox="0 0 512 512">
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="hsl(${hue} 80% 60%)"/>
      <stop offset="1" stop-color="hsl(${(hue + 70) % 360} 75% 45%)"/>
    </linearGradient>
  </defs>
  <rect width="512" height="512" rx="112" fill="url(#g)"/>
  <path d="M136 300c60-90 180-90 240 0" fill="none" stroke="#fff" stroke-width="36" stroke-linecap="round"/>
  <circle cx="256" cy="196" r="44" fill="#fff" opacity="0.92"/>
  <text x="256" y="430" font-family="system-ui, sans-serif" font-size="40" font-weight="600" text-anchor="middle" fill="#fff" opacity="0.85">${name.replace(/[<&>]/g, "")}</text>
</svg>
`;
}

export function byteLength(text: string): number {
  return enc.encode(text).length;
}
