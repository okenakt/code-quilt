import { useSyncExternalStore } from "react";
import type { SessionState } from "./types";

/** Session key to an explicit pin choice; sessions without one follow `pinned` in ui.toml. */
export type Pins = Record<string, boolean>;

const KEY = "cq.pins.v2";
const listeners = new Set<() => void>();

const load = (): Pins => {
  try {
    return JSON.parse(localStorage.getItem(KEY) ?? "{}") as Pins;
  } catch {
    return {};
  }
};

let pins = load();

/** React hook returning the explicit pin choices. */
export const usePins = (): Pins =>
  useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => pins,
  );

/** Whether a session is pinned: the user's choice, or else whether ui.toml pins its name. */
export const isPinned = (s: SessionState, choices: Pins, names: string[]): boolean =>
  choices[s.key] ?? (s.meta.name !== null && names.includes(s.meta.name));

/** Pins or unpins a session. */
export const togglePin = (s: SessionState, names: string[]): void => {
  pins = { ...pins, [s.key]: !isPinned(s, pins, names) };
  try {
    localStorage.setItem(KEY, JSON.stringify(pins));
  } catch {
    // Pins are a per-viewer convenience.
  }
  listeners.forEach((l) => l());
};
