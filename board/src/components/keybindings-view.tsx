import { useEffect, useMemo, useState } from "react";
import { Plus, X } from "lucide-react";
import { bindKey, unbindKey } from "../api";
import { setRecording, type Command, type Keymap } from "../commands";
import { chordOfEvent, formatChord } from "../keys";

type Props = {
  commands: Command[];
  keymap: Keymap;
  warnings: string[];
  onError: (message: string) => void;
  onClose: () => void;
};

const SOURCE_LABEL = { builtin: "default", plugin: "plugin", user: "user" } as const;

/** Lists every command with its keys; keys can be added by pressing them, or removed. */
export const KeybindingsView = ({ commands, keymap, warnings, onError, onClose }: Props) => {
  const [query, setQuery] = useState("");
  const [recordingFor, setRecordingFor] = useState<string | null>(null);

  const rows = useMemo(() => {
    const q = query.trim().toLowerCase();
    return [...commands]
      .sort((a, b) => a.title.localeCompare(b.title))
      .filter((c) => !q || c.title.toLowerCase().includes(q) || c.name.toLowerCase().includes(q) || (keymap.byCommand.get(c.name) ?? []).some((k) => k.includes(q)));
  }, [commands, keymap, query]);

  // While recording, the next key chord (with Esc to cancel) becomes the binding.
  useEffect(() => {
    if (!recordingFor) return;
    setRecording(true);
    const onKey = (e: KeyboardEvent) => {
      if (e.isComposing) return;
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") return setRecordingFor(null);
      const chord = chordOfEvent(e);
      if (!chord || /^(ctrl|shift|alt|meta)(\+(ctrl|shift|alt|meta))*$/.test(chord)) return;
      setRecordingFor(null);
      bindKey(chord, recordingFor).catch((err: unknown) => onError(String(err)));
    };
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      setRecording(false);
    };
  }, [recordingFor, onError]);

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
        {rows.map((c) => (
          <div key={c.name} className="keybindings-row">
            <div className="keybindings-command">
              <span>{c.title}</span>
              <span className="keybindings-name">
                {c.name}
                {c.plugin ? ` · ${c.plugin}` : ""}
              </span>
            </div>
            <div className="keybindings-keys">
              {(keymap.byCommand.get(c.name) ?? []).map((chord) => (
                <span key={chord} className={`key-chip ${keymap.sources.get(chord) ?? "builtin"}`} title={SOURCE_LABEL[keymap.sources.get(chord) ?? "builtin"]}>
                  <kbd>{formatChord(chord)}</kbd>
                  <button title="Remove this key" onClick={() => unbindKey(chord, c.name).catch((err: unknown) => onError(String(err)))}>
                    <X size={11} />
                  </button>
                </span>
              ))}
              <button className={`key-add ${recordingFor === c.name ? "recording" : ""}`} onClick={() => setRecordingFor(recordingFor === c.name ? null : c.name)}>
                {recordingFor === c.name ? "Press keys… (Esc to cancel)" : (
                  <>
                    <Plus size={11} /> key
                  </>
                )}
              </button>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
};
