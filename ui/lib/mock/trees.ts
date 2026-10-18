// Mock filesystem contents per machine. Built lazily (on first use) by host
// name; unknown hosts get a generic Linux tree with home /home/<user>.
//
// Demo paths worth knowing:
//   local      /home/rodrigo                 home (unicode names in Documents,
//              Pictures/* images incl. panorama-raw.png (24 MiB, over the
//              preview limit), Downloads/big-dataset.tar.zst (fails once),
//              Projects/big-dir (2000 entries), Desktop (empty),
//              notes -> file link, www -> dir link, old-config -> dangling)
//              /root                         permission denied
//   bastion    /home/ops/docs                conflicts with local ~/Documents
//              /var/log/archive              2000 rotated logs
//   win-buildbox /C:/Users/rodrigo           Windows paths; /C:/Windows/System32/config denied
//   media-share  /upload                     the only writable dir (SFTP-only, chrooted)

import { ELF, hash, type LineKind } from "./content";
import {
  Builder,
  GB,
  KB,
  kids,
  MB,
  splitPath,
  type DirNode,
  type Machine,
  type Node,
} from "./vfs";

const HOUR = 1 / 24;
const MIN = 1 / 1440;

/** mkdir -p `path` under `root` (missing dirs owned by `b`'s defaults) and returns it. */
function ensureDir(b: Builder, root: DirNode, path: string): DirNode {
  let cur = root;
  for (const part of splitPath(path)) {
    const map = kids(cur);
    let next = map.get(part);
    if (!next) {
      next = b.dir(part);
      map.set(part, next);
    }
    if (next.kind !== "dir") throw new Error(`mock tree: ${path} is not a dir`);
    cur = next;
  }
  return cur;
}

function put(b: Builder, root: DirNode, dir: string, ...nodes: Node[]) {
  const d = ensureDir(b, root, dir);
  for (const n of nodes) kids(d).set(n.name, n);
}

const bin = (b: Builder, name: string, size: number, perm = 0o755) =>
  b.with(name, size, { t: "binary", magic: ELF, seed: hash(name) }, { perm, owner: "root", group: "root" });
const lines = (b: Builder, name: string, size: number, kind: LineKind, age = 0.02) =>
  b.with(name, size, { t: "lines", kind, seed: hash(name) ^ size }, { age });

// ------------------------------------------------------------ unix skeleton

function unixSkeleton(sys: Builder, hostname: string): DirNode {
  const root = sys.dir("/", [], { owner: "root", group: "root" });
  put(sys, root, "/", sys.link("bin", "usr/bin"), sys.link("lib", "usr/lib"));
  put(
    sys,
    root,
    "/etc",
    sys.file("hosts"),
    sys.text("hostname", `${hostname}\n`),
    sys.text("os-release", `PRETTY_NAME="Debian GNU/Linux 12 (bookworm)"\nNAME="Debian GNU/Linux"\nVERSION_ID="12"\nID=debian\n`),
    sys.text("fstab", `UUID=4f1c2a90-7e1d-4b2a-9c3e-1d2e3f4a5b6c / ext4 errors=remount-ro 0 1\nUUID=9A3D-5E2F /boot/efi vfat umask=0077 0 1\n`),
    sys.file("passwd"),
    sys.text("shadow", "root:*:19700:0:99999:7:::\n", { perm: 0o640, group: "shadow" }),
  );
  put(sys, root, "/etc/ssh", sys.file("sshd_config"), sys.text("ssh_config", "Host *\n    SendEnv LANG LC_*\n    HashKnownHosts yes\n"));
  put(sys, root, "/root", ...[]);
  const r = kids(root).get("root") as DirNode;
  r.denied = true;
  r.permissions = 0o700;
  put(sys, root, "/tmp");
  (kids(root).get("tmp") as DirNode).permissions = 0o1777;
  put(
    sys,
    root,
    "/usr/bin",
    bin(sys, "bash", 1.4 * MB),
    bin(sys, "ls", 151 * KB),
    bin(sys, "ssh", 1.1 * MB),
    bin(sys, "sudo", 277 * KB, 0o4755),
    bin(sys, "python3", 6.8 * MB),
    bin(sys, "git", 3.9 * MB),
    bin(sys, "rsync", 548 * KB),
    bin(sys, "tar", 534 * KB),
    sys.link("python", "python3"),
  );
  put(sys, root, "/usr/lib");
  put(sys, root, "/usr/local/bin");
  put(sys, root, "/usr/share/doc");
  put(sys, root, "/opt");
  put(sys, root, "/srv");
  put(sys, root, "/home");
  put(
    sys,
    root,
    "/var/log",
    lines(sys, "syslog", 3.2 * MB, "syslog", 2 * MIN),
    lines(sys, "auth.log", 840 * KB, "auth", 14 * MIN),
    lines(sys, "kern.log", 210 * KB, "syslog", 3 * HOUR),
    sys.file("dpkg.log", 96 * KB, { age: 6 }),
  );
  put(sys, root, "/var/lib");
  put(sys, root, "/var/cache");
  put(sys, root, "/var/backups");
  return root;
}

