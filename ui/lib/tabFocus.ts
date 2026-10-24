// Moving keyboard focus into a tab's panel (keyboard shortcuts that switch tabs or groups).

import { tabs } from "./stores/tabs.svelte";
import { terminals } from "./stores/terminals.svelte";

/** Focuses a tab's panel: a terminal's focused pane, else the panel's autofocus target. */
export function focusTabPanel(id: string | null) {
  if (!id) return;
  if (tabs.get(id)?.kind === "terminal") {
    terminals.requestFocus(id);
    return;
  }
  requestAnimationFrame(() => {
    const panel = document.getElementById(`panel-${id}`);
    (panel?.querySelector<HTMLElement>("[data-autofocus-panel]") ?? panel)?.focus();
  });
}
