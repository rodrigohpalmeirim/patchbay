<script lang="ts">
  import { onMount } from "svelte";
  import { newHost } from "./lib/actions";
  import ContextMenu from "./lib/components/ContextMenu.svelte";
  import Sidebar from "./lib/components/Sidebar.svelte";
  import DragGhost from "./lib/components/DragGhost.svelte";
  import Toasts from "./lib/components/Toasts.svelte";
  import TransfersPanel from "./lib/components/TransfersPanel.svelte";
  import Workspace from "./lib/components/Workspace.svelte";
  import EditConflictDialog from "./lib/dialogs/EditConflictDialog.svelte";
  import FileConflictDialog from "./lib/dialogs/FileConflictDialog.svelte";
  import PermissionsDialog from "./lib/dialogs/PermissionsDialog.svelte";
  import ConfirmDialog from "./lib/dialogs/ConfirmDialog.svelte";
  import ForwardEditor from "./lib/dialogs/ForwardEditor.svelte";
  import HostEditor from "./lib/dialogs/HostEditor.svelte";
  import PromptDialog from "./lib/dialogs/PromptDialog.svelte";
  import SettingsDialog from "./lib/dialogs/SettingsDialog.svelte";
  import SnippetsDialog from "./lib/dialogs/SnippetsDialog.svelte";
  import WarningsDialog from "./lib/dialogs/WarningsDialog.svelte";
  import { startApp } from "./lib/stores/app";
  import { startUpdateChecks } from "./lib/updater";
  import { session } from "./lib/stores/session.svelte";
  import { prompts } from "./lib/stores/prompts.svelte";
  import { files } from "./lib/stores/files.svelte";
  import { tabs } from "./lib/stores/tabs.svelte";
  import { transfers } from "./lib/stores/transfers.svelte";
  import { toasts } from "./lib/stores/toasts.svelte";
  import { ui } from "./lib/stores/ui.svelte";
  import { toCommandError } from "./lib/util/errors";
  import { hasMod, isEditable, isMac } from "./lib/util/platform";
  import { inTerminal, isNewTerminalKey } from "./lib/terminal/keys";
  import { newTerminalForActive } from "./lib/terminal/ops";
  import Welcome from "./lib/views/Welcome.svelte";

  onMount(() => {
    let stop: (() => void) | undefined;
    let disposed = false;
    startApp()
      .then((fn) => (disposed ? fn() : (stop = fn)))
      .catch((e) => toasts.error(toCommandError(e), "Patchbay couldn't start"));
    startUpdateChecks();
    return () => {
      disposed = true;
      stop?.();
    };
  });

  function focusPanel(id: string | null) {
    if (!id) return;
    requestAnimationFrame(() => {
      const panel = document.getElementById(`panel-${id}`);
      (panel?.querySelector<HTMLElement>("[data-autofocus-panel]") ?? panel)?.focus();
    });
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.defaultPrevented) return;
    // Modal dialogs own the keyboard.
    if (document.querySelector("dialog[open]")) return;
    const mod = hasMod(e);
    const key = e.key.toLowerCase();

    const active = tabs.active;
    // Inside a terminal, Ctrl combinations belong to the shell (Ctrl+W/N/F/\
    // are readline / signals); only the shortcuts in terminal/keys.ts
    // `passesToApp` get here. Cmd on macOS never reaches the shell.
    const shellOwnsCtrl = !isMac && inTerminal(e.target);

    if (isNewTerminalKey(e)) {
      e.preventDefault();
      newTerminalForActive();
    } else if (shellOwnsCtrl && mod && ["f", "n", "w", "\\"].includes(key)) {
      return;
    } else if (mod && key === "f" && active?.kind === "files") {
      // Search inside the focused files tab.
      e.preventDefault();
      files.get(active.id)?.openSearch();
    } else if ((mod && key === "f") || (e.key === "/" && !mod && !isEditable(e.target))) {
      e.preventDefault();
      ui.focusSearch();
    } else if (mod && !e.shiftKey && key === "n") {
      e.preventDefault();
      newHost();
    } else if (mod && e.key === ",") {
      e.preventDefault();
      ui.settingsOpen = true;
    } else if (mod && key === "w" && tabs.activeId) {
      e.preventDefault();
      tabs.closeActive();
    } else if (e.ctrlKey && e.key === "Tab") {
      e.preventDefault();
      tabs.cycle(e.shiftKey ? -1 : 1);
    } else if (e.ctrlKey && (e.key === "PageDown" || e.key === "PageUp")) {
      e.preventDefault();
      tabs.cycle(e.key === "PageDown" ? 1 : -1);
    } else if (e.key === "F6" && tabs.isSplit) {
      // Jump to the other side of the split.
      e.preventDefault();
      tabs.focusOther();
      focusPanel(tabs.activeId);
    } else if (mod && e.key === "\\" && tabs.activeId) {
      e.preventDefault();
      const id = tabs.activeId;
      tabs.moveToGroup(id, "other");
      focusPanel(id);
    } else if (mod && e.shiftKey && key === "j") {
      e.preventDefault();
      transfers.toggle();
    } else if (mod && e.shiftKey && key === "p") {
      // Port forwards of the focused tab's host, or all of them.
      e.preventDefault();
      const hostId = active?.kind === "forwards" ? null : tabs.activeHostId;
      const tab = tabs.openForwards(hostId);
      focusPanel(tab.id);
    } else if (mod && /^[1-9]$/.test(e.key)) {
      e.preventDefault();
      tabs.activateIndex(e.key === "9" ? -1 : Number(e.key) - 1);
    }
  }
</script>

<svelte:window {onkeydown} />

<div class="app">
  <Sidebar />
  <main class="main">
    {#if tabs.list.length > 0}
      <Workspace />
    {:else if !session.pending}
      <Welcome />
    {/if}
    <TransfersPanel />
  </main>
</div>

{#if prompts.current}
  {#key prompts.current.id}
    {#if prompts.current.prompt.kind === "fileConflict"}
      <FileConflictDialog payload={prompts.current} prompt={prompts.current.prompt} />
    {:else if prompts.current.prompt.kind === "editConflict"}
      <EditConflictDialog payload={prompts.current} prompt={prompts.current.prompt} />
    {:else}
      <PromptDialog payload={prompts.current} />
    {/if}
  {/key}
{/if}
{#if ui.editor}
  {#key ui.editor}
    <HostEditor editor={ui.editor} />
  {/key}
{/if}
{#if ui.settingsOpen}
  <SettingsDialog />
{/if}
{#if ui.warningsOpen}
  <WarningsDialog />
{/if}
{#if ui.confirmState}
  <ConfirmDialog state={ui.confirmState} />
{/if}
{#if ui.permissions}
  {#key ui.permissions}
    <PermissionsDialog state={ui.permissions} />
  {/key}
{/if}
{#if ui.snippets}
  {#key ui.snippets}
    <SnippetsDialog state={ui.snippets} />
  {/key}
{/if}
{#if ui.forwardEditor}
  {#key ui.forwardEditor}
    <ForwardEditor editor={ui.forwardEditor} />
  {/key}
{/if}
{#if ui.menu}
  <ContextMenu menu={ui.menu} />
{/if}
<DragGhost />
<Toasts />

<style>
  .app {
    display: flex;
    height: 100%;
    min-width: 0;
  }
  .main {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
    background: var(--bg);
  }
</style>