function sshDir(b: Builder, extra: Node[] = []): DirNode {
  return b.dir(
    ".ssh",
    [
      b.file("authorized_keys", undefined, { perm: 0o600 }),
      b.file("known_hosts", undefined, { perm: 0o644 }),
      ...extra,
    ],
    { perm: 0o700 },
  );
}

function homeBasics(b: Builder): Node[] {
  return [
    b.file(".bashrc", undefined, { age: 90 }),
    b.file(".profile", undefined, { age: 400 }),
    lines(b, ".bash_history", 18 * KB, "build", 30 * MIN),
    sshDir(b),
  ];
}

// -------------------------------------------------------------- big dirs

const BIG_WORDS = ["report", "invoice", "photo", "draft", "export", "backup", "notes", "scan", "data", "render", "clip", "chart", "config", "dump", "sample", "trace", "frame", "summary"];
const BIG_EXT = ["ts", "rs", "json", "md", "png", "jpg", "log", "txt", "csv", "svg", "gz", "pdf", "yaml", "zip", "webp", "mp3"];

function bigDir(name: string, owner: string, group: string): DirNode {
  const seed = new Builder(name, owner, group);
  return seed.lazyDir(
    name,
    () => {
      const b = new Builder(`${name}/contents`, owner, group);
      const out: Node[] = [];
      for (let i = 1; i <= 40; i++) {
        const mod = `module-${String(i).padStart(2, "0")}`;
        out.push(
          b.lazyDir(mod, () => {
            const c = new Builder(`${name}/${mod}`, owner, group);
            return Array.from({ length: 8 + Math.floor(c.r() * 12) }, (_, j) => {
              const ext = c.pick(BIG_EXT);
              return c.file(`${c.pick(BIG_WORDS)}-${j}.${ext}`, c.size(300, 4 * MB));
            });
          }),
        );
      }
      for (let i = 1; out.length < 2000; i++) {
        const ext = b.pick(BIG_EXT);
        const hidden = i % 97 === 0 ? "." : "";
        out.push(b.file(`${hidden}${b.pick(BIG_WORDS)}-${String(i).padStart(4, "0")}.${ext}`, b.size(120, 60 * MB)));
      }
      return out;
    },
    { slow: true, age: 1 },
  );
}

function logArchive(): DirNode {
  const b0 = new Builder("archive", "syslog", "adm");
  return b0.lazyDir(
    "archive",
    () => {
      const b = new Builder("archive/contents", "syslog", "adm");
      const services: [string, number, number][] = [
        ["app", 200 * KB, 9 * MB],
        ["nginx-access", 1 * MB, 40 * MB],
        ["nginx-error", 2 * KB, 300 * KB],
        ["auth", 20 * KB, 900 * KB],
        ["worker", 80 * KB, 6 * MB],
      ];
      const out: Node[] = [];
      const today = new Date();
      for (let k = 1; k <= 400; k++) {
        const d = new Date(today.getTime() - k * 86400_000);
        const date = d.toISOString().slice(0, 10);
        for (const [svc, lo, hi] of services) {
          out.push(b.file(`${svc}-${date}.log.gz`, b.size(lo, hi), { perm: 0o640, age: k - 1 + 0.99 }));
        }
      }
      return out;
    },
    { slow: true, age: 0.2, perm: 0o755, owner: "root", group: "root" },
  );
}

// ------------------------------------------------------------------ local

const MEETING_LOCAL = `# Weekly ops sync\n\n## Agenda\n\n- Patchbay beta feedback\n- Bastion key rotation\n- Postgres upgrade window\n\n## Decisions\n\n- Rotate bastion keys on Thursday.\n`;
const MEETING_REMOTE = `${MEETING_LOCAL}- Move backups to the NAS (pending).\n\n## Action items\n\n- [ ] ops: schedule maintenance window\n- [ ] rodrigo: test resume of interrupted uploads\n- [x] everyone: update runbook links\n`;
const CHECKLIST_LOCAL = `# Deploy checklist\n\n1. Announce in #deploys\n2. Run migrations (dry-run first)\n3. Deploy canary, watch error rate for 10 minutes\n4. Roll out to all nodes\n5. Verify /healthz and dashboards\n6. Post summary\n`;
const CHECKLIST_REMOTE = `# Deploy checklist (old)\n\n1. Announce\n2. Migrate\n3. Deploy everything at once\n4. Hope\n\nThis version predates canary deploys — see the new one in the repo.\n`;

