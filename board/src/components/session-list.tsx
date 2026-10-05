import { useEffect, useState } from "react";
import { terminate } from "../api";
import { isPinned, togglePin, type Pins } from "../pins";
import { displayName } from "../store";
import type { Activity, MachineInfo, SessionState } from "../types";

const ACTIVITY_LABEL: Record<Exclude<Activity, "exited">, string> = {
  starting: "starting",
  working: "working",
  idle: "idle",
  approval: "needs approval",
};

/** Ended sessions listed before "show all". */
const ENDED_LIMIT = 20;

const formatTokens = (n: number): string =>
  n >= 1000 ? `${(n / 1000).toFixed(n >= 100_000 ? 0 : 1)}k` : String(n);

const ago = (ms: number): string => {
  const mins = Math.max(0, Math.round((Date.now() - ms) / 60000));
  if (mins < 60) return `${mins}m ago`;
  if (mins < 48 * 60) return `${Math.round(mins / 60)}h ago`;
  return `${Math.round(mins / 1440)}d ago`;
};

/** Model, context usage and token total of a running session. */
const UsageLine = ({ s }: { s: SessionState }) => {
  const st = s.status;
  const t = s.tokens;
  const total = t ? t.input + t.output + t.cacheRead + t.cacheCreation : (st?.inputTokens ?? 0) + (st?.outputTokens ?? 0);
  const ctx = st?.contextPercent ?? null;
  if (!st?.model && ctx === null && !total) return null;
  return (
    <div className="session-usage">
      {st?.model && <span>{st.model}</span>}
      {ctx !== null && (
        <span className="ctx" title={`context ${Math.round(ctx)}% used`}>
          ctx
          <span className="ctx-track">
            <span className="ctx-fill" style={{ width: `${Math.min(100, ctx)}%` }} />
          </span>
          {Math.round(ctx)}%
        </span>
      )}
      {total > 0 && <span>{formatTokens(total)} tokens</span>}
    </div>
  );
};

