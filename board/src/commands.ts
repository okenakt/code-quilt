import { useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { asRecord, str } from "./json";
import { chordOfEvent, parseChord } from "./keys";

/** An argument the palette asks for before running a command. */
export type CommandArg = { name: string; title: string; default?: string | null };

/** A command shown in the palette and bindable to keys. */
export type Command = {
  name: string;
  title: string;
  /** Plugin that declares it; built-in UI commands have none. */
  plugin?: string | null;
  args: CommandArg[];
  /** Distinguishes press and release (push-to-talk). */
  hold: boolean;
  palette: boolean;
};

/** A key bound to a command, with fixed arguments. */
export type Binding = { chord: string; command: string; args: Record<string, string> };

/** Where a default binding comes from; later layers override earlier ones. */
export type BindingLayer = "builtin" | "plugin" | "user";

type PluginCommand = {
  name: string;
  title: string;
  hold?: boolean;
  palette?: boolean;
  plugin?: string | null;
  args?: CommandArg[];
  keybinding?: string | null;
};

/** Contents of `ui.toml` and `keybindings.toml` as sent by the backend. */
export type UiConfig = { ui: unknown; keybindings: unknown[]; errors: string[] };

type State = { plugins: Command[]; pluginKeys: Binding[]; config: UiConfig; loaded: boolean };

let state: State = { plugins: [], pluginKeys: [], config: { ui: null, keybindings: [], errors: [] }, loaded: false };
const listeners = new Set<() => void>();
const set = (next: Partial<State>) => {
  state = { ...state, ...next };
  listeners.forEach((l) => l());
};

const applyCommands = (list: PluginCommand[]) => {
  set({
    plugins: list
      .filter((c) => c.palette)
      .map((c) => ({ name: c.name, title: c.title, plugin: c.plugin, args: c.args ?? [], hold: c.hold ?? false, palette: true })),
    pluginKeys: list.flatMap((c) => {
      const chord = c.keybinding ? parseChord(c.keybinding) : null;
      return chord ? [{ chord, command: c.name, args: {} }] : [];
    }),
  });
};

/** Starts following plugin commands and the settings files. */
export const startCommandSync = async (): Promise<() => void> => {
  const offs = await Promise.all([
    listen<PluginCommand[]>("commands", (e) => applyCommands(e.payload)),
    listen<UiConfig>("ui-config", (e) => set({ config: e.payload })),
  ]);
  applyCommands(await invoke<PluginCommand[]>("commands"));
  set({ config: await invoke<UiConfig>("ui_config"), loaded: true });
  return () => offs.forEach((o) => o());
};

/** React hook with plugin commands, their default keys and the settings files. */
export const useCommandState = (): State =>
  useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => state,
  );

/** Runs a plugin command through the daemon; resolves to its message. */
export const invokePluginCommand = (name: string, args: Record<string, string>, phase = "invoke"): Promise<string> =>
  invoke("invoke_command", { name, args, phase });

/** Bindings from `keybindings.toml`, with problems reported. A command prefixed with `-`
 * removes that key's binding (or all of the command's default bindings without a key). */
const userBindings = (raw: unknown[], errors: string[]): { add: Binding[]; remove: { chord: string | null; command: string }[] } => {
  const add: Binding[] = [];
  const remove: { chord: string | null; command: string }[] = [];
  raw.forEach((entry, i) => {
    const r = asRecord(entry);
    const command = str(r, "command");
    const keyText = str(r, "key");
    const chord = keyText ? parseChord(keyText) : null;
    if (!command) return errors.push(`keybindings.toml binding ${i + 1}: missing command`);
    if (keyText && !chord) return errors.push(`keybindings.toml binding ${i + 1}: invalid key "${keyText}"`);
    if (command.startsWith("-")) return remove.push({ chord, command: command.slice(1) });
    if (!chord) return errors.push(`keybindings.toml binding ${i + 1}: missing key`);
    const args: Record<string, string> = {};
    for (const [k, v] of Object.entries(asRecord(r?.args) ?? {})) args[k] = String(v);
    add.push({ chord, command, args });
  });
  return { add, remove };
};

/** The effective key map and any conflicts or errors to show to the user. */
export type Keymap = { byChord: Map<string, Binding>; byCommand: Map<string, string[]>; warnings: string[] };

