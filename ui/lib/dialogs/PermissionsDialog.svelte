<!--
  chmod/chown for one or more entries. The rwx grid and the octal field stay
  in sync; with a multi-selection, bits that differ between entries show as
  indeterminate and are left untouched unless the user sets them.
-->
<script lang="ts">
  import ChevronRight from "@lucide/svelte/icons/chevron-right";
  import Shield from "@lucide/svelte/icons/shield";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import { untrack } from "svelte";
  import Dialog from "../components/Dialog.svelte";
  import { machineName } from "../files/ops";
  import { call } from "../stores/backend";
  import { connections } from "../stores/connections.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { ui, type PermissionsState } from "../stores/ui.svelte";
  import { octal, permString, plural } from "../util/format";

  let { state: ps }: { state: PermissionsState } = $props();

  const entries = $derived(ps.entries);
  const hasDir = $derived(entries.some((e) => e.kind === "dir"));
  const shellLess = $derived(ps.hostId ? connections.get(ps.hostId).capabilities?.exec === false : false);

  // Initial common bits + which bits differ across the selection. The dialog
  // is re-created per request, so these are computed once.
  const common = (vals: (string | null)[]) => (vals.every((v) => v === vals[0]) ? (vals[0] ?? "") : null);
  const initial = untrack(() => {
    const modes = ps.entries.filter((e) => e.permissions != null).map((e) => (e.permissions ?? 0) & 0o7777);
    let mixed = 0;
    for (const m of modes) mixed |= m ^ modes[0];
    return {
      mode: modes.length ? modes[0] & ~mixed : 0o644,
      mixed,
      owner: common(ps.entries.map((e) => e.owner)),
      group: common(ps.entries.map((e) => e.group)),
    };
  });
  const initialOwner = initial.owner;
  const initialGroup = initial.group;

  let mode = $state(initial.mode);
  let mixed = $state(initial.mixed);
  let owner = $state(initial.owner ?? "");
  let group = $state(initial.group ?? "");
  let recursive = $state(false);
  let advanced = $state(((initial.mode | initial.mixed) & 0o7000) !== 0);
  let octalText = $state(initial.mixed ? "" : octal(initial.mode));
  let busy = $state(false);

  const ROWS: [string, number][] = [
    ["Owner", 6],
    ["Group", 3],
    ["Others", 0],
  ];
  const COLS: [string, number][] = [
    ["Read", 4],
    ["Write", 2],
    ["Execute", 1],
  ];
  const SPECIAL: [string, number, string][] = [
    ["Set user ID", 0o4000, "Run executables as the file’s owner"],
    ["Set group ID", 0o2000, "Run as the group / inherit group in folders"],
    ["Sticky", 0o1000, "Only owners can delete entries in a folder"],
  ];

  function setBit(bit: number, on: boolean) {
    mode = on ? mode | bit : mode & ~bit;
    mixed &= ~bit;
    octalText = mixed ? "" : octal(mode);
  }

  function indeterminate(node: HTMLInputElement, value: boolean) {
    node.indeterminate = value;
    return { update: (v: boolean) => (node.indeterminate = v) };
  }

  const octalValid = $derived(/^[0-7]{3,4}$/.test(octalText.trim()));
  function onoctal() {
    if (!octalValid) return;
    mode = parseInt(octalText.trim(), 8) & 0o7777;
    mixed = 0;
    if (mode & 0o7000) advanced = true;
  }

  const modeChanged = $derived(mode !== initial.mode || mixed !== initial.mixed);
  const ownerChanged = $derived(owner.trim() !== (initialOwner ?? "") && owner.trim() !== "");
  const groupChanged = $derived(group.trim() !== (initialGroup ?? "") && group.trim() !== "");
  const namesNeedShell = $derived(
    shellLess && ((ownerChanged && !/^\d+$/.test(owner.trim())) || (groupChanged && !/^\d+$/.test(group.trim()))),
  );
  const canApply = $derived(!busy && (modeChanged || ownerChanged || groupChanged || recursive) && (octalValid || !!mixed));

  const title = $derived(entries.length === 1 ? entries[0].name : plural(entries.length, "item"));
  const preview = $derived(mixed ? null : permString(mode, entries.length === 1 ? entries[0].kind : "file"));

  async function apply(e: SubmitEvent) {
    e.preventDefault();
    if (!canApply) return;
    busy = true;
    try {
      if (modeChanged || recursive) {
        // Group entries by their resulting mode so untouched mixed bits survive.
        const byMode = new Map<number, string[]>();
        for (const en of entries) {
          const cur = (en.permissions ?? initial.mode) & 0o7777;
          const next = ((cur & mixed) | (mode & ~mixed)) & 0o7777;
          byMode.set(next, [...(byMode.get(next) ?? []), en.path]);
        }
        for (const [m, paths] of byMode) {
          await call((b) => b.fsChmod(ps.hostId, paths, m, recursive), { action: "Couldn't change permissions" });
        }
      }
      if (ownerChanged || groupChanged) {
        await call(
          (b) =>
            b.fsChown(
              ps.hostId,
              entries.map((en) => en.path),
              ownerChanged ? owner.trim() : null,
              groupChanged ? group.trim() : null,
              recursive,
            ),
          { action: "Couldn't change owner" },
        );
      }
      toasts.success(`Updated ${entries.length === 1 ? `“${entries[0].name}”` : plural(entries.length, "item")}`);
      ps.onApplied();
      ui.permissions = null;
    } catch {
      ps.onApplied();
    } finally {
      busy = false;
    }
  }
