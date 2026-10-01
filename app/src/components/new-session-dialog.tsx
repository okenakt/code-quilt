import { useEffect, useState } from "react";
import { listDir, type DirList } from "../api";
import type { MachineInfo } from "../types";

type Props = {
  machines: MachineInfo[];
  initialMachine: string;
  onLaunch: (machine: string, cwd: string, args: string[]) => Promise<void>;
  onClose: () => void;
};

/** Splits an argument string on whitespace, honoring double quotes. */
const splitArgs = (s: string): string[] => [...s.matchAll(/"([^"]*)"|(\S+)/g)].map((m) => m[1] ?? m[2]);

const join = (dir: string, name: string) => (/[\\/]$/.test(dir) ? dir + name : `${dir}${dir.includes("\\") ? "\\" : "/"}${name}`);

/** Starts a new session in a directory under the machine's allowed roots. */
export const NewSessionDialog = ({ machines, initialMachine, onLaunch, onClose }: Props) => {
  const online = machines.filter((m) => m.online);
  const [machine, setMachine] = useState(initialMachine);
  const [list, setList] = useState<DirList | null>(null);
  const [args, setArgs] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const browse = (path: string) => {
    setError(null);
    listDir(machine, path).then(setList, (e: unknown) => setError(String(e)));
  };

  useEffect(() => browse(""), [machine]);

  const start = async () => {
    if (!list?.path) return;
    setBusy(true);
    setError(null);
    try {
      await onLaunch(machine, list.path, splitArgs(args));
      onClose();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal new-session"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => e.key === "Escape" && onClose()}
        role="dialog"
        aria-modal="true"
      >
        <header className="modal-title">New session</header>
        <label className="field">
          <span>machine</span>
          <select value={machine} onChange={(e) => setMachine(e.target.value)}>
            {online.map((m) => (
              <option key={m.id} value={m.id}>
                {m.label}
              </option>
            ))}
          </select>
        </label>
        <div className="field">
          <span>directory</span>
          <div className="dir-current">{list?.path || "choose a root"}</div>
        </div>
        <ul className="dir-list">
          {list?.path && (
            <li>
              <button onClick={() => browse(list.parent ?? "")}>..</button>
            </li>
          )}
          {list?.entries.map((name) => (
            <li key={name}>
              <button onClick={() => browse(list.path ? join(list.path, name) : name)}>{name}</button>
            </li>
          ))}
        </ul>
        <label className="field">
          <span>arguments</span>
          <input
            value={args}
            placeholder="e.g. --model sonnet"
            onChange={(e) => setArgs(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && void start()}
          />
        </label>
        {error && <div className="modal-error">{error}</div>}
        <footer className="modal-actions">
          <button onClick={onClose}>Cancel</button>
          <button className="primary" disabled={!list?.path || busy} onClick={() => void start()}>
            Start claude here
          </button>
        </footer>
      </div>
    </div>
  );
};