function localMachine(): Machine {
  const user = "rodrigo";
  const sys = new Builder("local/sys", "root", "root");
  const b = new Builder("local/home", user, user);
  const root = unixSkeleton(sys, "laptop");
  put(sys, root, "/etc/nginx", sys.file("nginx.conf"), sys.dir("sites-available", [sys.file("default")]));

  const home = "/home/rodrigo";
  put(
    b,
    root,
    home,
    ...homeBasics(b).filter((n) => n.name !== ".ssh"),
    b.file(".gitconfig", undefined, { age: 40 }),
    b.dir(
      ".ssh",
      [
        b.file("config", undefined, { age: 3 }),
        b.file("id_ed25519", undefined, { perm: 0o600, age: 300 }),
        b.text("id_ed25519.pub", "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGZlcnJ5LW1vY2sta2V5LW5vdC1hLXJlYWwta2V5 rodrigo@laptop\n", { age: 300 }),
        b.file("known_hosts", undefined, { age: 0.5 }),
        b.file("authorized_keys", undefined, { perm: 0o600, age: 300 }),
      ],
      { perm: 0o700 },
    ),
    b.dir(".config", [
      b.dir("patchbay", [b.text("settings.json", `{\n  "theme": "system",\n  "transfers": { "parallel": 3 }\n}\n`, { age: 1 })]),
      b.dir("git", [b.text("ignore", "*.swp\n.DS_Store\n")]),
      b.dir("htop", [b.text("htoprc", "fields=0 48 17 18 38 39 40 2 46 47 49 1\nsort_key=46\n")]),
      b.dir("Code", [b.dir("User", [b.file("settings.json")])]),
    ]),
    b.dir(".cache", [b.dir("fontconfig"), b.dir("pip", [b.dir("http")]), b.dir("mesa_shader_cache")]),
    b.dir(".local", [b.dir("share", [b.dir("applications"), b.dir("fonts")]), b.dir("bin")]),
    b.dir("Desktop", [], { age: 2 }),
    b.dir("Documents", [
      b.file("Ångström résumé.pdf", 182 * KB, { age: 30 }),
      b.file("日本語のメモ.txt", undefined, { age: 0.3 }),
      b.file("emoji 🚀 launch notes.md", undefined, { age: 4 }),
      b.file("Quarterly infrastructure review — final draft with appendix, speaker notes and the updated cost model (v7).docx", 2.4 * MB, { age: 12 }),
      b.text("meeting-notes.md", MEETING_LOCAL, { age: 0.08 }),
      b.text("deploy-checklist.md", CHECKLIST_LOCAL, { age: 2 }),
      b.file("architecture.pdf", 3.4 * MB, { age: 5 }),
      b.file("invoice-2026-09.pdf", 96 * KB, { age: 17 }),
      b.file("budget-2026.xlsx", 340 * KB, { age: 45 }),
      b.file("todo.txt", undefined, { age: 25 * MIN }),
      b.dir("Taxes", [b.file("2025-return.pdf", 1.2 * MB, { age: 180 }), b.file("receipts.zip", 38 * MB, { age: 190 })], { perm: 0o700 }),
    ]),
    b.dir("Downloads", [
      b.file("patchbay_0.1.0_amd64.deb", 14.2 * MB, { age: 1 }),
      b.file("Patchbay_0.1.0_aarch64.dmg", 18.6 * MB, { age: 1 }),
      b.file("ubuntu-24.04.1-desktop-amd64.iso", 4.2 * GB, { age: 60 }),
      b.file("big-dataset.tar.zst", 900 * MB, { age: 3 }),
      b.file("terraform_1.9.5_linux_amd64.zip", 27 * MB, { age: 20 }),
      b.file("Screenshot 2026-10-16 at 09.41.22.png", 640 * KB, { age: 1 }),
      b.file("invoice.pdf", 88 * KB, { age: 8 }),
    ]),
    b.dir("Pictures", [
      b.file("sunset.jpg", 2.1 * MB, { age: 9 }),
      b.file("mountains.webp", 860 * KB, { age: 40 }),
      b.file("logo.svg", undefined, { age: 70 }),
      b.file("diagram.png", 420 * KB, { age: 3 }),
      b.file("cat.gif", 610 * KB, { age: 120 }),
      b.file("favicon.ico", 4 * KB, { age: 200 }),
      b.file("scan.bmp", 1.8 * MB, { age: 300 }),
      b.with("panorama-raw.png", 24 * MB, { t: "image", fmt: "png", w: 2400, h: 800, label: "panorama-raw.png", seed: 7 }, { age: 14 }),
      b.file("wallpaper-4k.png", 7.8 * MB, { age: 33 }),
      b.dir("vacation-2025", Array.from({ length: 14 }, (_, i) => b.file(`IMG_${2041 + i}.jpg`, b.size(1.8 * MB, 5.5 * MB), { age: 400 - i * 0.1 })), { age: 400 }),
      b.dir("Screenshots", [b.file("Screenshot from 2026-10-14 11-02-51.png", 380 * KB, { age: 3 })]),
    ]),
    b.dir("Projects", [
      b.dir("patchbay", [
        b.file("README.md", undefined, { age: 0.5 }),
        b.file("Cargo.toml", undefined, { age: 4 }),
        b.file("package.json", undefined, { age: 2 }),
        b.file(".gitignore", undefined, { age: 60 }),
        b.dir(".git", [b.text("HEAD", "ref: refs/heads/main\n"), b.file("config"), b.dir("objects"), b.dir("refs", [b.dir("heads"), b.dir("tags")])], { age: 10 * MIN }),
        b.dir("src", [b.file("main.rs", undefined, { age: 1 }), b.file("lib.rs", undefined, { age: 0.2 })]),
        b.dir("ui", [b.file("App.svelte", undefined, { age: 12 * MIN }), b.file("main.ts"), b.file("app.css")]),
        b.dir("target", [b.dir("debug", [bin(b, "patchbay", 182 * MB)])], { age: 0.1 }),
      ], { age: 10 * MIN }),
      b.dir("website", [b.file("index.html"), b.file("style.css"), b.file("app.ts"), b.dir("images", [b.file("hero.jpg", 1.1 * MB), b.file("logo.svg")])]),
      bigDir("big-dir", user, user),
    ]),
    b.dir("Music", [
      b.dir("Boards of Canada — Music Has the Right to Children", Array.from({ length: 6 }, (_, i) => b.file(`${String(i + 1).padStart(2, "0")} Track ${i + 1}.flac`, b.size(18 * MB, 42 * MB), { age: 800 }))),
      b.file("podcast-episode-212.mp3", 61 * MB, { age: 6 }),
    ]),
    b.dir("Videos", [b.file("holiday.mp4", 1.4 * GB, { age: 80 }), b.file("screen-recording.webm", 210 * MB, { age: 2 })]),
    b.link("notes", "Documents/日本語のメモ.txt", { age: 50 }),
    b.link("www", "Projects/website", { age: 50 }),
    b.link("old-config", "/etc/patchbay/legacy.conf", { age: 500 }),
  );
  return { key: "local", label: "this computer", root, home, user, group: user, remote: false, exec: true, lan: true, windows: false };
}

