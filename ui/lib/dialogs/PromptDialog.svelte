<!--
  Answers one backend prompt (host key, password, passphrase, 2FA). Enter
  submits the default action; Esc sends {kind:"cancel"}.
-->
<script lang="ts">
  import { writeClipboard } from "../util/clipboard";
  import Copy from "@lucide/svelte/icons/copy";
  import Eye from "@lucide/svelte/icons/eye";
  import EyeOff from "@lucide/svelte/icons/eye-off";
  import Fingerprint from "@lucide/svelte/icons/fingerprint";
  import KeyRound from "@lucide/svelte/icons/key-round";
  import LockKeyhole from "@lucide/svelte/icons/lock-keyhole";
  import Server from "@lucide/svelte/icons/server";
  import ShieldAlert from "@lucide/svelte/icons/shield-alert";
  import ShieldCheck from "@lucide/svelte/icons/shield-check";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import { tick } from "svelte";
  import Dialog from "../components/Dialog.svelte";
  import { hosts } from "../stores/hosts.svelte";
  import { prompts } from "../stores/prompts.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import type { PromptPayload, PromptResponse } from "../types";
  import { address, tildify } from "../util/format";

  let { payload }: { payload: PromptPayload } = $props();

  const p = $derived(payload.prompt);
  const host = $derived(hosts.get(payload.hostId));
  const position = $derived(prompts.queue.findIndex((q) => q.id === payload.id) + 1);
  const total = $derived(prompts.queue.length);
  const uid = $derived(`prompt-${payload.id}`);

  /** The prompt concerns a jump host rather than the target itself. */
  const viaHop = $derived.by(() => {
    if (!host) return null;
    const target = "host" in p ? p.host : null;
    return target && target !== host.hostname && target !== host.name ? target : null;
  });

  const retryId = $derived("retry" in p && p.retry ? `${uid}-retry` : undefined);

  let secret = $state("");
  let remember = $state(false);
  let reveal = $state(false);
  let answers = $state<string[]>([]);
  let revealed = $state<boolean[]>([]);
  let confirmStep = $state(false);
  let sent = false;

  $effect.pre(() => {
    if (p.kind === "keyboardInteractive" && answers.length !== p.prompts.length) {
      answers = p.prompts.map(() => "");
      revealed = p.prompts.map(() => false);
    }
  });

  function send(response: PromptResponse) {
    if (sent) return;
    sent = true;
    void prompts.respond(payload.id, response);
  }

  const cancel = () => send({ kind: "cancel" });

  let formEl = $state<HTMLFormElement>();
  async function toggleConfirm(on: boolean) {
    confirmStep = on;
    await tick();
    // Keep focus inside the dialog on a safe control.
    formEl?.querySelector<HTMLElement>(on ? "[data-back]" : ".btn-danger-outline")?.focus();
  }

  function onsubmit(e: SubmitEvent) {
    e.preventDefault();
    switch (p.kind) {
      case "hostKeyUnknown":
        send({ kind: "accept" });
        break;
      case "hostKeyChanged":
        // Default action is the safe one.
        if (confirmStep) send({ kind: "accept" });
        else send({ kind: "reject" });
        break;
      case "password":
      case "keyPassphrase":
        if (secret) send({ kind: "secret", value: secret, remember });
        break;
      case "keyboardInteractive":
        send({ kind: "answers", answers: [...answers] });
        break;
    }
  }

  async function copy(text: string) {
    try {
      await writeClipboard(text);
      toasts.success("Copied to clipboard");
    } catch {
      toasts.show("error", "Couldn't access the clipboard");
    }
  }

  const label = (text: string) => text.trim().replace(/:$/, "");
  const looksLikeCode = (text: string) => /code|otp|token|verification|2fa|pin/i.test(text);
</script>