/** Merges built-in, plugin and user bindings (user > plugin > built-in). */
export const resolveKeymap = (builtin: Binding[], plugin: Binding[], userRaw: unknown[], known: Set<string>): Keymap => {
  const warnings: string[] = [];
  const user = userBindings(userRaw, warnings);
  const byChord = new Map<string, Binding>();
  const layer = new Map<string, BindingLayer>();
  const put = (b: Binding, from: BindingLayer) => {
    const prev = byChord.get(b.chord);
    if (prev && prev.command !== b.command && layer.get(b.chord) === from && from !== "builtin") {
      warnings.push(`${b.chord} is bound to both ${prev.command} and ${b.command}; using ${b.command}`);
    } else if (prev && prev.command !== b.command && from === "user") {
      warnings.push(`${b.chord} now runs ${b.command} instead of ${prev.command}`);
    }
    byChord.set(b.chord, b);
    layer.set(b.chord, from);
  };
  builtin.forEach((b) => put(b, "builtin"));
  plugin.forEach((b) => put(b, "plugin"));
  for (const r of user.remove) {
    for (const [chord, b] of [...byChord]) {
      if (b.command === r.command && (r.chord === null || r.chord === chord)) byChord.delete(chord);
    }
  }
  user.add.forEach((b) => {
    if (!known.has(b.command)) warnings.push(`keybindings.toml: unknown command ${b.command}`);
    put(b, "user");
  });
  const byCommand = new Map<string, string[]>();
  for (const [chord, b] of byChord) byCommand.set(b.command, [...(byCommand.get(b.command) ?? []), chord]);
  return { byChord, byCommand, warnings };
};

/** Fallback fonts appended after the user's choice so missing fonts never break the terminal. */
const FONT_FALLBACK = ['"Cascadia Mono"', '"BIZ UDGothic"', "Consolas", '"DejaVu Sans Mono"', '"Noto Sans Mono CJK JP"', "monospace"];

/** Terminal font settings from `ui.toml` (`[terminal] font_family`, `font_size`). */
export const terminalFont = (config: UiConfig): { family: string; size: number } => {
  const t = asRecord(asRecord(config.ui)?.terminal);
  const raw = t?.font_family;
  const chosen = (Array.isArray(raw) ? raw : typeof raw === "string" ? [raw] : [])
    .filter((f): f is string => typeof f === "string" && f.trim() !== "")
    .map((f) => `"${f.replace(/"/g, "")}"`);
  const size = typeof t?.font_size === "number" && t.font_size >= 6 && t.font_size <= 48 ? t.font_size : 13;
  return { family: [...chosen, ...FONT_FALLBACK].join(", "), size };
};

let activeChords = new Set<string>();

/** Records the chords in use so terminals can leave them to the application. */
export const setActiveChords = (chords: Iterable<string>): void => {
  activeChords = new Set(chords);
};

/** Whether a key event belongs to the application rather than the terminal. */
export const isAppChord = (e: KeyboardEvent): boolean => {
  if (e.isComposing) return false;
  const chord = chordOfEvent(e);
  return chord !== null && activeChords.has(chord);
};

/** `[[on_start]]` entries of `ui.toml`: commands to run when the board starts. */
export const onStartCommands = (config: UiConfig): { command: string; args: Record<string, string> }[] | string => {
  const raw = asRecord(config.ui)?.on_start;
  if (raw === undefined) return [];
  if (!Array.isArray(raw)) return "ui.toml: on_start must be a list of [[on_start]] tables";
  const out: { command: string; args: Record<string, string> }[] = [];
  for (const entry of raw) {
    const command = str(entry, "command");
    if (!command) return "ui.toml: every [[on_start]] needs a command";
    const args: Record<string, string> = {};
    for (const [k, v] of Object.entries(asRecord(asRecord(entry)?.args) ?? {})) args[k] = String(v);
    out.push({ command, args });
  }
  return out;
};

/** Names of sessions pinned by default (`pinned` in ui.toml). */
export const pinnedNames = (config: UiConfig): string[] => {
  const raw = asRecord(config.ui)?.pinned;
  return Array.isArray(raw) ? raw.filter((n): n is string => typeof n === "string") : [];
};
