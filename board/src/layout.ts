import { useSyncExternalStore } from "react";

/** `row` places children side by side, `column` stacks them. */
export type Dir = "row" | "column";
export type Side = "left" | "right" | "up" | "down";

/** A pane shows one view, a session key or a shared file (`fileView`), or nothing. */
export type PaneNode = { kind: "pane"; id: string; view: string | null };
export type SplitNode = { kind: "split"; id: string; dir: Dir; children: LayoutNode[]; sizes: number[] };
export type LayoutNode = PaneNode | SplitNode;

/** A tab holds its own pane tree, the focused pane and an optionally zoomed pane. */
export type Tab = { id: string; name: string; root: LayoutNode; focus: string; zoom: string | null };
export type Layout = { tabs: Tab[]; active: string };

type Rect = { x: number; y: number; w: number; h: number };

const KEY = "cq.tabs.v2";

let counter = 0;
const newId = (prefix: string): string => `${prefix}${Date.now().toString(36)}${(counter++).toString(36)}`;

const emptyPane = (): PaneNode => ({ kind: "pane", id: newId("p"), view: null });

/** A tab with one empty pane, named after its position (`tab N`) unless a name is given. */
const newTab = (tabs: Tab[], name = `tab ${tabs.length + 1}`): Tab => {
  const pane = emptyPane();
  return { id: newId("t"), name, root: pane, focus: pane.id, zoom: null };
};

// ---------- pure tree operations ----------

/** All panes of a tree, in reading order. */
export const panesOf = (node: LayoutNode): PaneNode[] =>
  node.kind === "pane" ? [node] : node.children.flatMap(panesOf);

const findPane = (node: LayoutNode, id: string): PaneNode | null => panesOf(node).find((p) => p.id === id) ?? null;

/** The split directly containing `id`, with the index of `id` in it. */
const parentOf = (node: LayoutNode, id: string): { split: SplitNode; index: number } | null => {
  if (node.kind === "pane") return null;
  const index = node.children.findIndex((c) => c.id === id);
  if (index >= 0) return { split: node, index };
  for (const c of node.children) {
    const found = parentOf(c, id);
    if (found) return found;
  }
  return null;
};

/** Returns the tree with node `id` replaced by `f(node)`. */
const replace = (node: LayoutNode, id: string, f: (n: LayoutNode) => LayoutNode): LayoutNode => {
  if (node.id === id) return f(node);
  if (node.kind === "pane") return node;
  return { ...node, children: node.children.map((c) => replace(c, id, f)) };
};

const dirOf = (side: Side): Dir => (side === "left" || side === "right" ? "row" : "column");

/** Splits pane `id` towards `side` with a new pane (empty unless `view` is given). */
export const splitPane = (root: LayoutNode, id: string, side: Side, view: string | null = null): { root: LayoutNode; pane: PaneNode } => {
  const pane: PaneNode = { ...emptyPane(), view };
  const dir = dirOf(side);
  const before = side === "left" || side === "up";
  const parent = parentOf(root, id);
  if (parent && parent.split.dir === dir) {
    const { split, index } = parent;
    const at = before ? index : index + 1;
    const half = split.sizes[index] / 2;
    const sizes = [...split.sizes];
    sizes.splice(index, 1, half);
    sizes.splice(at, 0, half);
    const children = [...split.children];
    children.splice(at, 0, pane);
    return { root: replace(root, split.id, () => ({ ...split, children, sizes })), pane };
  }
  const wrap = (n: LayoutNode): SplitNode => ({
    kind: "split",
    id: newId("s"),
    dir,
    children: before ? [pane, n] : [n, pane],
    sizes: [0.5, 0.5],
  });
  return { root: replace(root, id, wrap), pane };
};

/** Removes pane `id`; a split left with one child is replaced by that child. */
export const removePane = (root: LayoutNode, id: string): LayoutNode => {
  if (root.kind === "pane") return root.id === id ? emptyPane() : root;
  const parent = parentOf(root, id);
  if (!parent) return root;
  const { split, index } = parent;
  const children = split.children.filter((_, i) => i !== index);
  const freed = split.sizes[index];
  const sizes = split.sizes.filter((_, i) => i !== index).map((s) => s + freed / children.length);
  const next: LayoutNode = children.length === 1 ? children[0] : { ...split, children, sizes };
  return replace(root, split.id, () => next);
};

const setView = (root: LayoutNode, id: string, view: string | null): LayoutNode =>
  replace(root, id, (n) => ({ ...(n as PaneNode), view }));

/** Rectangles of all panes in a unit square. */
const rectsOf = (node: LayoutNode, r: Rect = { x: 0, y: 0, w: 1, h: 1 }): Map<string, Rect> => {
  if (node.kind === "pane") return new Map([[node.id, r]]);
  const out = new Map<string, Rect>();
  let offset = 0;
  node.children.forEach((c, i) => {
    const share = node.sizes[i];
    const sub = node.dir === "row" ? { x: r.x + offset * r.w, y: r.y, w: share * r.w, h: r.h } : { x: r.x, y: r.y + offset * r.h, w: r.w, h: share * r.h };
    rectsOf(c, sub).forEach((v, k) => out.set(k, v));
    offset += share;
  });
  return out;
};

