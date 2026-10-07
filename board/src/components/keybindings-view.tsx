import { useEffect, useMemo, useState } from "react";
import { X } from "lucide-react";
import { bindKey, unbindKey } from "../api";
import { setRecording, targetKey, type Command, type Keymap } from "../commands";
import { chordOfEvent, formatChord } from "../keys";
import { askConfirm } from "./confirm-dialog";

type Props = {
  commands: Command[];
  keymap: Keymap;
  warnings: string[];
  onError: (message: string) => void;
  onClose: () => void;
};

/** What one row binds: a command, with fixed arguments for rows like "Go to Tab (index=1)". */
type Row = { key: string; command: string; args: Record<string, string>; title: string; plugin?: string | null };

const SOURCE_LABEL = { builtin: "default", plugin: "plugin", user: "user" } as const;

/** One row per command, plus one per argument set that has a key (e.g. Alt+1..9 for `tab.goTo`). */
const rowsOf = (commands: Command[], keymap: Keymap): Row[] => {
  const byName = new Map(commands.map((c) => [c.name, c]));
  const rows = new Map<string, Row>();
  for (const c of commands) rows.set(targetKey(c.name, {}), { key: targetKey(c.name, {}), command: c.name, args: {}, title: c.title, plugin: c.plugin });
  for (const b of keymap.byChord.values()) {
    const c = byName.get(b.command);
    const key = targetKey(b.command, b.args);
    if (!c || rows.has(key)) continue;
    const argText = Object.entries(b.args)
      .map(([k, v]) => `${k}=${v}`)
      .join(", ");
    rows.set(key, { key, command: b.command, args: b.args, title: `${c.title} (${argText})`, plugin: c.plugin });
  }
  return [...rows.values()].sort((a, b) => a.title.localeCompare(b.title, undefined, { numeric: true }));
};

/** Lists every command with its key; binding a key replaces the row's key and takes it from any other command. */
export const KeybindingsView = ({ commands, keymap, warnings, onError, onClose }: Props) => {
  const [query, setQuery] = useState("");
  const [recordingFor, setRecordingFor] = useState<Row | null>(null);
  const all = useMemo(() => rowsOf(commands, keymap), [commands, keymap]);
  const keysOf = (r: Row) => keymap.byTarget.get(r.key) ?? [];

  const rows = useMemo(() => {
    const q = query.trim().toLowerCase();
    return all.filter((r) => !q || r.title.toLowerCase().includes(q) || r.command.toLowerCase().includes(q) || (keymap.byTarget.get(r.key) ?? []).some((k) => k.includes(q)));
  }, [all, keymap, query]);

  // While recording, the next key chord (with Esc to cancel) becomes the binding.
  useEffect(() => {
    if (!recordingFor) return;
    /** Binds `chord` to the row, after confirming if the key already runs something else. */
    const bind = async (row: Row, chord: string) => {
      const owner = keymap.byChord.get(chord);
      const ownerKey = owner ? targetKey(owner.command, owner.args) : null;
      if (ownerKey === row.key) return;
      if (owner) {
        const ownerTitle = all.find((r) => r.key === ownerKey)?.title ?? owner.command;
        if (!(await askConfirm("Reassign key", `${formatChord(chord)} runs "${ownerTitle}". Reassign it to "${row.title}"?`, "Reassign"))) return;
      }
      const old = (keymap.byTarget.get(row.key) ?? []).map((key) => ({ key, command: row.command }));
      await bindKey(chord, row.command, row.args, owner ? [...old, { key: chord, command: owner.command }] : old);
    };
    setRecording(true);
    const onKey = (e: KeyboardEvent) => {
      if (e.isComposing) return;
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") return setRecordingFor(null);
      const chord = chordOfEvent(e);
      if (!chord || /^(ctrl|shift|alt|meta)(\+(ctrl|shift|alt|meta))*$/.test(chord)) return;
      setRecordingFor(null);
      bind(recordingFor, chord).catch((err: unknown) => onError(String(err)));
    };
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      setRecording(false);
    };
  }, [recordingFor, keymap, all, onError]);

  return (
    <div className="keybindings-view" onKeyDown={(e) => e.key === "Escape" && !recordingFor && onClose()}>
      <header className="keybindings-header">
        <span className="keybindings-title">Keyboard shortcuts</span>
        <input autoFocus placeholder="Search commands or keys" value={query} onChange={(e) => setQuery(e.target.value)} spellCheck={false} />
        <button className="pane-button" title="Close" onClick={onClose}>
          <X size={14} />
        </button>
      </header>
      {warnings.length > 0 && (
        <div className="palette-warnings">
          {warnings.map((w) => (
            <div key={w}>{w}</div>
          ))}
        </div>
      )}
      <div className="keybindings-table">
        {rows.map((r) => {
          const recording = recordingFor?.key === r.key;
          return (
            <div key={r.key} className="keybindings-row">
              <div className="keybindings-command">
                <span>{r.title}</span>
                <span className="keybindings-name">
                  {r.command}
                  {r.plugin ? ` · ${r.plugin}` : ""}
                </span>
              </div>
              <div className="keybindings-keys">
                {keysOf(r).map((chord) => (
                  <span key={chord} className={`key-chip ${keymap.sources.get(chord) ?? "builtin"}`} title={SOURCE_LABEL[keymap.sources.get(chord) ?? "builtin"]}>
                    <kbd>{formatChord(chord)}</kbd>
                    <button title="Remove this key" onClick={() => unbindKey(chord, r.command).catch((err: unknown) => onError(String(err)))}>
                      <X size={11} />
                    </button>
                  </span>
                ))}
                <button className={`key-add ${recording ? "recording" : ""}`} onClick={() => setRecordingFor(recording ? null : r)}>
                  {recording ? "Press keys… (Esc to cancel)" : "bind"}
                </button>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
};
