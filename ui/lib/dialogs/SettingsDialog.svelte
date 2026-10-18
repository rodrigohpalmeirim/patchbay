<script lang="ts">
  import Monitor from "@lucide/svelte/icons/monitor";
  import Moon from "@lucide/svelte/icons/moon";
  import Sun from "@lucide/svelte/icons/sun";
  import X from "@lucide/svelte/icons/x";
  import { onMount } from "svelte";
  import { isTauri } from "../api";
  import Dialog from "../components/Dialog.svelte";
  import Logo from "../components/Logo.svelte";
  import RotateCcw from "@lucide/svelte/icons/rotate-ccw";
  import {
    DEFAULT_TERMINAL_FONT,
    settings,
    TERMINAL_DEFAULTS,
    TERMINAL_LIMITS,
    type CursorStyle,
    type ThemePref,
  } from "../stores/settings.svelte";
  import { ui } from "../stores/ui.svelte";
  import { appSettings, MAX_TRANSFERS_LIMITS } from "../stores/appSettings.svelte";
  import { hosts } from "../stores/hosts.svelte";
  import type { ReconnectMode } from "../stores/settings.svelte";

  const themes: { value: ThemePref; label: string; icon: typeof Sun }[] = [
    { value: "system", label: "System", icon: Monitor },
    { value: "light", label: "Light", icon: Sun },
    { value: "dark", label: "Dark", icon: Moon },
  ];

  const cursors: { value: CursorStyle; label: string }[] = [
    { value: "block", label: "Block" },
    { value: "bar", label: "Bar" },
    { value: "underline", label: "Underline" },
  ];
  const lineHeights = [1, 1.1, 1.2, 1.3, 1.4, 1.5, 1.6, 1.8, 2];
  const fontSuggestions = [
    "JetBrains Mono",
    "Fira Code",
    "Cascadia Code",
    "Cascadia Mono",
    "SF Mono",
    "Menlo",
    "Monaco",
    "Consolas",
    "Ubuntu Mono",
    "DejaVu Sans Mono",
    "Hack",
    "Source Code Pro",
    "IBM Plex Mono",
    "Iosevka",
  ];

  const t = $derived(settings.terminal);
  const isDefault = $derived(JSON.stringify(t) === JSON.stringify(TERMINAL_DEFAULTS));
  const previewFont = $derived(t.fontFamily.trim() || DEFAULT_TERMINAL_FONT);

  function num(e: Event, key: "fontSize" | "scrollback") {
    const input = e.currentTarget as HTMLInputElement;
    const v = Number(input.value);
    if (input.value.trim() === "" || !Number.isFinite(v)) {
      input.value = String(settings.terminal[key]);
      return;
    }
    settings.setTerminal({ [key]: key === "fontSize" ? Math.round(v * 2) / 2 : Math.round(v) });
    input.value = String(settings.terminal[key]);
  }

  function cursorKey(e: KeyboardEvent) {
    const i = cursors.findIndex((c) => c.value === t.cursorStyle);
    let next = -1;
    if (e.key === "ArrowRight" || e.key === "ArrowDown") next = (i + 1) % cursors.length;
    else if (e.key === "ArrowLeft" || e.key === "ArrowUp") next = (i - 1 + cursors.length) % cursors.length;
    else return;
    e.preventDefault();
    settings.setTerminal({ cursorStyle: cursors[next].value });
    (e.currentTarget as HTMLElement).querySelector<HTMLElement>(`[data-value="${cursors[next].value}"]`)?.focus();
  }

  const app = $derived(appSettings.value);

  let sharingBusy = $state(false);
  async function setSharing(input: HTMLInputElement) {
    sharingBusy = true;
    try {
      if (!(await hosts.setSharing(input.checked))) input.checked = !input.checked;
    } finally {
      sharingBusy = false;
    }
  }
  const transferChoices = Array.from({ length: MAX_TRANSFERS_LIMITS[1] - MAX_TRANSFERS_LIMITS[0] + 1 }, (_, i) => i + MAX_TRANSFERS_LIMITS[0]);
  const sessionPrefs = $derived(settings.session);
  const reconnectModes: { value: ReconnectMode; label: string; desc: string }[] = [
    { value: "lazy", label: "When a restored tab is opened", desc: "Only hosts you look at connect." },
    { value: "eager", label: "Immediately, for all restored tabs", desc: "Every host with a restored files or terminal tab connects at launch." },
  ];

  let version = $state<string | null>(null);
  onMount(() => {
    // Pick up changes made while the dialog was closed (e.g. from the tray).
    if (!appSettings.value) void appSettings.load();
  });
  onMount(async () => {
    if (!isTauri) return;
    try {
      const { getVersion } = await import("@tauri-apps/api/app");
      version = await getVersion();
    } catch {
      version = null;
    }
  });

  function onkeydown(e: KeyboardEvent) {
    const i = themes.findIndex((t) => t.value === settings.theme);
    let next = -1;
    if (e.key === "ArrowRight" || e.key === "ArrowDown") next = (i + 1) % themes.length;
    else if (e.key === "ArrowLeft" || e.key === "ArrowUp") next = (i - 1 + themes.length) % themes.length;
    else return;
    e.preventDefault();
    settings.setTheme(themes[next].value);
    (e.currentTarget as HTMLElement).querySelector<HTMLElement>(`[data-value="${themes[next].value}"]`)?.focus();
  }