/** The nearest pane next to `id` towards `side`, if any. */
const neighbor = (root: LayoutNode, id: string, side: Side): string | null => {
  const rects = rectsOf(root);
  const from = rects.get(id);
  if (!from) return null;
  const eps = 1e-6;
  let best: { id: string; dist: number } | null = null;
  for (const [pid, r] of rects) {
    if (pid === id) continue;
    const overlapsY = r.y < from.y + from.h - eps && r.y + r.h > from.y + eps;
    const overlapsX = r.x < from.x + from.w - eps && r.x + r.w > from.x + eps;
    const dist =
      side === "left" && overlapsY && r.x + r.w <= from.x + eps ? from.x - (r.x + r.w)
      : side === "right" && overlapsY && r.x >= from.x + from.w - eps ? r.x - (from.x + from.w)
      : side === "up" && overlapsX && r.y + r.h <= from.y + eps ? from.y - (r.y + r.h)
      : side === "down" && overlapsX && r.y >= from.y + from.h - eps ? r.y - (from.y + from.h)
      : null;
    if (dist !== null && (!best || dist < best.dist)) best = { id: pid, dist };
  }
  return best?.id ?? null;
};

// ---------- store ----------

const load = (): Layout => {
  try {
    const saved = JSON.parse(localStorage.getItem(KEY) ?? "null") as Layout | null;
    if (saved && saved.tabs.length > 0) return saved;
  } catch {
    // Fall back to a fresh layout.
  }
  const tab = newTab([]);
  return { tabs: [tab], active: tab.id };
};

let layout = load();
const listeners = new Set<() => void>();

const commit = (next: Layout) => {
  layout = next;
  try {
    localStorage.setItem(KEY, JSON.stringify(layout));
  } catch {
    // The layout is a per-viewer convenience.
  }
  listeners.forEach((l) => l());
};

/** React hook returning the tabs and the active tab id. */
export const useLayout = (): Layout =>
  useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => layout,
  );

/** The current layout, for code outside React (e.g. a drag in progress). */
export const currentLayout = (): Layout => layout;

/** The active tab. */
export const activeTab = (): Tab => layout.tabs.find((t) => t.id === layout.active) ?? layout.tabs[0];

const updateTab = (id: string, f: (t: Tab) => Tab) => commit({ ...layout, tabs: layout.tabs.map((t) => (t.id === id ? f(t) : t)) });

/** The view key shown in the focused pane of the active tab. */
export const focusedView = (): string | null => {
  const t = activeTab();
  return findPane(t.root, t.focus)?.view ?? null;
};

export const focusPane = (paneId: string) => updateTab(activeTab().id, (t) => ({ ...t, focus: paneId }));

/** Splits the focused pane of the active tab towards `side` and focuses the new empty pane. */
export const splitFocused = (side: Side) =>
  updateTab(activeTab().id, (t) => {
    const { root, pane } = splitPane(t.root, t.focus, side);
    return { ...t, root, focus: pane.id, zoom: null };
  });

/** Closes a pane of the active tab (a session in it is only detached). */
export const closePane = (paneId: string) => {
  const tab = activeTab();
  // Closing a tab's only pane when it is already empty closes the tab.
  if (tab.root.kind === "pane" && tab.root.id === paneId && tab.root.view === null) return closeTab(tab.id);
  updateTab(tab.id, (t) => {
    const root = removePane(t.root, paneId);
    const focus = findPane(root, t.focus) ? t.focus : panesOf(root)[0].id;
    return { ...t, root, focus, zoom: t.zoom === paneId ? null : t.zoom };
  });
};

export const moveFocus = (side: Side) => {
  const t = activeTab();
  const next = neighbor(t.root, t.focus, side);
  if (next) focusPane(next);
};

export const toggleZoom = () => updateTab(activeTab().id, (t) => ({ ...t, zoom: t.zoom ? null : t.focus }));

/** Changes the sizes of a split (fractions summing to 1). */
export const resizeSplit = (splitId: string, sizes: number[]) =>
  updateTab(activeTab().id, (t) => ({ ...t, root: replace(t.root, splitId, (n) => ({ ...(n as SplitNode), sizes })) }));

/**
 * Shows a view in a tab: focuses it if the tab already has it, opens it to the right of the pane
 * showing `near` if there is one, fills the focused pane if that is empty, and otherwise splits
 * the focused pane across its parent's direction.
 */