// ------------------------------------------------------------ remote unix

interface UnixHost {
  key: string;
  label: string;
  hostname: string;
  user: string;
  group: string;
  home: string;
  lan: boolean;
  exec?: boolean;
  /** Adds host-specific content. */
  fill: (root: DirNode, sys: Builder, b: Builder) => void;
}

function unixMachine(h: UnixHost): Machine {
  const sys = new Builder(`${h.key}/sys`, "root", "root");
  const b = new Builder(`${h.key}/home`, h.user, h.group);
  const root = unixSkeleton(sys, h.hostname);
  ensureDir(b, root, h.home);
  h.fill(root, sys, b);
  return { key: h.key, label: h.label, root, home: h.home, user: h.user, group: h.group, remote: true, exec: h.exec ?? true, lan: h.lan, windows: false };
}

function releases(b: Builder, names: string[], files: () => Node[]): DirNode {
  return b.dir("releases", names.map((n, i) => b.dir(n, files(), { age: (names.length - i) * 9 })));
}

function bastion(key: string, label: string): Machine {
  return unixMachine({
    key,
    label,
    hostname: "bastion",
    user: "ops",
    group: "ops",
    home: "/home/ops",
    lan: false,
    fill(root, sys, b) {
      put(
        b,
        root,
        "/home/ops",
        ...homeBasics(b),
        b.dir("docs", [
          b.text("meeting-notes.md", MEETING_REMOTE, { age: 0.02 }),
          b.text("deploy-checklist.md", CHECKLIST_REMOTE, { age: 220 }),
          b.file("architecture.pdf", 1.1 * MB, { age: 0.01 }),
          b.file("runbook.md", undefined, { age: 15 }),
          b.file("oncall-rotation.md", undefined, { age: 7 }),
        ]),
        b.dir("scripts", [b.file("backup.sh", undefined, { perm: 0o755 }), b.file("rotate-keys.sh", undefined, { perm: 0o750 }), b.file("healthcheck.py", undefined, { perm: 0o755 })]),
        b.dir("deploy", [
          releases(b, ["2026-09-12", "2026-10-01"], () => [b.file("app.tar.zst", b.size(30 * MB, 60 * MB)), b.file("manifest.json"), b.file("CHANGELOG.md")]),
          b.link("current", "releases/2026-10-01", { age: 16 }),
          b.dir("shared", [b.file("config.yaml")]),
        ]),
        b.link("old-backups", "/mnt/old-backups/latest", { age: 300 }),
        b.file("notes.txt", undefined, { age: 3 }),
      );
      put(sys, root, "/var/log", logArchive(), lines(sys, "app.log", 6.5 * MB, "app", 1 * MIN));
      put(sys, root, "/var/log/nginx", lines(sys, "access.log", 48 * MB, "nginx", 10 / 86400), lines(sys, "error.log", 120 * KB, "syslog", 2 * HOUR));
      put(sys, root, "/etc/nginx", sys.file("nginx.conf"));
    },
  });
}

