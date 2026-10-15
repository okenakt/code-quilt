import { ArrowLeft } from "lucide-react";
import { useEffect, useState } from "react";
import { listDir, type Agent, type DirList } from "../api";
import type { MachineInfo } from "../types";
import { DirName } from "./dir-name";

type Props = {
  machines: MachineInfo[];
  initialMachine: string;
  onLaunch: (machine: string, agent: Agent, cwd: string, args: string[]) => Promise<void>;
  onClose: () => void;
};

/** Splits an argument string on whitespace, honoring double quotes. */
const splitArgs = (s: string): string[] => [...s.matchAll(/"([^"]*)"|(\S+)/g)].map((m) => m[1] ?? m[2]);

const join = (dir: string, name: string) => (/[\\/]$/.test(dir) ? dir + name : `${dir}${dir.includes("\\") ? "\\" : "/"}${name}`);

/** Starts a new session in a folder the user trusted in an agent, or in another allowed folder. */
export const NewSessionDialog = ({ machines, initialMachine, onLaunch, onClose }: Props) => {
  const online = machines.filter((m) => m.online);
  const [machine, setMachine] = useState(initialMachine);
  const [agent, setAgent] = useState<Agent>("claude");
  const [known, setKnown] = useState<string[]>([]);
  const [cwd, setCwd] = useState<string | null>(null);
  /** The folder browser, open while choosing another folder. */
  const [browser, setBrowser] = useState<DirList | null>(null);
  const [args, setArgs] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const browse = (path: string) => {
    setError(null);
    listDir(machine, path).then((l) => {
      setBrowser(l);
      if (l.path) setCwd(l.path);
    }, (e: unknown) => setError(String(e)));
  };

  // The trusted folders and roots are read anew each time, so changes show up right away.
  useEffect(() => {
    setCwd(null);
    setBrowser(null);
    listDir(machine, "").then((l) => {
      setKnown(l.known);
      if (l.known.length === 0) setBrowser(l);
    }, (e: unknown) => setError(String(e)));
  }, [machine]);

  const start = async () => {
    if (!cwd) return;
    setBusy(true);
    setError(null);
    try {
      await onLaunch(machine, agent, cwd, splitArgs(args));
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
                {m.name}
              </option>
            ))}
          </select>
        </label>
        <label className="field">
          <span>agent</span>
          <select value={agent} onChange={(e) => setAgent(e.target.value === "codex" ? "codex" : "claude")}>
            <option value="claude">Claude Code</option>
            <option value="codex">Codex</option>
          </select>
        </label>
        <div className="field">
          <span>folder</span>
          <div className="dir-current">{cwd ? <DirName path={cwd} /> : "choose a folder"}</div>
        </div>
        {!browser && (
          <ul className="dir-list">
            {known.map((dir) => (
              <li key={dir}>
                <button className={dir === cwd ? "selected" : ""} onClick={() => setCwd(dir)}>
                  <DirName path={dir} />
                </button>
              </li>
            ))}
            <li>
              <button className="dir-other" onClick={() => browse("")}>
                Choose another folder…
              </button>
            </li>
          </ul>
        )}
        {browser && (
          <ul className="dir-list">
            {known.length > 0 && (
              <li>
                <button className="dir-other" onClick={() => setBrowser(null)}>
                  <ArrowLeft size={12} /> Trusted folders
                </button>
              </li>
            )}
            {browser.path && (
              <li>
                <button onClick={() => browse(browser.parent ?? "")}>..</button>
              </li>
            )}
            {browser.entries.map((name) => (
              <li key={name}>
                <button onClick={() => browse(browser.path ? join(browser.path, name) : name)}>
                  <DirName path={name} />
                </button>
              </li>
            ))}
          </ul>
        )}
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
          <button className="primary" disabled={!cwd || busy} onClick={() => void start()}>
            Start {agent} here
          </button>
        </footer>
      </div>
    </div>
  );
};
