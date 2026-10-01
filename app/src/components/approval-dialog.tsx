import { useEffect, useRef, useState } from "react";
import { approve } from "../api";
import { displayName } from "../store";
import type { MachineInfo, SessionState } from "../types";

type Props = {
  sessions: SessionState[];
  machines: MachineInfo[];
  focusToken: number;
};

const promptId = (s: SessionState) => `${s.key}\n${s.prompt?.text ?? ""}`;

/**
 * Permission prompts from all sessions. Each dialog names the session, machine and directory so an
 * answer can never be routed to the wrong session; answering locally closes it automatically.
 */
export const ApprovalDialog = ({ sessions, machines, focusToken }: Props) => {
  const [dismissed, setDismissed] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const pending = sessions.filter((s) => s.prompt && s.activity !== "exited");
  const visible = pending.filter((s) => !dismissed.has(promptId(s)));
  const current = visible[0];

  useEffect(() => {
    if (focusToken > 0) setDismissed(new Set());
  }, [focusToken]);

  useEffect(() => {
    setError(null);
    setBusy(false);
    if (current) ref.current?.focus();
  }, [current?.key, current?.prompt?.text]);

  if (!current?.prompt) return null;
  const prompt = current.prompt;
  const machineLabel = machines.find((m) => m.id === current.machine)?.label ?? current.machine;

  const answer = async (option: number) => {
    setBusy(true);
    setError(null);
    try {
      await approve(current.machine, current.meta.id, option);
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      setDismissed((d) => new Set(d).add(promptId(current)));
    } else if (/^[1-9]$/.test(e.key) && prompt.options.some((o) => o.number === Number(e.key))) {
      void answer(Number(e.key));
    }
  };

  return (
    <div className="approval-backdrop">
      <div className="approval-dialog" tabIndex={-1} ref={ref} onKeyDown={onKey} role="dialog" aria-modal="true">
        <header>
          <div className="approval-title">Permission requested</div>
          <div className="approval-count">{visible.length > 1 ? `+${visible.length - 1} more` : ""}</div>
        </header>
        <dl className="approval-meta">
          <dt>session</dt>
          <dd>{displayName(current)}</dd>
          <dt>machine</dt>
          <dd>{machineLabel}</dd>
          <dt>cwd</dt>
          <dd>{current.meta.cwd}</dd>
        </dl>
        <pre className="approval-text">{prompt.text}</pre>
        <div className="approval-options">
          {prompt.options.map((o) => (
            <button key={o.number} disabled={busy} onClick={() => void answer(o.number)}>
              <kbd>{o.number}</kbd> {o.label}
            </button>
          ))}
        </div>
        {error && <div className="approval-error">{error}</div>}
        <footer>Esc to hide · answering in the terminal closes this dialog</footer>
      </div>
    </div>
  );
};