function dbInternal(key: string, label: string): Machine {
  return unixMachine({
    key,
    label,
    hostname: "db-internal",
    user: "postgres",
    group: "postgres",
    home: "/var/lib/postgresql",
    lan: false,
    fill(root, sys, b) {
      const seg = (i: number) => b.with(`0000000100000A2B000000${(0x40 + i).toString(16).toUpperCase()}`, 16 * MB, { t: "binary", magic: [0x10, 0xd1, 0x05, 0x00], seed: i }, { perm: 0o600, age: (12 - i) * HOUR });
      put(
        b,
        root,
        "/var/lib/postgresql",
        lines(b, ".psql_history", 9 * KB, "postgres", 2 * HOUR),
        lines(b, ".bash_history", 4 * KB, "build", 1),
        b.dir("16", [
          b.dir("main", [
            b.text("PG_VERSION", "16\n", { perm: 0o600, age: 300 }),
            b.text("postgresql.auto.conf", "# Do not edit this file manually!\nmax_wal_senders = '10'\n", { perm: 0o600 }),
            b.text("postmaster.pid", "1123\n/var/lib/postgresql/16/main\n1729100000\n5432\n", { perm: 0o600, age: 4 }),
            b.dir("base", ["1", "4", "5", "16384"].map((oid) => b.dir(oid, Array.from({ length: 6 }, (_, i) => b.with(String(1247 + i * 13), b.size(8 * KB, 900 * MB), { t: "binary", magic: [0, 0, 0, 0], seed: i }, { perm: 0o600 })), { perm: 0o700 })), { perm: 0o700 }),
            b.dir("global", [b.with("pg_control", 8 * KB, { t: "binary", magic: [0x31, 0x2f], seed: 1 }, { perm: 0o600 })], { perm: 0o700 }),
            b.dir("pg_wal", Array.from({ length: 12 }, (_, i) => seg(i)), { perm: 0o700 }),
          ], { perm: 0o700 }),
        ]),
        b.dir("backups", Array.from({ length: 14 }, (_, i) => {
          const d = new Date(Date.now() - i * 86400_000).toISOString().slice(0, 10);
          return b.file(`app-${d}.sql.gz`, b.size(380 * MB, 460 * MB), { age: i + 0.9, perm: 0o640 });
        })),
        b.dir("wal-archive", [], { perm: 0o700 }),
        b.link("latest-backup", `backups/app-${new Date().toISOString().slice(0, 10)}.sql.gz`, { age: 0.9 }),
        b.link("replica-slot", "/srv/replica/slot-01", { age: 120 }),
      );
      put(sys, root, "/etc/postgresql/16/main", sys.file("postgresql.conf"), sys.file("pg_hba.conf"));
      put(sys, root, "/var/log/postgresql", lines(sys, "postgresql-16-main.log", 22 * MB, "postgres", 30 / 86400));
    },
  });
}

function prodWeb(key: string, label: string): Machine {
  return unixMachine({
    key,
    label,
    hostname: "web01",
    user: "deploy",
    group: "deploy",
    home: "/home/deploy",
    lan: false,
    fill(root, sys, b) {
      put(
        b,
        root,
        "/home/deploy",
        ...homeBasics(b),
        b.dir("app", [
          releases(b, ["2026-09-28", "2026-10-01", "2026-10-15"], () => [bin(b, "server", 24 * MB), b.file("config.toml"), b.dir("static", [b.file("app.js"), b.file("site.css")])]),
          b.link("current", "releases/2026-10-15", { age: 2 }),
          b.dir("shared", [b.file(".env", undefined, { perm: 0o600 }), b.dir("uploads", [b.file("avatar-1182.png", 220 * KB), b.file("report-q3.pdf", 1.4 * MB)])]),
        ]),
        b.file("deploy.sh", undefined, { perm: 0o755, age: 2 }),
        b.link("logs", "/var/log/app", { age: 200 }),
        b.link("app.sock", "/run/app/app.sock", { age: 2 }),
      );
      const www = new Builder(`${key}/www`, "www-data", "www-data");
      put(
        www,
        root,
        "/var/www/site",
        www.file("index.html", undefined, { age: 2 }),
        www.file("about.html"),
        www.file("404.html"),
        www.file("robots.txt"),
        www.file("favicon.ico", 15 * KB),
        www.dir("css", [www.file("site.css")]),
        www.dir("js", [www.file("app.js")]),
        www.dir("images", [www.file("hero.jpg", 1.6 * MB), www.file("logo.svg"), www.file("og-image.png", 310 * KB), www.file("team.webp", 520 * KB)]),
      );
      put(sys, root, "/etc/nginx", sys.file("nginx.conf"), sys.dir("sites-available", [sys.file("default")]), sys.dir("sites-enabled", [sys.link("default", "../sites-available/default")]));
      put(sys, root, "/var/log/nginx", lines(sys, "access.log", 120 * MB, "nginx", 3 / 86400), lines(sys, "error.log", 340 * KB, "syslog", 1 * HOUR));
      put(sys, root, "/var/log/app", lines(sys, "app.log", 14 * MB, "app", 20 / 86400));
    },
  });
}

