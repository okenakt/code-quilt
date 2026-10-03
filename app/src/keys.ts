/** Key chords such as `ctrl+shift+p`, normalized so strings from files and events compare equal. */

const MODIFIERS = ["ctrl", "shift", "alt", "meta"] as const;

const MODIFIER_ALIASES: Record<string, (typeof MODIFIERS)[number]> = {
  ctrl: "ctrl",
  control: "ctrl",
  shift: "shift",
  alt: "alt",
  option: "alt",
  meta: "meta",
  cmd: "meta",
  win: "meta",
  super: "meta",
};

const KEY_ALIASES: Record<string, string> = {
  esc: "escape",
  return: "enter",
  pgup: "pageup",
  pgdn: "pagedown",
  del: "delete",
  ins: "insert",
  arrowleft: "left",
  arrowright: "right",
  arrowup: "up",
  arrowdown: "down",
  backslash: "\\",
  minus: "-",
  plus: "=",
};

/** `KeyboardEvent.code` to key names; layout independent, so bindings work on JIS keyboards too. */
const CODE_KEYS: Record<string, string> = {
  ArrowLeft: "left",
  ArrowRight: "right",
  ArrowUp: "up",
  ArrowDown: "down",
  PageUp: "pageup",
  PageDown: "pagedown",
  Home: "home",
  End: "end",
  Insert: "insert",
  Delete: "delete",
  Backspace: "backspace",
  Enter: "enter",
  NumpadEnter: "enter",
  Escape: "escape",
  Space: "space",
  Tab: "tab",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  IntlRo: "\\",
  IntlYen: "\\",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Backquote: "`",
};

const keyOfCode = (code: string): string | null => {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3).toLowerCase();
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code.toLowerCase();
  return CODE_KEYS[code] ?? null;
};

/** Normalizes a chord written by a user or plugin; returns `null` when it is not valid. */
export const parseChord = (text: string): string | null => {
  // A trailing "+" names the plus key itself (e.g. "ctrl++"), which lives on Equal.
  const parts = text.trim().toLowerCase().replace(/\+\+$/, "+=").split("+").filter((p) => p !== "");
  const mods = new Set<string>();
  let key: string | null = null;
  for (const p of parts) {
    const mod = MODIFIER_ALIASES[p];
    if (mod) {
      mods.add(mod);
      continue;
    }
    if (key !== null) return null;
    key = KEY_ALIASES[p] ?? p;
  }
  if (key === null) return null;
  return [...MODIFIERS.filter((m) => mods.has(m)), key].join("+");
};

/** The chord of a key event, or `null` for bare modifier presses and unknown keys. */
export const chordOfEvent = (e: KeyboardEvent): string | null => {
  const key = keyOfCode(e.code);
  if (key === null) return null;
  const mods = [e.ctrlKey && "ctrl", e.shiftKey && "shift", e.altKey && "alt", e.metaKey && "meta"].filter(Boolean);
  return [...mods, key].join("+");
};

const DISPLAY: Record<string, string> = {
  ctrl: "Ctrl",
  shift: "Shift",
  alt: "Alt",
  meta: "Meta",
  left: "←",
  right: "→",
  up: "↑",
  down: "↓",
  pageup: "PageUp",
  pagedown: "PageDown",
  escape: "Esc",
};

/** Human-readable form, e.g. `Ctrl+Shift+P`. */
export const formatChord = (chord: string): string =>
  chord
    .split("+")
    .map((p) => DISPLAY[p] ?? (p.length === 1 ? p.toUpperCase() : p[0].toUpperCase() + p.slice(1)))
    .join("+");
