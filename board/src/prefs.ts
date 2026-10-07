import { useSyncExternalStore } from "react";

/** Per-viewer layout of the sidebars. */
export type Prefs = {
  leftWidth: number;
  rightWidth: number;
  leftOpen: boolean;
  rightOpen: boolean;
  folded: string[];
  tokenView: "tree" | "rank";
  tokenPeriod: TokenPeriod;
};

export type TokenPeriod = "24h" | "7d" | "all";

const KEY = "cq.prefs.v1";
const DEFAULTS: Prefs = { leftWidth: 280, rightWidth: 260, leftOpen: true, rightOpen: true, folded: [], tokenView: "tree", tokenPeriod: "7d" };

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

export const setTokenPrefs = (patch: Partial<Pick<Prefs, "tokenView" | "tokenPeriod">>) => set(patch);

export const toggleSidebar = (side: "left" | "right") =>
  set(side === "left" ? { leftOpen: !prefs.leftOpen } : { rightOpen: !prefs.rightOpen });

/** Folds or unfolds a section or an item in one (keys like `machines`, `tab:<id>`, `ended:<machine>`). */
export const toggleFold = (key: string) =>
  set({ folded: prefs.folded.includes(key) ? prefs.folded.filter((x) => x !== key) : [...prefs.folded, key] });
