// Transient UI state: which dialogs/menus are open.

import type { LucideIcon } from "@lucide/svelte";
import type { FileEntry, HostDraft, HostId, PortForwardDraft } from "../types";

export type EditorState =
  | { mode: "create"; initial?: Partial<HostDraft> }
  | { mode: "edit"; hostId: HostId }
  | { mode: "view"; hostId: HostId };

export type MenuItem =
  | {
      label: string;
      icon?: LucideIcon;
      onSelect: () => void;
      danger?: boolean;
      disabled?: boolean;
      hint?: string;
    }
  | { separator: true };

export interface MenuState {
  items: MenuItem[];
  x: number;
  y: number;
  /** Element to return focus to when the menu closes. */
  returnFocus: HTMLElement | null;
  label: string;
}

export interface ConfirmState {
  title: string;
  message: string;
  confirmLabel: string;
  danger: boolean;
  /** Optional list shown under the message (e.g. files to delete). */
  details?: string[];
  /** Optional checkbox (e.g. "Don't ask again"); its value is passed to resolve. */
  option?: string;
  resolve: (ok: boolean, optionChecked?: boolean) => void;
}

export interface SnippetsDialogState {
  /** Snippet to select initially; "new" starts a new one. */
  select?: string | "new";
  /** Host to preselect as scope for a new snippet. */
  hostId?: HostId | null;
}

export type ForwardEditorState =
  | { mode: "create"; initial?: Partial<PortForwardDraft> }
  | { mode: "edit"; id: string };

export interface PermissionsState {
  hostId: HostId | null;
  entries: FileEntry[];
  /** Called after changes were applied. */
  onApplied: () => void;
}

class Ui {
  editor = $state<EditorState | null>(null);
  settingsOpen = $state(false);
  warningsOpen = $state(false);
  menu = $state<MenuState | null>(null);
  confirmState = $state<ConfirmState | null>(null);
  permissions = $state<PermissionsState | null>(null);
  snippets = $state<SnippetsDialogState | null>(null);
  forwardEditor = $state<ForwardEditorState | null>(null);
  /** Registered by the sidebar so global shortcuts can focus the search box. */
  focusSearch: () => void = () => {};

  openMenu(label: string, items: MenuItem[], at: { x: number; y: number } | HTMLElement) {
    const returnFocus =
      at instanceof HTMLElement ? at : (document.activeElement as HTMLElement | null);
    let x: number, y: number;
    if (at instanceof HTMLElement) {
      const r = at.getBoundingClientRect();
      x = r.left;
      y = r.bottom + 4;
    } else {
      ({ x, y } = at);
    }
    this.menu = { label, items, x, y, returnFocus };
  }

  closeMenu() {
    this.menu = null;
  }

  confirm(opts: {
    title: string;
    message: string;
    confirmLabel?: string;
    danger?: boolean;
    details?: string[];
  }): Promise<boolean> {
    return this.confirmWithOption(opts).then((r) => r.ok);
  }

  /** Like `confirm`, with an optional checkbox whose state is returned. */
  confirmWithOption(opts: {
    title: string;
    message: string;
    confirmLabel?: string;
    danger?: boolean;
    details?: string[];
    option?: string;
  }): Promise<{ ok: boolean; checked: boolean }> {
    this.confirmState?.resolve(false);
    return new Promise((resolve) => {
      this.confirmState = {
        title: opts.title,
        message: opts.message,
        confirmLabel: opts.confirmLabel ?? "OK",
        danger: opts.danger ?? false,
        details: opts.details,
        option: opts.option,
        resolve: (ok, checked = false) => {
          this.confirmState = null;
          resolve({ ok, checked });
        },
      };
    });
  }
}

export const ui = new Ui();
