// Icon + colour tone for a file entry, by kind and extension.

import type { LucideIcon } from "@lucide/svelte";
import File from "@lucide/svelte/icons/file";
import FileArchive from "@lucide/svelte/icons/file-archive";
import FileBox from "@lucide/svelte/icons/file-box";
import FileBraces from "@lucide/svelte/icons/file-braces";
import FileCode from "@lucide/svelte/icons/file-code";
import FileCog from "@lucide/svelte/icons/file-cog";
import FileImage from "@lucide/svelte/icons/file-image";
import FileKey from "@lucide/svelte/icons/file-key";
import FileMusic from "@lucide/svelte/icons/file-music";
import FilePlay from "@lucide/svelte/icons/file-play";
import FileSpreadsheet from "@lucide/svelte/icons/file-spreadsheet";
import FileTerminal from "@lucide/svelte/icons/file-terminal";
import FileText from "@lucide/svelte/icons/file-text";
import FileX from "@lucide/svelte/icons/file-x";
import Folder from "@lucide/svelte/icons/folder";
import FolderSymlink from "@lucide/svelte/icons/folder-symlink";
import Disc3 from "@lucide/svelte/icons/disc-3";
import type { FileEntry } from "../types";
import { extension } from "./paths";

export type IconTone = "folder" | "image" | "code" | "archive" | "media" | "doc" | "key" | "exec" | "plain" | "broken";

const EXT: Record<string, [LucideIcon, IconTone]> = {};
const add = (exts: string, icon: LucideIcon, tone: IconTone) => {
  for (const e of exts.split(" ")) EXT[e] = [icon, tone];
};
add("png jpg jpeg gif webp svg bmp ico tif tiff heic avif psd raw cr2 nef", FileImage, "image");
add(
  "rs ts tsx js jsx mjs cjs svelte vue py rb go java kt c h cc cpp hpp cs swift php lua pl sql html htm css scss less zig ex exs hs ml",
  FileCode,
  "code",
);
add("json jsonc json5 toml yaml yml xml lock plist", FileBraces, "code");
add("conf cfg ini env properties service socket timer rules", FileCog, "plain");
add("sh bash zsh fish ps1 bat cmd command", FileTerminal, "exec");
add("zip gz tgz bz2 xz zst 7z rar tar deb rpm apk jar war whl", FileArchive, "archive");
add("dmg pkg msi exe appimage ipa app", FileBox, "archive");
add("iso img vhd vhdx vmdk qcow2", Disc3, "archive");
add("mp4 mkv mov avi webm m4v flv wmv", FilePlay, "media");
add("mp3 flac wav ogg m4a aac opus aiff", FileMusic, "media");
add("txt md markdown rst log csv tsv rtf pdf doc docx odt pages tex nfo", FileText, "doc");
add("xls xlsx ods numbers", FileSpreadsheet, "doc");
add("pem key crt cer pub der p12 pfx gpg asc kdbx", FileKey, "key");

const KEY_NAMES = /^(id_[a-z0-9_]+|authorized_keys|known_hosts)$/i;

export function iconFor(entry: Pick<FileEntry, "name" | "kind" | "targetKind" | "permissions">): {
  icon: LucideIcon;
  tone: IconTone;
} {
  if (entry.kind === "dir") return { icon: Folder, tone: "folder" };
  if (entry.kind === "symlink") {
    if (entry.targetKind === "dir") return { icon: FolderSymlink, tone: "folder" };
    if (entry.targetKind == null) return { icon: FileX, tone: "broken" };
  }
  if (KEY_NAMES.test(entry.name)) return { icon: FileKey, tone: "key" };
  const hit = EXT[extension(entry.name)];
  if (hit) return { icon: hit[0], tone: hit[1] };
  if (entry.kind === "file" && entry.permissions != null && entry.permissions & 0o111) {
    return { icon: FileTerminal, tone: "exec" };
  }
  return { icon: File, tone: "plain" };
}

/** Is this entry something we navigate into (dir or link to a dir)? */
export function isDirLike(entry: Pick<FileEntry, "kind" | "targetKind">): boolean {
  return entry.kind === "dir" || (entry.kind === "symlink" && entry.targetKind === "dir");
}

const IMAGE_MIME: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  svg: "image/svg+xml",
  bmp: "image/bmp",
  ico: "image/x-icon",
  avif: "image/avif",
};

export function imageMime(name: string): string | null {
  return IMAGE_MIME[extension(name)] ?? null;
}
