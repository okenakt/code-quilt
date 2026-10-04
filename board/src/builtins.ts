import type { Binding, Command } from "./commands";

const builtin = (name: string, title: string, args: Command["args"] = []): Command => ({
  name,
  title,
  plugin: null,
  args,
  hold: false,
  palette: true,
});

/** Commands the dashboard itself provides. */
export const BUILTIN_COMMANDS: Command[] = [
  builtin("ui.palette", "Show Command Palette"),
  builtin("ui.nextTile", "Focus Next Tile"),
  builtin("ui.prevTile", "Focus Previous Tile"),
  builtin("ui.closeTile", "Close Tile (Detach)"),
  builtin("ui.splitRight", "Split Right"),
  builtin("ui.splitDown", "Split Down"),
  builtin("ui.approvals", "Show Pending Approvals"),
  builtin("ui.openSession", "Open Session by Number", [{ name: "index", title: "Session number (1-9)", default: "1" }]),
  builtin("ui.toggleUsage", "Toggle Usage Sidebar"),
  builtin("ui.newSession", "New Session", [{ name: "machine", title: "Machine id", default: "local" }]),
  builtin("session.end", "End Focused Session"),
  builtin("session.pin", "Pin or Unpin Focused Session"),
  builtin("session.copyId", "Copy Focused Session Id"),
  builtin("daemon.restart", "Restart Daemon (sessions keep running)", [{ name: "machine", title: "Machine id", default: "local" }]),
];

const key = (chord: string, command: string, args: Record<string, string> = {}): Binding => ({ chord, command, args });

/** Default bindings of the built-in commands; all use Ctrl+Shift or Alt+digit so they rarely
 * take keys an agent needs. */
export const BUILTIN_KEYS: Binding[] = [
  key("ctrl+shift+p", "ui.palette"),
  key("ctrl+shift+right", "ui.nextTile"),
  key("ctrl+shift+pagedown", "ui.nextTile"),
  key("ctrl+shift+left", "ui.prevTile"),
  key("ctrl+shift+pageup", "ui.prevTile"),
  key("ctrl+shift+w", "ui.closeTile"),
  key("ctrl+shift+\\", "ui.splitRight"),
  key("ctrl+shift+-", "ui.splitDown"),
  key("ctrl+shift+a", "ui.approvals"),
  ...Array.from({ length: 9 }, (_, i) => key(`alt+${i + 1}`, "ui.openSession", { index: String(i + 1) })),
];