</script>

<Dialog onclose={() => (ui.permissions = null)} labelledby="perm-title" width={460}>
  <form class="form" onsubmit={apply}>
    <div class="dialog-header">
      <span class="dialog-icon"><Shield size={18} /></span>
      <div class="titles">
        <h2 class="dialog-title" id="perm-title">Permissions</h2>
        <p class="dialog-subtitle"><span class="name" title={title}>{title}</span> · {machineName(ps.hostId)}</p>
      </div>
    </div>
    <div class="dialog-body">
      <table class="grid">
        <thead>
          <tr>
            <th scope="col"><span class="sr-only">Who</span></th>
            {#each COLS as [label] (label)}<th scope="col">{label}</th>{/each}
          </tr>
        </thead>
        <tbody>
          {#each ROWS as [who, shift] (who)}
            <tr>
              <th scope="row">{who}</th>
              {#each COLS as [what, bit] (what)}
                {@const b = bit << shift}
                <td>
                  <input
                    type="checkbox"
                    aria-label="{who} {what.toLowerCase()}"
                    checked={(mode & b) !== 0}
                    use:indeterminate={(mixed & b) !== 0}
                    onchange={(e) => setBit(b, e.currentTarget.checked)}
                    data-autofocus={who === "Owner" && what === "Read" ? true : undefined}
                  />
                </td>
              {/each}
            </tr>
          {/each}
        </tbody>
      </table>

      <div class="octal-row">
        <label class="field-label" for="perm-octal">Numeric</label>
        <input
          id="perm-octal"
          class="input mono octal"
          value={octalText}
          oninput={(e) => ((octalText = e.currentTarget.value), onoctal())}
          placeholder={mixed ? "Mixed" : "644"}
          maxlength="4"
          inputmode="numeric"
          spellcheck="false"
          aria-invalid={octalText !== "" && !octalValid}
        />
        <code class="symbolic" aria-label="Resulting mode">{preview ?? "mixed"}</code>
      </div>

      <button type="button" class="disclosure" aria-expanded={advanced} onclick={() => (advanced = !advanced)}>
        <span class="chev" class:open={advanced}><ChevronRight size={12} strokeWidth={2.5} /></span>
        Special bits
      </button>
      {#if advanced}
        <div class="special">
          {#each SPECIAL as [label, bit, hint] (label)}
            <label class="checkbox" title={hint}>
              <input
                type="checkbox"
                checked={(mode & bit) !== 0}
                use:indeterminate={(mixed & bit) !== 0}
                onchange={(e) => setBit(bit, e.currentTarget.checked)}
              />
              {label}
            </label>
          {/each}
        </div>
      {/if}

      <div class="owners">
        <div>
          <label class="field-label" for="perm-owner">Owner</label>
          <input id="perm-owner" class="input" bind:value={owner} placeholder={initialOwner === null ? "Mixed" : "Unchanged"} spellcheck="false" autocomplete="off" />
        </div>
        <div>
          <label class="field-label" for="perm-group">Group</label>
          <input id="perm-group" class="input" bind:value={group} placeholder={initialGroup === null ? "Mixed" : "Unchanged"} spellcheck="false" autocomplete="off" />
        </div>
      </div>
      {#if shellLess}
        <p class="note" class:warn={namesNeedShell}>
          <TriangleAlert size={13} />
          This server only allows SFTP, so owner and group must be numeric IDs (e.g. 1000). Names need shell access.
        </p>
      {:else if ownerChanged || groupChanged}
        <p class="note">Changing the owner usually requires administrator rights on the server.</p>
      {/if}

      {#if hasDir}
        <label class="checkbox recursive">
          <input type="checkbox" bind:checked={recursive} />
          Apply to everything inside {entries.length === 1 ? "this folder" : "the selected folders"}
        </label>
      {/if}
    </div>
    <div class="dialog-footer">
      <button type="button" class="btn" onclick={() => (ui.permissions = null)}>Cancel</button>
      <button type="submit" class="btn btn-primary" disabled={!canApply || namesNeedShell}>{busy ? "Applying…" : "Apply"}</button>
    </div>
  </form>
</Dialog>

<style>
  .form {
    display: contents;
  }
  .titles {
    min-width: 0;
  }
  .dialog-subtitle {
    display: flex;
    gap: 4px;
    min-width: 0;
  }
  .name {
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text);
  }
  .grid {
    width: 100%;
    border-collapse: separate;
    border-spacing: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    overflow: hidden;
  }
  .grid th,
  .grid td {
    height: 34px;
    padding: 0 12px;
    text-align: center;
    font-size: var(--text-sm);
  }
  .grid thead th {
    height: 30px;
    background: var(--bg-subtle);
    color: var(--text-muted);
    font-weight: var(--weight-medium);
    border-bottom: 1px solid var(--border);
  }
  .grid tbody th {
    text-align: left;
    font-weight: var(--weight-medium);
  }
  .grid tbody tr + tr > * {
    border-top: 1px solid var(--border-subtle);
  }
  .grid input {
    width: 15px;
    height: 15px;
    margin: 0;
    accent-color: var(--accent);
  }
  .octal-row {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 14px;
  }
  .octal-row .field-label {
    margin: 0;
  }
  .octal {
    width: 72px;
    text-align: center;
  }
  .symbolic {
    padding: 4px 8px;
    border-radius: var(--radius-sm);
    background: var(--bg-inset);
    color: var(--text-muted);
    font-size: 12px;
    letter-spacing: 0.04em;
  }
  .field-label {
    display: block;
    margin: 12px 0 6px;
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
  }
  .disclosure {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    margin-top: 12px;
    padding: 2px 4px 2px 0;
    border: 0;
    background: transparent;
    color: var(--text-muted);
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
  }
  .disclosure:hover {
    color: var(--text);
  }
  .chev {
    display: flex;
    transition: transform var(--dur-fast) var(--ease-out);
  }
  .chev.open {
    transform: rotate(90deg);
  }
  .special {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 16px;
    margin-top: 8px;
    font-size: var(--text-sm);
  }
  .owners {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
    margin-top: 4px;
  }
  .note {
    display: flex;
    gap: 6px;
    margin-top: 8px;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .note :global(svg) {
    flex: none;
    margin-top: 2px;
  }
  .note.warn {
    color: var(--warning-text);
  }
  .recursive {
    margin-top: 14px;
    font-size: var(--text-sm);
  }
</style>
