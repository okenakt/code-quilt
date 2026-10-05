import { useSyncExternalStore } from "react";
import type { SessionState } from "./types";

/** Sessions with this tag are pinned unless the user unpins them (e.g. orchestrators). */
export const AUTO_PIN_TAG = "queen";

/** Session key to an explicit pin choice; sessions without one follow their tag. */
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

/** Whether a session is pinned: the user's choice, or else its tag. */
export const isPinned = (s: SessionState, choices: Pins): boolean => choices[s.key] ?? s.meta.tags.includes(AUTO_PIN_TAG);

/** Pins or unpins a session. */
export const togglePin = (s: SessionState): void => {
  pins = { ...pins, [s.key]: !isPinned(s, pins) };
  try {
    localStorage.setItem(KEY, JSON.stringify(pins));
  } catch {
    // Pins are a per-viewer convenience.
  }
  listeners.forEach((l) => l());
};
