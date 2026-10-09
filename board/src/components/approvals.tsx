import { useEffect, useRef, useState } from "react";
import { Minus } from "lucide-react";
import { approve } from "../api";
import { displayName } from "../store";
import type { MachineInfo, SessionState } from "../types";
import { DirName } from "./dir-name";

/** Identifies one prompt of one session, so a newer prompt shows up again after minimizing. */
export const promptKey = (s: SessionState): string => `${s.key}#${s.prompt?.id ?? ""}`;

/** Sessions waiting for an answer, oldest first. */
export const pendingApprovals = (sessions: SessionState[]): SessionState[] =>
  sessions.filter((s) => s.prompt && s.activity !== "exited");

const Card = ({ s, machine, focusToken, onMinimize }: { s: SessionState; machine: string; focusToken: number; onMinimize: () => void }) => {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const prompt = s.prompt;

  useEffect(() => {
    if (focusToken > 0) ref.current?.focus();
  }, [focusToken]);

  if (!prompt) return null;
  const answer = async (option: number) => {
    setBusy(true);
    setError(null);
    try {
      await approve(s.machine, s.meta.id, option, prompt.id);
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") onMinimize();
    else if (/^[1-9]$/.test(e.key) && prompt.options.some((o) => o.number === Number(e.key))) void answer(Number(e.key));
  };

  return (
    <div className="approval-card" tabIndex={-1} ref={ref} onKeyDown={onKey} role="dialog" aria-label={`Approval for ${displayName(s)}`}>
      <header>
        <div className="approval-title">{prompt.kind === "dialog" ? "Confirmation needed" : "Permission requested"}</div>
        <button className="pane-button" title="Minimize (stays in the Approvals list)" onClick={onMinimize}>
          <Minus size={14} />
        </button>
      </header>
      <dl className="approval-meta">
        <dt>session</dt>
        <dd>{displayName(s)}</dd>
        <dt>machine</dt>
        <dd>{machine}</dd>
        <dt>cwd</dt>
        <dd>
          <DirName path={s.meta.cwd} />
        </dd>
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
    </div>
  );
};

type Props = {
  sessions: SessionState[];
  machines: MachineInfo[];
  minimized: Set<string>;
  onMinimize: (key: string) => void;
  /** Card to focus (from `ui.approvals` or the Approvals list); a new token focuses it again. */
  focus: { key: string; token: number } | null;
};

/**
 * Approval cards in the bottom-right corner. They never take the keyboard focus by themselves,
 * so typing in a pane cannot answer a prompt by accident; answering locally removes the card.
 */
export const ApprovalCards = ({ sessions, machines, minimized, onMinimize, focus }: Props) => (
  <div className="approval-cards">
    {pendingApprovals(sessions)
      .filter((s) => !minimized.has(promptKey(s)))
      .map((s) => (
        <Card
          key={promptKey(s)}
          s={s}
          machine={machines.find((m) => m.id === s.machine)?.label ?? s.machine}
          focusToken={focus?.key === promptKey(s) ? focus.token : 0}
          onMinimize={() => onMinimize(promptKey(s))}
        />
      ))}
  </div>
);

/** The Approvals list of the right sidebar; selecting an item brings its card back. */
export const ApprovalList = ({ sessions, onShow }: { sessions: SessionState[]; onShow: (key: string) => void }) => {
  const pending = pendingApprovals(sessions);
  if (pending.length === 0) return <div className="empty">Nothing waiting</div>;
  return (
    <ul className="approval-list">
      {pending.map((s) => (
        <li key={promptKey(s)}>
          <button onClick={() => onShow(promptKey(s))}>
            <span className="session-name">{displayName(s)}</span>
            <span className="approval-kind">{s.prompt?.kind === "dialog" ? "confirm" : "permission"}</span>
          </button>
        </li>
      ))}
    </ul>
  );
};
