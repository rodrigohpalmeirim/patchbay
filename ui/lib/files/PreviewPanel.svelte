<!--
  Quick-look side panel for the focused entry: images (via fsRead → Blob URL),
  text with line numbers (first 256 KiB), a hex peek for binary files, and
  metadata. Blob URLs are revoked when the entry changes or on unmount.
-->
<script lang="ts">
  import Binary from "@lucide/svelte/icons/binary";
  import CircleAlert from "@lucide/svelte/icons/circle-alert";
  import ImageOff from "@lucide/svelte/icons/image-off";
  import LoaderCircle from "@lucide/svelte/icons/loader-circle";
  import X from "@lucide/svelte/icons/x";
  import { attempt, call } from "../stores/backend";
  import type { FileBrowser } from "../stores/files.svelte";
  import type { FileEntry } from "../types";
  import { sentence, toCommandError } from "../util/errors";
  import { iconFor, imageMime, isDirLike } from "../util/fileIcons";
  import { formatBytes, formatExact, octal, permString, plural } from "../util/format";

  let { browser: b, onclose }: { browser: FileBrowser; onclose: () => void } = $props();

  const IMAGE_LIMIT = 16 * 1024 * 1024;
  const TEXT_LIMIT = 256 * 1024;
  const HEX_BYTES = 512;

  type Content =
    | { kind: "none" }
    | { kind: "loading" }
    | { kind: "image"; url: string; w?: number; h?: number }
    | { kind: "text"; text: string; lines: number; truncated: boolean }
    | { kind: "binary"; hex: string }
    | { kind: "tooBig"; limit: number }
    | { kind: "dir"; count: number | null }
    | { kind: "error"; message: string };

  const entry = $derived<FileEntry | null>(
    (b.cursor && b.rows[b.rowIndex.get(b.cursor) ?? -1]) || (b.selectedEntries.length === 1 ? b.selectedEntries[0] : null),
  );
  const multi = $derived(b.selectedEntries.length > 1 ? b.selectedEntries : null);

  let content = $state<Content>({ kind: "none" });
  let seq = 0;

  function looksBinary(bytes: Uint8Array): boolean {
    const n = Math.min(bytes.length, 8192);
    if (n === 0) return false;
    let odd = 0;
    for (let i = 0; i < n; i++) {
      const c = bytes[i];
      if (c === 0) return true;
      if (c < 7 || (c > 13 && c < 32 && c !== 27)) odd++;
    }
    return odd / n > 0.1;
  }

  function hexDump(bytes: Uint8Array): string {
    const lines: string[] = [];
    for (let off = 0; off < Math.min(bytes.length, HEX_BYTES); off += 16) {
      const chunk = bytes.subarray(off, off + 16);
      const hex = [...chunk].map((x) => x.toString(16).padStart(2, "0")).join(" ");
      const ascii = [...chunk].map((x) => (x >= 32 && x < 127 ? String.fromCharCode(x) : "·")).join("");
      lines.push(`${off.toString(16).padStart(8, "0")}  ${hex.padEnd(47)}  ${ascii}`);
    }
    return lines.join("\n");
  }

  // Load content whenever the previewed entry changes (path + mtime + size).
  const key = $derived(entry ? `${entry.path}|${entry.modified}|${entry.size}` : "");
  $effect(() => {
    void key;
    const e = entry;
    const my = ++seq;
    let url: string | null = null;
    if (!e) {
      content = { kind: "none" };
      return;
    }
    if (isDirLike(e)) {
      content = { kind: "dir", count: null };
      void attempt((be) => be.fsList(b.hostId, e.path), { toast: false }).then((l) => {
        if (my === seq) content = { kind: "dir", count: l ? l.entries.length : null };
      });
      return;
    }
    if (e.kind === "symlink" && e.targetKind == null) {
      content = { kind: "error", message: "This link points to something that doesn't exist." };
      return;
    }
    const mime = imageMime(e.name);
    if (mime && e.size > IMAGE_LIMIT) {
      content = { kind: "tooBig", limit: IMAGE_LIMIT };
      return;
    }
    content = { kind: "loading" };
    call((be) => be.fsRead(b.hostId, e.path, mime ? IMAGE_LIMIT : TEXT_LIMIT), { toast: false })
      .then((bytes) => {
        if (my !== seq) return;
        if (mime) {
          url = URL.createObjectURL(new Blob([bytes as BlobPart], { type: mime }));
          content = { kind: "image", url };
        } else if (looksBinary(bytes)) {
          content = { kind: "binary", hex: hexDump(bytes) };
        } else {
          let text = new TextDecoder("utf-8", { fatal: false }).decode(bytes);
          const truncated = e.size > bytes.length;
          // Don't end mid-line when truncated.
          if (truncated) text = text.slice(0, Math.max(0, text.lastIndexOf("\n")));
          const lines = text ? text.split("\n").length - (text.endsWith("\n") ? 1 : 0) : 0;
          content = { kind: "text", text: text.endsWith("\n") ? text.slice(0, -1) : text, lines, truncated };
        }
      })
      .catch((err) => {
        if (my === seq) content = { kind: "error", message: sentence(toCommandError(err).message) };
      });
    return () => {
      seq++;
      if (url) URL.revokeObjectURL(url);
    };
  });

  const lineNumbers = $derived(
    content.kind === "text" ? Array.from({ length: Math.max(1, content.lines) }, (_, i) => i + 1).join("\n") : "",
  );

  function onload(e: Event) {
    const img = e.currentTarget as HTMLImageElement;
    if (content.kind === "image") content = { ...content, w: img.naturalWidth, h: img.naturalHeight };
  }

  const multiSize = $derived(multi ? multi.reduce((n, e) => n + (isDirLike(e) ? 0 : e.size), 0) : 0);
