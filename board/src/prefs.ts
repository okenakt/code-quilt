import { useSyncExternalStore } from "react";

/** Sections of the sidebars. The left ones can be hidden; all of them can be folded. */
export type Section = "pinned" | "tabs" | "machines" | "approvals" | "usage";
export type LeftSection = "pinned" | "tabs" | "machines";

/** Per-viewer layout of the sidebars. */
export type Prefs = {
  leftWidth: number;
  rightWidth: number;
  rightOpen: boolean;
  hidden: LeftSection[];
  folded: string[];
};

const KEY = "cq.prefs.v1";
const DEFAULTS: Prefs = { leftWidth: 280, rightWidth: 260, rightOpen: true, hidden: [], folded: [] };

const load = (): Prefs => {
  try {
    return { ...DEFAULTS, ...(JSON.parse(localStorage.getItem(KEY) ?? "{}") as Partial<Prefs>) };
  } catch {
    return DEFAULTS;
  }
};

let prefs = load();
const listeners = new Set<() => void>();

const set = (patch: Partial<Prefs>) => {
  prefs = { ...prefs, ...patch };
  try {
    localStorage.setItem(KEY, JSON.stringify(prefs));
  } catch {
    // Sidebar layout is a per-viewer convenience.
  }
  listeners.forEach((l) => l());
};

/** React hook returning the sidebar layout. */
export const usePrefs = (): Prefs =>
  useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => prefs,
  );

export const setWidth = (side: "left" | "right", width: number) =>
  set(side === "left" ? { leftWidth: Math.round(width) } : { rightWidth: Math.round(width) });

export const toggleRight = () => set({ rightOpen: !prefs.rightOpen });

export const toggleSection = (s: LeftSection) =>
  set({ hidden: prefs.hidden.includes(s) ? prefs.hidden.filter((x) => x !== s) : [...prefs.hidden, s] });

/** Folds or unfolds a section or an item in one (keys like `machines`, `tab:<id>`, `ended:<machine>`). */
export const toggleFold = (key: string) =>
  set({ folded: prefs.folded.includes(key) ? prefs.folded.filter((x) => x !== key) : [...prefs.folded, key] });
