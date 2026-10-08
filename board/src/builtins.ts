import type { Binding, Command } from "./commands";

const builtin = (name: string, title: string, args: Command["args"] = []): Command => ({
  name,
  title,
  plugin: null,
  args,
  palette: true,
});

/** Commands the dashboard itself provides. */
export const BUILTIN_COMMANDS: Command[] = [
  builtin("ui.palette", "Show Command Palette"),
  builtin("ui.keybindings", "Show Keyboard Shortcuts"),
  builtin("ui.approvals", "Show Pending Approvals"),
  builtin("ui.toggleLeftSidebar", "Toggle Left Sidebar"),
  builtin("ui.toggleRightSidebar", "Toggle Right Sidebar"),
  builtin("ui.newSession", "New Session", [{ name: "machine", title: "Machine id", default: "local" }]),
  builtin("tab.new", "New Tab"),
  builtin("tab.close", "Close Tab (Sessions Keep Running)"),
  builtin("tab.rename", "Rename Tab", [{ name: "name", title: "Tab name" }]),
  builtin("tab.next", "Next Tab"),
  builtin("tab.prev", "Previous Tab"),
  builtin("tab.goTo", "Go to Tab by Number", [{ name: "index", title: "Tab number (1-9)", default: "1" }]),
  builtin("pane.splitLeft", "Split Pane Left"),
  builtin("pane.splitRight", "Split Pane Right"),
  builtin("pane.splitUp", "Split Pane Up"),
  builtin("pane.splitDown", "Split Pane Down"),
  builtin("pane.focusLeft", "Focus Pane Left"),
  builtin("pane.focusRight", "Focus Pane Right"),
  builtin("pane.focusUp", "Focus Pane Up"),
  builtin("pane.focusDown", "Focus Pane Down"),
  builtin("pane.close", "Close Pane (Detach)"),
  builtin("pane.zoom", "Zoom Pane"),
  builtin("session.end", "End Focused Session"),
  builtin("session.pin", "Pin or Unpin Focused Session"),
  builtin("session.copyId", "Copy Focused Session Id"),
  builtin("daemon.restart", "Restart Daemon"),
];

const key = (chord: string, command: string, args: Record<string, string> = {}): Binding => ({ chord, command, args });

/** Default bindings of the built-in commands; all use Ctrl+Shift or Alt+digit so they rarely
 * take keys an agent needs. */
export const BUILTIN_KEYS: Binding[] = [
  key("ctrl+shift+p", "ui.palette"),
  key("ctrl+shift+a", "ui.approvals"),
  key("ctrl+shift+t", "tab.new"),
  key("ctrl+shift+pagedown", "tab.next"),
  key("ctrl+shift+pageup", "tab.prev"),
  ...Array.from({ length: 9 }, (_, i) => key(`alt+${i + 1}`, "tab.goTo", { index: String(i + 1) })),
  key("ctrl+shift+\\", "pane.splitRight"),
  key("ctrl+shift+-", "pane.splitDown"),
  key("ctrl+shift+left", "pane.focusLeft"),
  key("ctrl+shift+right", "pane.focusRight"),
  key("ctrl+shift+up", "pane.focusUp"),
  key("ctrl+shift+down", "pane.focusDown"),
  key("ctrl+shift+w", "pane.close"),
  key("ctrl+shift+z", "pane.zoom"),
];