function raspberrypi(key: string, label: string): Machine {
  return unixMachine({
    key,
    label,
    hostname: "raspberrypi",
    user: "pi",
    group: "pi",
    home: "/home/pi",
    lan: true,
    fill(root, sys, b) {
      put(
        b,
        root,
        "/home/pi",
        ...homeBasics(b),
        b.dir("camera", Array.from({ length: 30 }, (_, i) => {
          const t = new Date(Date.now() - i * 3 * 3600_000);
          const stamp = `${t.toISOString().slice(0, 10)}-${String(t.getUTCHours()).padStart(2, "0")}00`;
          return b.file(`capture-${stamp}.jpg`, b.size(1.2 * MB, 3.4 * MB), { age: i * 3 * HOUR });
        })),
        b.dir("photos", [b.file("garden.jpg", 2.8 * MB, { age: 20 })]),
        b.dir("scripts", [b.file("timelapse.py", undefined, { perm: 0o755 }), b.file("temp-monitor.sh", undefined, { perm: 0o755 })]),
        b.with("sensors.csv", 1.6 * MB, { t: "lines", kind: "csv", seed: 42 }, { age: 5 * MIN }),
        b.link("latest", "camera", { age: 30 }),
        b.link("usb", "/media/pi/USB-STICK", { age: 90 }),
      );
      put(sys, root, "/boot/firmware", sys.file("config.txt"), sys.text("cmdline.txt", "console=serial0,115200 console=tty1 root=PARTUUID=4f1c2a90-02 rootfstype=ext4 fsck.repair=yes rootwait\n"));
    },
  });
}

function legacyNas(key: string, label: string): Machine {
  return unixMachine({
    key,
    label,
    hostname: "nas",
    user: "admin",
    group: "wheel",
    home: "/home/admin",
    lan: true,
    fill(root, sys, b) {
      put(b, root, "/home/admin", ...homeBasics(b), b.link("tank", "/mnt/tank", { age: 600 }), b.link("offsite", "/mnt/offsite/current", { age: 400 }));
      put(
        b,
        root,
        "/mnt/tank",
        b.dir("backups", Array.from({ length: 8 }, (_, i) => b.file(`laptop-2026-10-${String(16 - i * 2).padStart(2, "0")}.tar.zst`, b.size(20 * GB, 26 * GB), { age: i * 2 + 0.2 }))),
        b.dir("media", [b.dir("Movies"), b.dir("Series"), b.dir("Music")]),
        b.dir("shares", [b.dir("family"), b.dir("work")]),
      );
      put(sys, root, "/usr/local/etc", sys.text("smb4.conf", "[global]\n  workgroup = HOME\n  server string = legacy-nas\n\n[tank]\n  path = /mnt/tank/shares\n  read only = no\n"));
    },
  });
}