{#snippet context()}
  <div class="context">
    <Server size={12} />
    <span>{host?.name ?? "Unknown host"}</span>
    {#if host}<span class="sep">·</span><span class="mono">{address(host)}</span>{/if}
    {#if viaHop}<span class="hop">via jump host <span class="mono">{viaHop}</span></span>{/if}
  </div>
{/snippet}

{#snippet queueInfo()}
  {#if total > 1}<span class="queue">{position} of {total}</span>{/if}
{/snippet}

{#snippet secretField(id: string, labelText: string)}
  <label class="field-label" for={id}>{labelText}</label>
  <div class="secret">
    <input
      {id}
      class="input"
      type={reveal ? "text" : "password"}
      bind:value={secret}
      autocomplete="off"
      spellcheck="false"
      data-autofocus
      aria-describedby={retryId}
    />
    <button
      type="button"
      class="icon-btn sm reveal"
      aria-label={reveal ? "Hide" : "Show"}
      aria-pressed={reveal}
      title={reveal ? "Hide" : "Show"}
      onclick={() => (reveal = !reveal)}
    >
      {#if reveal}<EyeOff size={14} />{:else}<Eye size={14} />{/if}
    </button>
  </div>
  <label class="checkbox remember">
    <input type="checkbox" bind:checked={remember} />
    Remember in system keychain
  </label>
{/snippet}

<Dialog
  onclose={cancel}
  labelledby="{uid}-title"
  describedby="{uid}-desc"
  width={p.kind === "hostKeyChanged" ? 520 : 440}
  tone={p.kind === "hostKeyChanged" ? "danger" : "default"}
  role={p.kind === "hostKeyChanged" ? "alertdialog" : "dialog"}
  dismissable={false}
>
  <form {onsubmit} class="form" bind:this={formEl}>
    {#if p.kind === "hostKeyUnknown"}
      <div class="dialog-header">
        <span class="dialog-icon"><Fingerprint size={18} /></span>
        <div>
          <h2 class="dialog-title" id="{uid}-title">Verify the server’s identity</h2>
          {@render context()}
        </div>
      </div>
      <div class="dialog-body">
        <p id="{uid}-desc" class="text">
          This is the first time you’re connecting to <strong class="mono">{p.host}{p.port !== 22 ? `:${p.port}` : ""}</strong>.
          Check that the fingerprint matches the one you expect before trusting it.
        </p>
        <div class="fp">
          <div class="fp-head">
            <span class="fp-type">{p.keyType}</span>
            <button type="button" class="icon-btn sm" aria-label="Copy fingerprint" title="Copy fingerprint" onclick={() => copy(p.fingerprint)}>
              <Copy size={13} />
            </button>
          </div>
          <code class="fp-value selectable">{p.fingerprint}</code>
        </div>
        <p class="hint">Accepting adds the key to <code>~/.ssh/known_hosts</code>.</p>
      </div>
      <div class="dialog-footer">
        {@render queueInfo()}
        <span class="spacer"></span>
        <button type="button" class="btn" onclick={cancel}>Cancel</button>
        <button type="submit" class="btn btn-primary" data-autofocus>Accept &amp; remember</button>
      </div>
    {:else if p.kind === "hostKeyChanged"}
      <div class="dialog-header">
        <span class="dialog-icon danger"><ShieldAlert size={18} /></span>
        <div>
          <h2 class="dialog-title danger-title" id="{uid}-title">The server’s host key has changed</h2>
          {@render context()}
        </div>
      </div>
      <div class="dialog-body">
        {#if !confirmStep}
          <p id="{uid}-desc" class="text">
            The key presented by <strong class="mono">{p.host}{p.port !== 22 ? `:${p.port}` : ""}</strong> does not match the one
            you trusted before. <strong>Someone may be intercepting this connection</strong> (a man-in-the-middle attack). It’s
            also possible the server was reinstalled or its keys were rotated.
          </p>
          <div class="fp danger">
            <div class="fp-head">
              <span class="fp-type">New {p.keyType} key</span>
              <button type="button" class="icon-btn sm" aria-label="Copy fingerprint" title="Copy fingerprint" onclick={() => copy(p.fingerprint)}>
                <Copy size={13} />
              </button>
            </div>
            <code class="fp-value selectable">{p.fingerprint}</code>
          </div>
          <div class="known">
            <span>Previously trusted key</span>
            <code class="selectable">{tildify(p.knownHostsFile)}:{p.line}</code>
          </div>
          <p class="hint">
            If you expected this change, remove line {p.line} from that file and connect again to trust the new key.
          </p>
        {:else}
          <div id="{uid}-desc" class="confirm">
            <TriangleAlert size={16} />
            <div>
              <strong>Connect once without trusting the new key?</strong>
              <p>
                Only continue if you’ve verified the new fingerprint with the server’s administrator. Anything you type,
                including passwords, could be visible to an attacker. Your known_hosts file won’t be changed.
              </p>
            </div>
          </div>
        {/if}
      </div>
      <div class="dialog-footer">
        {#if !confirmStep}
          <button type="button" class="btn btn-danger-outline" onclick={() => toggleConfirm(true)}>Connect once anyway…</button>
          <span class="spacer"></span>
          {@render queueInfo()}
          <button type="submit" class="btn btn-primary" data-autofocus>Cancel connection</button>
        {:else}
          <button type="button" class="btn btn-ghost" data-back onclick={() => toggleConfirm(false)}>Back</button>
          <span class="spacer"></span>
          <button type="button" class="btn" onclick={() => send({ kind: "reject" })}>Cancel connection</button>
          <button type="submit" class="btn btn-danger">Connect once</button>
        {/if}
      </div>
    {:else if p.kind === "password"}
      <div class="dialog-header">
        <span class="dialog-icon"><KeyRound size={18} /></span>
        <div>
          <h2 class="dialog-title" id="{uid}-title">Password for {p.user}@{p.host}</h2>
          {@render context()}
        </div>
      </div>
      <div class="dialog-body">
        <p id="{uid}-desc" class="sr-only">Enter the password to log in.</p>
        {#if p.retry}
          <div class="retry" id="{uid}-retry" role="alert">That password didn’t work. Try again.</div>
        {/if}
        {@render secretField(`${uid}-secret`, "Password")}
      </div>
      <div class="dialog-footer">
        {@render queueInfo()}
        <span class="spacer"></span>
        <button type="button" class="btn" onclick={cancel}>Cancel</button>
        <button type="submit" class="btn btn-primary" disabled={!secret}>Log in</button>
      </div>
    {:else if p.kind === "keyPassphrase"}
      <div class="dialog-header">
        <span class="dialog-icon"><LockKeyhole size={18} /></span>
        <div>
          <h2 class="dialog-title" id="{uid}-title">Unlock private key</h2>
          {@render context()}
        </div>
      </div>
      <div class="dialog-body">
        <p id="{uid}-desc" class="text">
          Enter the passphrase for <code class="selectable path">{tildify(p.keyPath)}</code>
        </p>
        {#if p.retry}
          <div class="retry" id="{uid}-retry" role="alert">Wrong passphrase. Try again.</div>
        {/if}
        {@render secretField(`${uid}-secret`, "Passphrase")}
      </div>
      <div class="dialog-footer">
        {@render queueInfo()}
        <span class="spacer"></span>
        <button type="button" class="btn" onclick={cancel}>Cancel</button>
        <button type="submit" class="btn btn-primary" disabled={!secret}>Unlock</button>
      </div>
    {:else if p.kind === "keyboardInteractive"}
      <div class="dialog-header">
        <span class="dialog-icon"><ShieldCheck size={18} /></span>
        <div>
          <h2 class="dialog-title" id="{uid}-title">{p.name.trim() || "Verification required"}</h2>
          {@render context()}
        </div>
      </div>
      <div class="dialog-body">
        <p id="{uid}-desc" class="text">
          {p.instructions.trim() || `The server is asking ${p.user} for more information.`}
        </p>
        {#each p.prompts as kp, i (i)}
          <div class="kbd-field">
            <label class="field-label" for="{uid}-a{i}">{label(kp.text)}</label>
            <div class="secret">
              <input
                id="{uid}-a{i}"
                class="input"
                class:mono={kp.echo && looksLikeCode(kp.text)}
                type={kp.echo || revealed[i] ? "text" : "password"}
                bind:value={answers[i]}
                autocomplete={kp.echo && looksLikeCode(kp.text) ? "one-time-code" : "off"}
                inputmode={kp.echo && looksLikeCode(kp.text) ? "numeric" : undefined}
                spellcheck="false"
                data-autofocus={i === 0 ? true : undefined}
              />
              {#if !kp.echo}
                <button
                  type="button"
                  class="icon-btn sm reveal"
                  aria-label={revealed[i] ? "Hide" : "Show"}
                  aria-pressed={revealed[i]}
                  onclick={() => (revealed[i] = !revealed[i])}
                >
                  {#if revealed[i]}<EyeOff size={14} />{:else}<Eye size={14} />{/if}
                </button>
              {/if}
            </div>
          </div>
        {/each}
      </div>
      <div class="dialog-footer">
        {@render queueInfo()}
        <span class="spacer"></span>
        <button type="button" class="btn" onclick={cancel}>Cancel</button>
        <button type="submit" class="btn btn-primary" data-autofocus={p.prompts.length === 0 ? true : undefined}>Continue</button>
      </div>
    {/if}
  </form>
</Dialog>

<style>
  .form {
    display: contents;
  }
  .context {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 5px;
    margin-top: 3px;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .context .sep {
    color: var(--text-subtle);
  }
  .context .mono {
    font-size: 11.5px;
  }
  .hop {
    padding: 0 6px;
    border-radius: var(--radius-full);
    background: var(--bg-active);
    font-size: var(--text-xs);
  }
  .danger-title {
    color: var(--danger-text);
  }
  .text {
    color: var(--text);
    overflow-wrap: anywhere;
  }
  .text strong.mono {
    font-weight: var(--weight-semibold);
  }
  .hint {
    margin-top: 10px;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .fp {
    margin-top: 14px;
    padding: 10px 12px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--bg-inset);
  }
  .fp.danger {
    border-color: var(--danger-border);
    background: var(--danger-soft);
  }
  .fp-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 4px;
  }
  .fp-type {
    color: var(--text-muted);
    font-size: var(--text-xs);
    font-weight: var(--weight-semibold);
    letter-spacing: 0.03em;
    text-transform: uppercase;
  }
  .fp-value {
    display: block;
    font-size: 13px;
    line-height: 1.5;
    letter-spacing: 0.01em;
    word-break: break-all;
    color: var(--text);
  }
  .known {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    justify-content: space-between;
    gap: 4px 12px;
    margin-top: 10px;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .known code {
    color: var(--text);
    word-break: break-all;
  }
  .confirm {
    display: flex;
    gap: 10px;
    padding: 12px;
    border-radius: var(--radius-md);
    background: var(--danger-soft);
    color: var(--danger-text);
  }
  .confirm > :global(svg) {
    flex: none;
    margin-top: 2px;
  }
  .confirm p {
    margin-top: 4px;
    color: var(--text);
  }
  .retry {
    margin-bottom: 12px;
    padding: 8px 10px;
    border-radius: var(--radius-sm);
    background: var(--danger-soft);
    color: var(--danger-text);
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
  }
  .path {
    word-break: break-all;
  }
  .field-label {
    display: block;
    margin: 12px 0 6px;
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
  }
  .retry + .field-label {
    margin-top: 0;
  }
  .secret {
    position: relative;
  }
  .secret .input {
    padding-right: 34px;
  }
  .reveal {
    position: absolute;
    top: 4px;
    right: 4px;
  }
  .remember {
    margin-top: 12px;
    font-size: var(--text-sm);
  }
  .kbd-field:first-of-type .field-label {
    margin-top: 14px;
  }
  .queue {
    color: var(--text-subtle);
    font-size: var(--text-sm);
  }
</style>
