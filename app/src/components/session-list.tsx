import { displayName } from "../store";
import type { Activity, MachineInfo, SessionState } from "../types";

const ACTIVITY_LABEL: Record<Activity, string> = {
  starting: "starting",
  working: "working",
  idle: "idle",
  approval: "needs approval",
  exited: "exited",
};

const formatTokens = (n: number | null): string | null => {
  if (n === null) return null;
  return n >= 1000 ? `${(n / 1000).toFixed(n >= 100_000 ? 0 : 1)}k` : String(n);
};

/** Badge with model, context usage and token totals from the status line. */
const UsageBadge = ({ s }: { s: SessionState }) => {
  const st = s.status;
  if (!st) return null;
  const parts = [
    st.model,
    st.contextPercent !== null ? `ctx ${Math.round(st.contextPercent)}%` : null,
    formatTokens((st.inputTokens ?? 0) + (st.outputTokens ?? 0) || null),
  ].filter((p): p is string => p !== null);
  return <div className="session-usage">{parts.join(" · ")}</div>;
};

type Props = {
  machines: MachineInfo[];
  sessions: SessionState[];
  activeKey: string | null;
  onOpen: (s: SessionState) => void;
};

/** Task list: sessions grouped by machine, live ones first. */
export const SessionList = ({ machines, sessions, activeKey, onOpen }: Props) => (
  <nav className="session-list">
    {machines.map((m) => {
      const mine = sessions.filter((s) => s.machine === m.id);
      const live = mine.filter((s) => s.activity !== "exited");
      const exited = mine.filter((s) => s.activity === "exited").slice(-8).reverse();
      return (
        <section key={m.id} className="machine">
          <header className="machine-header" title={m.error ?? undefined}>
            <span className={`dot ${m.online ? "online" : "offline"}`} />
            {m.label}
            {!m.online && <span className="machine-offline">offline</span>}
          </header>
          {live.length === 0 && <div className="empty">no running sessions</div>}
          {[...live, ...exited].map((s) => (
            <button
              key={s.key}
              className={`session ${s.activity} ${s.key === activeKey ? "active" : ""}`}
              onClick={() => onOpen(s)}
              title={s.meta.cwd}
            >
              <div className="session-row">
                <span className="session-name">{displayName(s)}</span>
                <span className={`badge ${s.activity}`}>{ACTIVITY_LABEL[s.activity]}</span>
              </div>
              <div className="session-cwd">{s.meta.cwd}</div>
              <UsageBadge s={s} />
            </button>
          ))}
        </section>
      );
    })}
  </nav>
);
