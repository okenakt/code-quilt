/** Application-level shortcuts. They are handled before the terminal so xterm never sees them. */
export type ShortcutAction =
  | { type: "next-panel" }
  | { type: "prev-panel" }
  | { type: "close-panel" }
  | { type: "split-right" }
  | { type: "split-down" }
  | { type: "open-nth"; index: number }
  | { type: "focus-approval" };

/** Maps a key event to an application shortcut, or `null` when the terminal should get it. */
export const matchShortcut = (e: KeyboardEvent): ShortcutAction | null => {
  if (e.type !== "keydown" || e.isComposing) return null;
  const ctrlShift = e.ctrlKey && e.shiftKey && !e.altKey && !e.metaKey;
  if (ctrlShift) {
    switch (e.code) {
      case "PageDown":
      case "ArrowRight":
        return { type: "next-panel" };
      case "PageUp":
      case "ArrowLeft":
        return { type: "prev-panel" };
      case "KeyW":
        return { type: "close-panel" };
      case "Backslash":
      case "IntlRo":
        return { type: "split-right" };
      case "Minus":
        return { type: "split-down" };
      case "KeyA":
        return { type: "focus-approval" };
    }
  }
  if (e.altKey && !e.ctrlKey && !e.shiftKey && !e.metaKey && /^Digit[1-9]$/.test(e.code)) {
    return { type: "open-nth", index: Number(e.code.slice(5)) - 1 };
  }
  return null;
};

/** Human-readable list for the help footer. */
export const SHORTCUT_HELP: [string, string][] = [
  ["Alt+1…9", "open session"],
  ["Ctrl+Shift+←/→", "prev/next tile"],
  ["Ctrl+Shift+\\ / -", "split right / down"],
  ["Ctrl+Shift+W", "close tile"],
  ["Ctrl+Shift+A", "approvals"],
];