function buildMac(key: string, label: string): Machine {
  const sys = new Builder(`${key}/sys`, "root", "wheel");
  const b = new Builder(`${key}/home`, "ci", "staff");
  const root = sys.dir("/", []);
  put(sys, root, "/", sys.link("tmp", "private/tmp"), sys.link("etc", "private/etc"), sys.link("var", "private/var"));
  put(sys, root, "/private/tmp");
  put(sys, root, "/private/etc", sys.file("hosts"), sys.text("shells", "/bin/bash\n/bin/zsh\n/bin/sh\n"));
  put(sys, root, "/private/var/log", lines(sys, "system.log", 2.1 * MB, "syslog", 5 * MIN));
  put(sys, root, "/private/var/root");
  (kids(ensureDir(sys, root, "/private/var")).get("root") as DirNode).denied = true;
  put(sys, root, "/Applications", sys.dir("Xcode.app", [sys.dir("Contents", [sys.file("Info.plist")])], { age: 20 }), sys.dir("Utilities", [sys.dir("Terminal.app")]));
  put(sys, root, "/Library", sys.dir("Developer"), sys.dir("Preferences"));
  put(sys, root, "/System", sys.dir("Library"), sys.dir("Applications"));
  put(sys, root, "/usr/bin", bin(sys, "zsh", 1.3 * MB), bin(sys, "ssh", 1.6 * MB), bin(sys, "xcodebuild", 140 * KB), bin(sys, "sudo", 1.4 * MB, 0o4511));
  put(
    b,
    root,
    "/Users/ci",
    b.file(".zshrc", undefined, { age: 30 }),
    sshDir(b),
    b.dir("Library", [b.dir("Caches", [b.dir("com.apple.dt.Xcode")]), b.dir("Logs", [lines(b, "build.log", 3.4 * MB, "build", 40 * MIN)]), b.dir("Developer", [b.dir("Xcode", [b.dir("DerivedData")])])]),
    b.dir("builds", [
      b.file("Patchbay-0.1.0.ipa", 38 * MB, { age: 1 }),
      b.dir("Patchbay.app", [b.dir("Contents", [b.file("Info.plist"), b.dir("MacOS", [bin(b, "patchbay", 22 * MB)]), b.dir("Resources", [b.file("icon.icns", 1.1 * MB)])])], { age: 1 }),
      b.file("Patchbay-0.1.0-universal.dmg", 41 * MB, { age: 1 }),
      lines(b, "build.log", 860 * KB, "build", 1),
    ]),
    b.dir("actions-runner", [b.file("run.sh", undefined, { perm: 0o755 }), b.dir("_diag", [lines(b, "Runner_20261017.log", 400 * KB, "app", 2 * MIN)])]),
    b.link("latest-build", "builds/Patchbay.app", { age: 1 }),
    b.link(".keychain-backup", "/Volumes/Backup/keychain", { age: 300 }),
  );
  return { key, label, root, home: "/Users/ci", user: "ci", group: "staff", remote: true, exec: true, lan: false, windows: false };
}

function genericLinux(key: string, label: string, user: string): Machine {
  return unixMachine({
    key,
    label,
    hostname: label,
    user,
    group: user,
    home: `/home/${user}`,
    lan: false,
    fill(root, sys, b) {
      put(
        b,
        root,
        `/home/${user}`,
        ...homeBasics(b),
        b.dir("projects", [b.dir("api", [b.file("README.md"), b.file("docker-compose.yml"), b.dir("src", [b.file("main.rs")])])]),
        b.dir("logs", [lines(b, "api.log", 2.4 * MB, "app", 3 * MIN)]),
        b.link("current", "projects/api", { age: 10 }),
        b.link("stale-link", "/srv/removed/thing", { age: 100 }),
      );
      put(sys, root, "/var/log", lines(sys, "app.log", 1.2 * MB, "app", 2 * MIN));
    },
  });
}

// ----------------------------------------------------------------- windows

