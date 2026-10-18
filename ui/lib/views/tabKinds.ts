// Registry of tab kinds -> view component, icon and title.
// To add a kind: extend `Tab` in stores/tabs.svelte.ts and add an entry here;
// the mapped type below makes a missing entry a type error.

import type { LucideIcon } from "@lucide/svelte";
import Cable from "@lucide/svelte/icons/cable";
import FolderOpen from "@lucide/svelte/icons/folder-open";
import Laptop from "@lucide/svelte/icons/laptop";
import Server from "@lucide/svelte/icons/server";
import SquareTerminal from "@lucide/svelte/icons/square-terminal";
import type { Component } from "svelte";
import type { Tab, TabKind } from "../stores/tabs.svelte";
import type { Host } from "../types";
import { basename } from "../util/paths";
import FilesView from "./FilesView.svelte";
import ForwardsView from "./ForwardsView.svelte";
import HostOverview from "./HostOverview.svelte";
import TerminalView from "./TerminalView.svelte";
import { machineLabel, terminalTitle } from "../terminal/ops";

/** Tabs whose hostId may be null (local) get `Host | null`. */
type HostOf<T extends Tab> = null extends T["hostId"] ? Host | null : Host;

export interface TabViewProps<T extends Tab = Tab> {
  tab: T;
  host: HostOf<T>;
  active: boolean;
}

export interface TabKindDef<T extends Tab = Tab> {
  icon: (tab: T) => LucideIcon;
  title: (tab: T, host: Host | undefined) => string;
  /** Secondary, muted label (e.g. the machine a files tab is on). */
  subtitle?: (tab: T, host: Host | undefined) => string;
  /** Tooltip; defaults to the title. */
  tooltip?: (tab: T, host: Host | undefined) => string;
  component: Component<TabViewProps<T>>;
}

type Registry = { [K in TabKind]: TabKindDef<Extract<Tab, { kind: K }>> };

export const tabKinds: Registry = {
  host: {
    icon: () => Server,
    title: (_tab, host) => host?.name ?? "Unknown host",
    tooltip: (_tab, host) => (host ? `${host.name} — ${host.hostname}` : "Unknown host"),
    component: HostOverview,
  },
  files: {
    icon: (tab) => (tab.hostId ? FolderOpen : Laptop),
    // A local path of "" is the drive list on Windows.
    title: (tab) => (tab.path ? basename(tab.path) : tab.path === "" ? "This PC" : "Files"),
    subtitle: (tab, host) => (tab.hostId ? (host?.name ?? "Unknown host") : "Local"),
    tooltip: (tab, host) => `${tab.hostId ? (host?.name ?? "Unknown host") : "This computer"}: ${tab.path ?? "home"}`,
    component: FilesView,
  },
  terminal: {
    icon: () => SquareTerminal,
    title: (tab) => terminalTitle(tab),
    subtitle: (tab, host) => {
      const title = terminalTitle(tab);
      const machine = tab.hostId ? (host?.name ?? "Unknown host") : "Local";
      return title.includes(machine) || title === machineLabel(tab.hostId) ? "" : machine;
    },
    tooltip: (tab, host) => `Terminal — ${tab.hostId ? (host?.name ?? "Unknown host") : "This computer"}: ${terminalTitle(tab)}`,
    component: TerminalView,
  },
  forwards: {
    icon: () => Cable,
    title: (tab) => (tab.hostId ? "Forwards" : "Port forwards"),
    subtitle: (tab, host) => (tab.hostId ? (host?.name ?? "Unknown host") : ""),
    tooltip: (tab, host) => (tab.hostId ? `Port forwards — ${host?.name ?? "Unknown host"}` : "Port forwards — all hosts"),
    component: ForwardsView,
  },
};

/** Type-erased lookup for rendering a tab of any kind. */
export function tabDef(tab: Tab): TabKindDef<Tab> {
  return tabKinds[tab.kind] as unknown as TabKindDef<Tab>;
}