</script>

<Dialog onclose={() => (ui.settingsOpen = false)} labelledby="settings-title" width={480}>
  <div class="dialog-header">
    <div class="head">
      <h2 class="dialog-title" id="settings-title">Settings</h2>
    </div>
    <button class="icon-btn close" aria-label="Close settings" onclick={() => (ui.settingsOpen = false)}><X size={15} /></button>
  </div>
  <div class="dialog-body">
    <div class="row">
      <div>
        <div class="label" id="theme-label">Appearance</div>
        <div class="desc">“System” follows your OS setting.</div>
      </div>
    </div>
    <!-- svelte-ignore a11y_interactive_supports_focus -->
    <div class="segmented" role="radiogroup" aria-labelledby="theme-label" {onkeydown}>
      {#each themes as t (t.value)}
        {@const checked = settings.theme === t.value}
        <button
          type="button"
          role="radio"
          aria-checked={checked}
          tabindex={checked ? 0 : -1}
          data-value={t.value}
          class:checked
          onclick={() => settings.setTheme(t.value)}
          data-autofocus={checked ? true : undefined}
        >
          <t.icon size={14} />
          {t.label}
        </button>
      {/each}
    </div>

    <section class="section" aria-labelledby="app-title">
      <div class="section-head">
        <h3 class="section-title" id="app-title">App</h3>
        {#if appSettings.saving}<span class="saving" aria-live="polite">Saving…</span>{/if}
      </div>
      {#if app}
        <div class="checks">
          <label class="checkbox opt">
            <input type="checkbox" checked={app.closeToTray} onchange={(e) => void appSettings.set({ closeToTray: e.currentTarget.checked })} />
            <span>
              <span class="opt-label">Keep running in the system tray when the window is closed</span>
              <span class="desc">Connections and port forwards stay up in the background.</span>
            </span>
          </label>
          <label class="checkbox opt">
            <input type="checkbox" checked={app.launchAtLogin} onchange={(e) => void appSettings.set({ launchAtLogin: e.currentTarget.checked })} />
            <span class="opt-label">Launch at login</span>
          </label>
          <label class="checkbox opt nested" class:disabled={!app.launchAtLogin}>
            <input
              type="checkbox"
              checked={app.launchAtLogin && app.startMinimized}
              disabled={!app.launchAtLogin}
              onchange={(e) => void appSettings.set({ startMinimized: e.currentTarget.checked })}
            />
            <span>
              <span class="opt-label">Start minimized</span>
              <span class="desc">Open to the tray instead of showing the window.</span>
            </span>
          </label>
        </div>
        <div class="inline-field">
          <span>
            <label for="st-transfers" class="opt-label">Simultaneous transfers</label>
            <span class="desc">More files wait in the queue until one finishes.</span>
          </span>
          <select
            id="st-transfers"
            class="input select"
            value={app.maxConcurrentTransfers}
            onchange={(e) => void appSettings.set({ maxConcurrentTransfers: Number(e.currentTarget.value) })}
          >
            {#each transferChoices as n (n)}<option value={n}>{n}</option>{/each}
          </select>
        </div>
      {:else}
        <p class="desc">Couldn’t load these settings from Patchbay’s backend.</p>
      {/if}
    </section>

    <section class="section" aria-labelledby="hosts-title">
      <div class="section-head">
        <h3 class="section-title" id="hosts-title">Hosts</h3>
      </div>
      <div class="checks">
        <label class="checkbox opt">
          <input
            type="checkbox"
            checked={hosts.sharing?.shared ?? false}
            disabled={!hosts.sharing || sharingBusy}
            onchange={(e) => void setSharing(e.currentTarget)}
          />
          <span>
            <span class="opt-label">Let ssh use the hosts saved in Patchbay</span>
            <span class="desc">
              Adds <code>Include patchbay.conf</code> to the top of <code>~/.ssh/config</code>, so commands like
              <code>ssh my-server</code> work in a terminal too.
            </span>
          </span>
        </label>
        {#if app}
          <label class="checkbox opt">
            <input type="checkbox" checked={app.tailscale} onchange={(e) => void appSettings.set({ tailscale: e.currentTarget.checked })} />
            <span>
              <span class="opt-label">Show devices on my tailnet</span>
              <span class="desc">Lists the machines on your Tailscale network when Tailscale is installed and running.</span>
            </span>
          </label>
        {/if}
      </div>
    </section>

    <section class="section" aria-labelledby="session-title">
      <div class="section-head">
        <h3 class="section-title" id="session-title">Session</h3>
      </div>
      <div class="checks">
        <label class="checkbox opt">
          <input type="checkbox" checked={sessionPrefs.restoreTabs} onchange={(e) => settings.setSession({ restoreTabs: e.currentTarget.checked })} />
          <span>
            <span class="opt-label">On launch, restore previous tabs</span>
            <span class="desc">Files tabs reopen at their folder; terminals start fresh shells in their last directory.</span>
          </span>
        </label>
      </div>
      <div class="radios" role="radiogroup" aria-labelledby="st-reconnect-label" class:disabled={!sessionPrefs.restoreTabs}>
        <span class="label-cell" id="st-reconnect-label">Reconnect restored tabs</span>
        {#each reconnectModes as m (m.value)}
          <label class="checkbox opt">
            <input
              type="radio"
              name="st-reconnect"
              value={m.value}
              checked={sessionPrefs.reconnect === m.value}
              disabled={!sessionPrefs.restoreTabs}
              onchange={() => settings.setSession({ reconnect: m.value })}
            />
            <span>
              <span class="opt-label">{m.label}{m.value === "lazy" ? " (default)" : ""}</span>
              <span class="desc">{m.desc}</span>
            </span>
          </label>
        {/each}
      </div>
    </section>

    <section class="section" aria-labelledby="term-title">
      <div class="section-head">
        <h3 class="section-title" id="term-title">Terminal</h3>
        <button class="btn btn-ghost btn-sm" disabled={isDefault} onclick={() => settings.resetTerminal()}><RotateCcw size={12} /> Reset</button>
      </div>

      <div class="grid">
        <label for="st-font">Font</label>
        <div class="control">
          <input
            id="st-font"
            class="input"
            list="st-font-list"
            value={t.fontFamily}
            placeholder="Default monospace"
            spellcheck="false"
            autocomplete="off"
            onchange={(e) => settings.setTerminal({ fontFamily: e.currentTarget.value })}
            onkeydown={(e) => e.key === "Enter" && settings.setTerminal({ fontFamily: e.currentTarget.value })}
          />
          <datalist id="st-font-list">
            {#each fontSuggestions as f (f)}<option value={f}></option>{/each}
          </datalist>
        </div>

        <label for="st-size">Size</label>
        <div class="control inline">
          <input
            id="st-size"
            class="input num"
            type="number"
            min={TERMINAL_LIMITS.fontSize[0]}
            max={TERMINAL_LIMITS.fontSize[1]}
            step="0.5"
            value={t.fontSize}
            onchange={(e) => num(e, "fontSize")}
          />
          <label for="st-lh" class="sub">Line height</label>
          <select id="st-lh" class="input select" value={t.lineHeight} onchange={(e) => settings.setTerminal({ lineHeight: Number(e.currentTarget.value) })}>
            {#each lineHeights as lh (lh)}<option value={lh}>{lh.toFixed(1)}</option>{/each}
          </select>
        </div>

        <span class="label-cell" id="st-cursor">Cursor</span>
        <div class="control inline">
          <!-- svelte-ignore a11y_interactive_supports_focus -->
          <div class="segmented small" role="radiogroup" aria-labelledby="st-cursor" onkeydown={cursorKey}>
            {#each cursors as c (c.value)}
              {@const checked = t.cursorStyle === c.value}
              <button
                type="button"
                role="radio"
                aria-checked={checked}
                tabindex={checked ? 0 : -1}
                data-value={c.value}
                class:checked
                onclick={() => settings.setTerminal({ cursorStyle: c.value })}
              >
                <span class="cursor-glyph {c.value}" aria-hidden="true"></span>
                {c.label}
              </button>
            {/each}
          </div>
          <label class="checkbox"><input type="checkbox" checked={t.cursorBlink} onchange={(e) => settings.setTerminal({ cursorBlink: e.currentTarget.checked })} /> Blink</label>
        </div>

        <label for="st-scroll">Scrollback</label>
        <div class="control inline">
          <input
            id="st-scroll"
            class="input num wide"
            type="number"
            min={TERMINAL_LIMITS.scrollback[0]}
            max={TERMINAL_LIMITS.scrollback[1]}
            step="1000"
            value={t.scrollback}
            onchange={(e) => num(e, "scrollback")}
          />
          <span class="unit">lines</span>
        </div>

        <label for="st-bell">Bell</label>
        <div class="control">
          <select id="st-bell" class="input select" value={t.bell} onchange={(e) => settings.setTerminal({ bell: e.currentTarget.value === "none" ? "none" : "visual" })}>
            <option value="visual">Flash the pane</option>
            <option value="none">Off</option>
          </select>
        </div>
      </div>

      <div class="checks">
        <label class="checkbox">
          <input type="checkbox" checked={t.copyOnSelect} onchange={(e) => settings.setTerminal({ copyOnSelect: e.currentTarget.checked })} />
          Copy text to the clipboard when selected
        </label>
        <label class="checkbox">
          <input
            type="checkbox"
            checked={t.confirmMultilinePaste}
            onchange={(e) => settings.setTerminal({ confirmMultilinePaste: e.currentTarget.checked })}
          />
          Ask before pasting several lines into a shell
        </label>
      </div>

      <div class="preview" style:font-family={previewFont} style:font-size="{t.fontSize}px" style:line-height={t.lineHeight} aria-hidden="true">
        <div><span class="p-user">ops@bastion</span>:<span class="p-path">~/deploy</span>$ ls</div>
        <div><span class="p-dir">current</span>  <span class="p-dir">releases</span>  <span class="p-dir">shared</span>  <span class="p-exe">deploy.sh</span></div>
        <div><span class="p-user">ops@bastion</span>:<span class="p-path">~/deploy</span>$ <span class="p-cursor {t.cursorStyle}">&nbsp;</span></div>
      </div>
    </section>

    <div class="about">
      <Logo size={28} />
      <div>
        <div class="label">Patchbay {version ?? ""}</div>
        <div class="desc">Open source SSH client · GPL-3.0</div>
      </div>
    </div>
  </div>
</Dialog>

<style>
  .head {
    flex: 1;
    align-self: center;
  }
  .close {
    margin: -3px -6px 0 0;
  }
  .label {
    font-weight: var(--weight-medium);
  }
  .desc {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .segmented {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 2px;
    margin-top: 10px;
    padding: 3px;
    border-radius: var(--radius-md);
    background: var(--bg-inset);
    border: 1px solid var(--border-subtle);
  }
  .segmented button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    height: 30px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-muted);
    font-weight: var(--weight-medium);
    transition:
      background var(--dur-fast),
      color var(--dur-fast);
  }
  .segmented button:hover:not(.checked) {
    color: var(--text);
  }
  .segmented button.checked {
    background: var(--bg-elevated);
    color: var(--text);
    box-shadow: var(--shadow-sm), 0 0 0 1px var(--border-subtle);
  }
  .section {
    margin-top: 22px;
    padding-top: 16px;
    border-top: 1px solid var(--border-subtle);
  }
  .section-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 10px;
  }
  .section-title {
    font-size: var(--text-base);
    font-weight: var(--weight-semibold);
  }
  .grid {
    display: grid;
    grid-template-columns: 92px 1fr;
    align-items: center;
    gap: 8px 12px;
  }
  .grid > label,
  .label-cell {
    color: var(--text-muted);
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
  }
  .control {
    min-width: 0;
  }
  .control.inline {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .sub {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .num {
    width: 72px;
  }
  .num.wide {
    width: 110px;
  }
  .select {
    width: auto;
    min-width: 76px;
    padding-right: 6px;
  }
  .unit {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .segmented.small {
    margin-top: 0;
    flex: 1 1 220px;
  }
  .segmented.small button {
    height: 26px;
    font-size: var(--text-sm);
  }
  .cursor-glyph {
    display: inline-block;
    width: 8px;
    height: 12px;
    border-radius: 1px;
  }
  .cursor-glyph.block {
    background: currentColor;
    opacity: 0.7;
  }
  .cursor-glyph.bar {
    width: 2px;
    background: currentColor;
  }
  .cursor-glyph.underline {
    height: 2px;
    margin-top: 10px;
    background: currentColor;
  }
  .checks {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 14px;
  }
  .checks .checkbox {
    color: var(--text);
  }
  .opt {
    align-items: flex-start;
    color: var(--text);
  }
  .opt input {
    flex: none;
    margin-top: 2px;
  }
  .opt > span {
    display: flex;
    flex-direction: column;
  }
  .opt .desc {
    font-size: var(--text-xs);
    line-height: 1.4;
  }
  .opt.nested {
    padding-left: 23px;
  }
  .opt.disabled,
  .radios.disabled {
    opacity: 0.55;
  }
  .section .checks:first-of-type {
    margin-top: 0;
  }
  .saving {
    color: var(--text-subtle);
    font-size: var(--text-sm);
  }
  .inline-field {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-top: 12px;
  }
  .inline-field > span {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .inline-field .desc {
    font-size: var(--text-xs);
  }
  .radios {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 14px;
  }
  .radios .label-cell {
    margin-bottom: 2px;
  }
  .preview {
    margin-top: 14px;
    padding: 10px 12px;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg);
    color: var(--text);
    overflow: hidden;
  }
  .preview > div {
    white-space: pre;
  }
  .p-user {
    color: light-dark(#116329, #5fd38d);
    font-weight: 700;
  }
  .p-path,
  .p-dir {
    color: light-dark(#0969da, #6aa5ff);
    font-weight: 700;
  }
  .p-exe {
    color: light-dark(#116329, #5fd38d);
    font-weight: 700;
  }
  .p-cursor {
    display: inline-block;
    width: 0.6em;
    background: var(--accent);
    animation: none;
  }
  .p-cursor.bar {
    width: 2px;
  }
  .p-cursor.underline {
    background: none;
    box-shadow: inset 0 -2px 0 var(--accent);
  }
  .about {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-top: 24px;
    padding-top: 16px;
    border-top: 1px solid var(--border-subtle);
  }
</style>
