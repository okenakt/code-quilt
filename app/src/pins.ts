import { useSyncExternalStore } from "react";
import type { SessionState } from "./types";

/** Sessions with this tag are pinned automatically (e.g. orchestrators). */
export const AUTO_PIN_TAG = "queen";

const KEY = "cq.pins.v1";
const listeners = new Set<() => void>();

const load = (): Set<string> => {
  try {
    return new Set(JSON.parse(localStorage.getItem(KEY) ?? "[]") as string[]);
  } catch {
    return new Set();
  }
};

let pins = load();

const save = () => {
  try {
    localStorage.setItem(KEY, JSON.stringify([...pins]));
  } catch {
    // Pins are a per-viewer convenience.
  }
};

/** React hook returning the set of manually pinned session keys. */
export const usePins = (): Set<string> =>
  useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => pins,
  );

/** Pins or unpins a session. */
export const togglePin = (key: string): void => {
  pins = new Set(pins);
  if (pins.has(key)) pins.delete(key);
  else pins.add(key);
  save();
  listeners.forEach((l) => l());
};

/** Whether a session is pinned, manually or by its tag. */
export const isPinned = (s: SessionState, manual: Set<string>): boolean =>
  manual.has(s.key) || s.meta.tags.includes(AUTO_PIN_TAG);