/** Per-model token breakdown shown for the selected session. */
const TokenDetail = ({ s }: { s: SessionState }) => {
  if (!s.tokens) return null;
  return (
    <table className="token-detail">
      <thead>
        <tr>
          <th>model</th>
          <th>in</th>
          <th>out</th>
          <th>cache r</th>
          <th>cache w</th>
        </tr>
      </thead>
      <tbody>
        {Object.entries(s.tokens.byModel).map(([model, c]) => (
          <tr key={model}>
            <td>{model.replace(/^claude-/, "")}</td>
            <td>{formatTokens(c.input)}</td>
            <td>{formatTokens(c.output)}</td>
            <td>{formatTokens(c.cacheRead)}</td>
            <td>{formatTokens(c.cacheCreation)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
};

/** Short session id; clicking copies the full id. */
const SessionId = ({ id }: { id: string }) => {
  const [copied, setCopied] = useState(false);
  return (
    <span
      role="button"
      tabIndex={-1}
      className={`session-id ${copied ? "copied" : ""}`}
      title={copied ? "copied" : `${id} (click to copy)`}
      onClick={(e) => {
        e.stopPropagation();
        void navigator.clipboard.writeText(id).then(() => {
          setCopied(true);
          setTimeout(() => setCopied(false), 1200);
        });
      }}
    >
      {id.slice(0, 8)}
    </span>
  );
};

type ActiveProps = {
  s: SessionState;
  machine: MachineInfo | undefined;
  active: boolean;
  pinned: boolean;
  /** Show the machine label (in the pinned section, which mixes machines). */
  showMachine: boolean;
  onOpen: (s: SessionState) => void;
};

const ActiveItem = ({ s, machine, active, pinned, showMachine, onOpen }: ActiveProps) => (
  <div
    role="button"
    tabIndex={0}
    className={`session ${s.activity} ${active ? "active" : ""}`}
    onClick={() => onOpen(s)}
    onKeyDown={(e) => e.key === "Enter" && onOpen(s)}
    title={s.meta.cwd}
  >
    <div className="session-row">
      <span className="session-name">{displayName(s)}</span>
      <SessionId id={s.meta.id} />
      {s.activity !== "exited" && <span className={`badge ${s.activity}`}>{ACTIVITY_LABEL[s.activity]}</span>}
      <button
        className={`pin ${pinned ? "on" : ""}`}
        title={pinned ? "unpin" : "pin to the front"}
        onClick={(e) => {
          e.stopPropagation();
          togglePin(s);
        }}
      >
        📌
      </button>
      <button
        className="end-session"
        title="end session (the agent exits; killed after 10s)"
        onClick={(e) => {
          e.stopPropagation();
          if (confirm(`End session "${displayName(s)}" (${s.meta.id.slice(0, 8)})?`)) {
            terminate(s.machine, s.meta.id).catch((err: unknown) => alert(String(err)));
          }
        }}
      >
        ✕
      </button>
    </div>
    <div className="session-cwd">
      {showMachine && machine ? `${machine.label} · ` : ""}
      {s.meta.cwd}
    </div>
    {s.meta.tags.length > 0 && (
      <div className="session-tags">
        {s.meta.tags.map((t) => (
          <span key={t} className="tag">
            {t}
          </span>
        ))}
      </div>
    )}
    <UsageLine s={s} />
    {active && <TokenDetail s={s} />}
  </div>
);

const EndedItem = ({ s, canResume, onResume }: { s: SessionState; canResume: boolean; onResume: (s: SessionState) => void }) => (
  <div className="ended" title={s.meta.cwd}>
    <span className="session-name">{displayName(s)}</span>
    <SessionId id={s.meta.id} />
    <span className="ended-when">{ago(s.meta.ended_at_ms ?? s.meta.started_at_ms)}</span>
    {canResume && s.meta.agent_session_id && (
      <button className="session-resume" onClick={() => onResume(s)}>
        Resume
      </button>
    )}
  </div>
);

const openKey = (machine: string) => `cq.ended-open.${machine}`;

const loadOpen = (machine: string): boolean => {
  try {
    return localStorage.getItem(openKey(machine)) === "1";
  } catch {
    return false;
  }
};

/** Ended sessions of a machine, newest first, collapsed by default. */
const EndedSection = ({ machine, sessions, onResume }: { machine: MachineInfo; sessions: SessionState[]; onResume: (s: SessionState) => void }) => {
  const [open, setOpen] = useState(() => loadOpen(machine.id));
  const [all, setAll] = useState(false);
  useEffect(() => {
    try {
      localStorage.setItem(openKey(machine.id), open ? "1" : "0");
    } catch {
      // Remembering the section state is a convenience only.
    }
  }, [machine.id, open]);
  if (sessions.length === 0) return null;
  const sorted = [...sessions].sort(
    (a, b) => (b.meta.ended_at_ms ?? b.meta.started_at_ms) - (a.meta.ended_at_ms ?? a.meta.started_at_ms),
  );
  const shown = all ? sorted : sorted.slice(0, ENDED_LIMIT);
  return (
    <div className="ended-section">
      <button className="section-toggle" onClick={() => setOpen(!open)}>
        {open ? "▾" : "▸"} Ended ({sessions.length})
      </button>
      {open && (
        <>
          {shown.map((s) => (
            <EndedItem key={s.key} s={s} canResume={machine.online} onResume={onResume} />
          ))}
          {!all && sorted.length > ENDED_LIMIT && (
            <button className="section-more" onClick={() => setAll(true)}>
              Show all {sorted.length}
            </button>
          )}
        </>
      )}
    </div>
  );
};

type Props = {
  machines: MachineInfo[];
  sessions: SessionState[];
  activeKey: string | null;
  onOpen: (s: SessionState) => void;
  onNew: (machine: string) => void;
  onResume: (s: SessionState) => void;
  pins: Pins;
};

/** Pinned sessions first (also listed under their machine), then each machine's sessions. */
export const SessionList = ({ machines, sessions, activeKey, onOpen, onNew, onResume, pins }: Props) => {
  const running = sessions.filter((s) => s.activity !== "exited");
  const pinned = running.filter((s) => isPinned(s, pins));
  const machineOf = (s: SessionState) => machines.find((m) => m.id === s.machine);
  return (
    <nav className="session-list">
      {pinned.length > 0 && (
        <section className="machine pinned">
          <header className="machine-header">Pinned</header>
          {pinned.map((s) => (
            <ActiveItem key={s.key} s={s} machine={machineOf(s)} active={s.key === activeKey} pinned showMachine onOpen={onOpen} />
          ))}
        </section>
      )}
      {machines.map((m) => {
        const active = running.filter((s) => s.machine === m.id);
        const ended = sessions.filter((s) => s.machine === m.id && s.activity === "exited");
        return (
          <section key={m.id} className="machine">
            <header className="machine-header" title={m.error ?? undefined}>
              <span className={`dot ${m.online ? "online" : "offline"}`} />
              {m.label}
              {!m.online && <span className="machine-offline">offline</span>}
              {m.online && (
                <button className="machine-new" title="New session" onClick={() => onNew(m.id)}>
                  +
                </button>
              )}
            </header>
            {active.length === 0 && <div className="empty">No active sessions</div>}
            {active.map((s) => (
              <ActiveItem key={s.key} s={s} machine={m} active={s.key === activeKey} pinned={isPinned(s, pins)} showMachine={false} onOpen={onOpen} />
            ))}
            <EndedSection machine={m} sessions={ended} onResume={onResume} />
          </section>
        );
      })}
    </nav>
  );
};
