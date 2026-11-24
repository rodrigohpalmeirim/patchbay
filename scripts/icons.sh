#!/usr/bin/env bash
# Regenerate src-tauri/icons/ from the logo sources (run in the dev container):
#
#   scripts/dev.sh scripts/icons.sh
#
#   assets/logo.svg        full-bleed tile: icon.ico (32-256), Linux PNGs, icon.png (window icon)
#   assets/logo-16.svg     pixel-fitted 16px version (icon.ico 16x16 frame)
#   assets/logo-24.svg     pixel-fitted 24px version (icon.ico 24x24 frame)
#   assets/logo-macos.svg  same artwork on Apple's icon grid (padding, squircle, shadow): icon.icns
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

out=src-tauri/icons
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# macOS: only the .icns from the padded source.
npx tauri icon assets/logo-macos.svg -o "$tmp/mac" >/dev/null
cp "$tmp/mac/icon.icns" "$out/icon.icns"

# Linux/window icon: full-bleed PNGs.
npx tauri icon assets/logo.svg -o "$tmp/full" >/dev/null
cp "$tmp/full/"{32x32.png,128x128.png,128x128@2x.png,icon.png} "$out/"

# Windows: .ico assembled from PNG frames (pixel-fitted 16/24, full-bleed 32+).
npx tauri icon assets/logo-16.svg -o "$tmp/ico" -p 16 >/dev/null
npx tauri icon assets/logo-24.svg -o "$tmp/ico" -p 24 >/dev/null
npx tauri icon assets/logo.svg -o "$tmp/ico" -p 32 -p 48 -p 64 -p 256 >/dev/null
node - "$out/icon.ico" "$tmp/ico/"{16,24,32,48,64,256}x*.png <<'EOF'
// ICO with PNG-compressed frames (supported since Windows Vista).
const fs = require("fs");
const [outPath, ...files] = process.argv.slice(2);
const pngs = files.map((f) => fs.readFileSync(f));
const header = Buffer.alloc(6 + 16 * pngs.length);
header.writeUInt16LE(1, 2);
header.writeUInt16LE(pngs.length, 4);
let offset = header.length;
pngs.forEach((png, i) => {
  const size = png.readUInt32BE(16);
  const e = 6 + 16 * i;
  header[e] = header[e + 1] = size >= 256 ? 0 : size;
  header.writeUInt16LE(1, e + 4);
  header.writeUInt16LE(32, e + 6);
  header.writeUInt32LE(png.length, e + 8);
  header.writeUInt32LE(offset, e + 12);
  offset += png.length;
});
fs.writeFileSync(outPath, Buffer.concat([header, ...pngs]));
EOF

ls -l "$out"