function winBuildbox(key: string, label: string): Machine {
  const b = new Builder(`${key}/win`, null, null);
  const ro = { perm: 0o555 };
  const root = b.dir("/", [], ro);
  put(b, root, "/", b.dir("C:", [], { age: 400 }), b.dir("D:", [], { age: 200 }));
  put(
    b,
    root,
    "/C:/Users/rodrigo",
    b.dir("Desktop", [b.file("Patchbay.lnk", 2 * KB), b.file("notes.txt")]),
    b.dir("Documents", [b.dir("Visual Studio 2022", [b.dir("Projects")]), b.file("build-notes.md"), b.file("Release plan.docx", 88 * KB)]),
    b.dir("Downloads", [b.file("rustup-init.exe", 9.8 * MB, { age: 90 }), b.file("Git-2.47.0-64-bit.exe", 66 * MB, { age: 30 }), b.file("vs_BuildTools.exe", 4.2 * MB, { age: 100 })]),
    b.dir("source", [
      b.dir("repos", [
        b.dir("patchbay", [b.file("README.md"), b.file("Cargo.toml"), b.dir("src", [b.file("main.rs")]), b.file("build.ps1"), b.dir("target", [b.dir("release", [b.file("patchbay.exe", 18 * MB, { age: 0.1 })])])]),
        b.dir("playground", [b.file("hello.c"), b.file("Makefile")]),
      ]),
    ]),
    b.dir("AppData", [b.dir("Local", [b.dir("Temp")]), b.dir("Roaming", [b.dir("Code")]), b.dir("LocalLow")], { hidden: true }),
    b.file("NTUSER.DAT", 3.1 * MB, { hidden: true, age: 0.01 }),
    b.file(".gitconfig"),
    b.dir(".ssh", [b.file("authorized_keys"), b.file("known_hosts")]),
  );
  put(b, root, "/C:/Users/Public", b.dir("Documents"), b.dir("Desktop"));
  put(
    b,
    root,
    "/C:/Program Files",
    b.dir("Git", [b.dir("bin", [b.file("git.exe", 3.9 * MB)])], ro),
    b.dir("OpenSSH", [b.file("sshd.exe", 1.6 * MB), b.file("sftp-server.exe", 380 * KB)], ro),
    b.dir("Microsoft Visual Studio", [], ro),
  );
  put(
    b,
    root,
    "/C:/Windows",
    b.dir("System32", [
      b.dir("config", [], { denied: true, ...ro }),
      b.dir("drivers", [b.dir("etc", [b.file("hosts")])], ro),
      b.file("cmd.exe", 323 * KB),
      b.file("kernel32.dll", 790 * KB),
      b.dir("OpenSSH", [b.file("ssh.exe", 1.3 * MB)], ro),
    ], ro),
    b.dir("Temp", [b.file("setup.log", 44 * KB)]),
  );
  put(b, root, "/C:/ProgramData", b.dir("ssh", [b.file("sshd_config"), b.file("administrators_authorized_keys")]));
  for (const p of ["/C:/Program Files", "/C:/Windows"]) ensureDir(b, root, p).permissions = 0o555;
  put(
    b,
    root,
    "/D:/builds",
    b.file("patchbay-0.1.0-x64.msi", 12.4 * MB, { age: 1 }),
    b.file("patchbay-0.1.0-x64-setup.exe", 11.9 * MB, { age: 1 }),
    b.dir("nightly", Array.from({ length: 10 }, (_, i) => b.file(`patchbay-nightly-2026-10-${String(17 - i).padStart(2, "0")}.msi`, b.size(12 * MB, 13 * MB), { age: i + 0.1 }))),
  );
  return { key, label, root, home: "/C:/Users/rodrigo", user: "rodrigo", group: null, remote: true, exec: true, lan: true, windows: true };
}

// ------------------------------------------------------------ media-share

function mediaShare(key: string, label: string): Machine {
  const sys = new Builder(`${key}/sys`, "root", "root");
  const b = new Builder(`${key}/media`, "media", "media");
  const ro = { perm: 0o555 };
  const root = sys.dir("/", []);
  put(
    b,
    root,
    "/",
    b.dir("upload", [
      b.file("holiday.mp4", 380 * MB, { age: 0.2 }),
      b.text("README.txt", "Drop files here. Everything else on this share is read-only.\nUploads are moved to the library every night at 03:00.\n", { age: 90 }),
      b.dir("incoming", [b.file("family-photos-2026.zip", 640 * MB, { age: 1 })]),
    ], { perm: 0o775 }),
    b.dir("movies", [
      b.file("Big Buck Bunny (2008).mkv", 2.6 * GB, { age: 500 }),
      b.file("Sintel (2010).mkv", 1.9 * GB, { age: 480 }),
      b.file("Tears of Steel (2012).mp4", 1.2 * GB, { age: 460 }),
    ], ro),
    b.dir("music", [
      b.dir("Kevin MacLeod", Array.from({ length: 8 }, (_, i) => b.file(`Track ${String(i + 1).padStart(2, "0")}.mp3`, b.size(4 * MB, 9 * MB))), ro),
      b.dir("Podcasts", [b.file("episode-001.mp3", 58 * MB)], ro),
    ], ro),
    b.dir("photos", [
      b.dir("2024", Array.from({ length: 12 }, (_, i) => b.file(`DSC_${4100 + i}.jpg`, b.size(3 * MB, 7 * MB), { age: 600 - i })), ro),
      b.dir("2025", Array.from({ length: 16 }, (_, i) => b.file(`DSC_${5200 + i}.jpg`, b.size(3 * MB, 7 * MB), { age: 300 - i })), ro),
    ], ro),
  );
  return { key, label, root, home: "/", user: "media", group: "media", remote: true, exec: false, lan: true, windows: false, writableRoots: ["/upload"] };
}

// ------------------------------------------------------------------ entry

/** Builds the machine for a host (null = this computer). */
export function buildMachine(host: { id: string; name: string; user: string | null } | null): Machine {
  if (!host) return localMachine();
  const { id, name } = host;
  switch (name) {
    case "bastion":
      return bastion(id, name);
    case "db-internal":
      return dbInternal(id, name);
    case "prod-web-01":
      return prodWeb(id, name);
    case "raspberrypi":
      return raspberrypi(id, name);
    case "legacy-nas":
      return legacyNas(id, name);
    case "build-mac":
      return buildMac(id, name);
    case "win-buildbox":
      return winBuildbox(id, name);
    case "media-share":
      return mediaShare(id, name);
    default:
      return genericLinux(id, name, host.user ?? "user");
  }
}