const showIn = (t: Tab, view: string, near?: string): Tab => {
  const existing = panesOf(t.root).find((p) => p.view === view);
  if (existing) return { ...t, focus: existing.id };
  const anchor = near === undefined ? undefined : panesOf(t.root).find((p) => p.view === near);
  if (anchor) {
    const { root, pane } = splitPane(t.root, anchor.id, "right", view);
    return { ...t, root, focus: pane.id, zoom: null };
  }
  const focused = findPane(t.root, t.focus);
  if (focused && focused.view === null) return { ...t, root: setView(t.root, focused.id, view) };
  const parent = parentOf(t.root, t.focus);
  const side: Side = parent?.split.dir === "row" ? "down" : "right";
  const { root, pane } = splitPane(t.root, t.focus, side, view);
  return { ...t, root, focus: pane.id, zoom: null };
};

/** Shows a view in the active tab, next to the pane showing `near` if any (see `showIn`). */
export const openInActiveTab = (view: string, near?: string) => updateTab(activeTab().id, (t) => showIn(t, view, near));

/** Drops panes whose view is gone (e.g. an ended session), in every tab. */
export const dropViews = (gone: (view: string) => boolean) => {
  let changed = false;
  const tabs = layout.tabs.map((t) => {
    let root = t.root;
    for (const p of panesOf(t.root)) {
      if (p.view !== null && gone(p.view)) {
        root = removePane(root, p.id);
        changed = true;
      }
    }
    if (root === t.root) return t;
    const focus = findPane(root, t.focus) ? t.focus : panesOf(root)[0].id;
    return { ...t, root, focus, zoom: t.zoom && findPane(root, t.zoom) ? t.zoom : null };
  });
  if (changed) commit({ ...layout, tabs });
};

export const addTab = (name?: string) => {
  const tab = newTab(layout.tabs, name);
  commit({ tabs: [...layout.tabs, tab], active: tab.id });
};

/** Closes a tab; its sessions are only detached. The last tab is replaced by an empty one. */
export const closeTab = (id: string) => {
  const index = layout.tabs.findIndex((t) => t.id === id);
  const tabs = layout.tabs.filter((t) => t.id !== id);
  if (tabs.length === 0) tabs.push(newTab(tabs));
  const active = layout.active === id ? tabs[Math.min(index, tabs.length - 1)].id : layout.active;
  commit({ tabs, active });
};

export const renameTab = (id: string, name: string) => updateTab(id, (t) => ({ ...t, name: name.trim() || t.name }));

export const selectTab = (id: string) => commit({ ...layout, active: id });

/** Selects the tab `step` places after the active one, wrapping around. */
export const cycleTab = (step: number) => {
  const i = layout.tabs.findIndex((t) => t.id === layout.active);
  selectTab(layout.tabs[(i + step + layout.tabs.length) % layout.tabs.length].id);
};

/** Moves tab `id` to position `index`. */
export const moveTab = (id: string, index: number) => {
  const tab = layout.tabs.find((t) => t.id === id);
  if (!tab) return;
  const tabs = layout.tabs.filter((t) => t.id !== id);
  tabs.splice(index, 0, tab);
  commit({ ...layout, tabs });
};

/** Where a dragged pane goes: beside a pane, or into a tab (a new tab for `null`) by the `showIn` rule. */
export type MoveTarget = { tab: string; pane: string; side: Side } | { tab: string | null };

/** Whether tab `tabId` already shows the view of pane `src` in another tab `srcTab`. */
export const holdsViewOf = (srcTab: string, src: string, tabId: string): boolean => {
  const from = layout.tabs.find((t) => t.id === srcTab);
  const view = from && findPane(from.root, src)?.view;
  const dst = layout.tabs.find((t) => t.id === tabId);
  return tabId !== srcTab && !!view && !!dst && panesOf(dst.root).some((p) => p.view === view);
};

/** Moves pane `src` of tab `srcTab` and shows the destination tab. A tab left without panes is closed. */
export const movePane = (srcTab: string, src: string, to: MoveTarget) => {
  const from = layout.tabs.find((t) => t.id === srcTab);
  const pane = from && findPane(from.root, src);
  if (!from || !pane) return;
  if ("pane" in to ? to.pane === src : to.tab === srcTab) return selectTab(srcTab);
  let tabs = layout.tabs;
  let dst = to.tab;
  if (dst === null) {
    const tab = newTab(tabs);
    tabs = [...tabs, tab];
    dst = tab.id;
  }
  const remove = (t: Tab): Tab => {
    const root = removePane(t.root, src);
    return { ...t, root, focus: findPane(root, t.focus) ? t.focus : panesOf(root)[0].id, zoom: t.zoom === src ? null : t.zoom };
  };
  const place = (t: Tab): Tab => {
    if (!("pane" in to)) return pane.view === null ? t : showIn(t, pane.view);
    const { root, pane: placed } = splitPane(t.root, to.pane, to.side, pane.view);
    return { ...t, root, focus: placed.id, zoom: null };
  };
  const emptied = panesOf(from.root).length === 1;
  tabs = tabs.flatMap((t) => {
    if (t.id === srcTab && emptied) return [];
    const removed = t.id === srcTab ? remove(t) : t;
    return [t.id === dst ? place(removed) : removed];
  });
  commit({ tabs, active: dst });
};