</script>

<aside class="preview" aria-label="Preview">
  <header class="head">
    {#if entry && !multi}
      {@const ic = iconFor(entry)}
      <span class="head-icon"><ic.icon size={15} /></span>
      <span class="head-title" title={entry.name}>{entry.name}</span>
    {:else}
      <span class="head-title muted">Preview</span>
    {/if}
    <button class="icon-btn sm" aria-label="Close preview" title="Close preview (Space)" onclick={onclose}><X size={14} /></button>
  </header>

  {#if multi}
    <div class="empty">
      <p class="empty-title">{plural(multi.length, "item")} selected</p>
      <p class="empty-text">{formatBytes(multiSize)} in files</p>
    </div>
  {:else if !entry}
    <div class="empty">
      <p class="empty-title">Nothing selected</p>
      <p class="empty-text">Select a file to preview it here.</p>
    </div>
  {:else}
    <div class="content">
      {#if content.kind === "loading"}
        <div class="center"><LoaderCircle size={18} class="spin" /></div>
      {:else if content.kind === "image"}
        <div class="image">
          <img src={content.url} alt={entry.name} {onload} />
        </div>
      {:else if content.kind === "text"}
        <div class="code" role="document" aria-label="Contents of {entry.name}">
          <pre class="gutter" aria-hidden="true">{lineNumbers}</pre>
          <pre class="text selectable">{content.text}</pre>
        </div>
      {:else if content.kind === "binary"}
        <div class="binary">
          <div class="note"><Binary size={14} /> Binary file — first {Math.min(entry.size, HEX_BYTES)} bytes</div>
          <pre class="hex selectable">{content.hex}</pre>
        </div>
      {:else if content.kind === "tooBig"}
        <div class="center col">
          <ImageOff size={20} />
          <p>Too large to preview</p>
          <span>Images over {formatBytes(content.limit)} aren’t loaded.</span>
        </div>
      {:else if content.kind === "dir"}
        {@const ic = iconFor(entry)}
        <div class="center col">
          <span class="big-icon"><ic.icon size={34} strokeWidth={1.5} /></span>
          <p>{content.count == null ? "Folder" : plural(content.count, "item")}</p>
        </div>
      {:else if content.kind === "error"}
        <div class="center col">
          <CircleAlert size={20} />
          <p>Can’t preview</p>
          <span class="selectable">{content.message}</span>
        </div>
      {/if}
    </div>
    {#if content.kind === "text" && content.truncated}
      <div class="trunc">Showing the first {formatBytes(TEXT_LIMIT)} of {formatBytes(entry.size)}</div>
    {/if}
    <dl class="meta">
      <div><dt>Kind</dt><dd>{entry.kind === "symlink" ? `Link to ${entry.targetKind ?? "nothing"}` : entry.kind === "dir" ? "Folder" : entry.kind === "file" ? "File" : "Other"}</dd></div>
      {#if !isDirLike(entry)}
        <div><dt>Size</dt><dd title="{entry.size.toLocaleString()} bytes">{formatBytes(entry.size)}{#if entry.size >= 1024}<span class="sub">{" · "}{entry.size.toLocaleString()} bytes</span>{/if}</dd></div>
      {/if}
      {#if content.kind === "image" && content.w}
        <div><dt>Dimensions</dt><dd>{content.w} × {content.h}</dd></div>
      {/if}
      <div><dt>Modified</dt><dd>{formatExact(entry.modified)}</dd></div>
      <div>
        <dt>Permissions</dt>
        <dd class="mono">{permString(entry.permissions, entry.kind)}{#if entry.permissions != null}<span class="sub">{" "}({octal(entry.permissions)})</span>{/if}</dd>
      </div>
      {#if entry.owner || entry.group}
        <div><dt>Owner</dt><dd>{entry.owner ?? "—"}<span class="sub">{" · "}{entry.group ?? "—"}</span></dd></div>
      {/if}
      {#if entry.linkTarget}
        <div><dt>Target</dt><dd class="mono selectable wrap">{entry.linkTarget}</dd></div>
      {/if}
      <div><dt>Path</dt><dd class="mono selectable wrap">{entry.path}</dd></div>
    </dl>
  {/if}
</aside>

<style>
  .preview {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-width: 0;
    background: var(--bg);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 30px;
    padding: 0 6px 0 12px;
    border-bottom: 1px solid var(--border-subtle);
    flex: none;
  }
  .head-icon {
    display: flex;
    color: var(--text-muted);
  }
  .head-title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-weight: var(--weight-medium);
  }
  .muted {
    color: var(--text-muted);
    font-weight: normal;
  }
  .content {
    flex: 1 1 auto;
    min-height: 120px;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .center {
    flex: 1;
    display: grid;
    place-items: center;
    color: var(--text-subtle);
  }
  .center.col {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 4px;
    padding: 16px;
    text-align: center;
  }
  .center.col p {
    color: var(--text);
    font-weight: var(--weight-medium);
  }
  .center.col span {
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
  }
  .big-icon {
    display: flex;
    color: #3b82f6;
    margin-bottom: 4px;
  }
  .image {
    flex: 1;
    display: grid;
    place-items: center;
    min-height: 0;
    padding: 12px;
    background-color: var(--bg-subtle);
    background-image:
      linear-gradient(45deg, var(--bg-inset) 25%, transparent 25%),
      linear-gradient(-45deg, var(--bg-inset) 25%, transparent 25%),
      linear-gradient(45deg, transparent 75%, var(--bg-inset) 75%),
      linear-gradient(-45deg, transparent 75%, var(--bg-inset) 75%);
    background-size: 16px 16px;
    background-position:
      0 0,
      0 8px,
      8px -8px,
      -8px 0;
  }
  .image img {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
    border-radius: var(--radius-xs);
  }
  .code {
    flex: 1;
    display: flex;
    min-height: 0;
    overflow: auto;
    background: var(--bg-inset);
    font-family: var(--font-mono);
    font-size: 11.5px;
    line-height: 1.55;
  }
  .code pre {
    margin: 0;
    padding: 8px 0;
    font: inherit;
  }
  .gutter {
    position: sticky;
    left: 0;
    flex: none;
    padding: 8px 8px 8px 10px !important;
    min-width: 36px;
    text-align: right;
    color: var(--text-subtle);
    background: var(--bg-inset);
    border-right: 1px solid var(--border-subtle);
    user-select: none;
  }
  .text {
    padding: 8px 12px !important;
    color: var(--text);
    white-space: pre;
    tab-size: 4;
  }
  .binary {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .note {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 12px;
    color: var(--text-muted);
    font-size: var(--text-sm);
    border-bottom: 1px solid var(--border-subtle);
  }
  .hex {
    flex: 1;
    margin: 0;
    padding: 8px 12px;
    overflow: auto;
    background: var(--bg-inset);
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: 11px;
    line-height: 1.6;
  }
  .trunc {
    padding: 5px 12px;
    border-top: 1px solid var(--border-subtle);
    background: var(--bg-subtle);
    color: var(--text-muted);
    font-size: var(--text-xs);
    flex: none;
  }
  .meta {
    flex: none;
    max-height: 45%;
    margin: 0;
    padding: 10px 12px 12px;
    overflow-y: auto;
    border-top: 1px solid var(--border-subtle);
    font-size: var(--text-sm);
  }
  .meta div {
    display: grid;
    grid-template-columns: 84px 1fr;
    gap: 8px;
    padding: 3px 0;
  }
  .meta dt {
    color: var(--text-subtle);
  }
  .meta dd {
    margin: 0;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meta dd.mono {
    font-size: 11.5px;
  }
  .meta dd.wrap {
    white-space: normal;
    overflow-wrap: anywhere;
  }
  .sub {
    color: var(--text-subtle);
  }
  .empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 2px;
    padding: 16px;
    text-align: center;
  }
  .empty-title {
    font-weight: var(--weight-medium);
  }
  .empty-text {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
</style>
