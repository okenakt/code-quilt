import { isPinned, togglePin } from "../pins";
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

const sumTokens = (c: { input: number; output: number; cacheRead: number; cacheCreation: number }) =>
  c.input + c.output + c.cacheRead + c.cacheCreation;

/** Badge with model, context usage and token totals (transcript totals when available). */
const UsageBadge = ({ s }: { s: SessionState }) => {
  const st = s.status;
  if (!st && !s.tokens) return null;
  const total = s.tokens ? sumTokens(s.tokens) : (st?.inputTokens ?? 0) + (st?.outputTokens ?? 0);
  const parts = [
    st?.model ?? null,
    st?.contextPercent != null ? `ctx ${Math.round(st.contextPercent)}%` : null,
    total ? `${formatTokens(total)} tok` : null,
  ].filter((p): p is string => p !== null);
  return <div className="session-usage">{parts.join(" · ")}</div>;
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
      {s.status?.costUsd != null && (
        <tfoot>
          <tr>
            <td colSpan={5}>≈ ${s.status.costUsd.toFixed(2)} API-equivalent</td>
          </tr>
        </tfoot>
      )}
    </table>
  );
};

type ItemProps = {
  s: SessionState;
  machine: MachineInfo | undefined;
  active: boolean;
  pinned: boolean;
  /** Show the machine label (in the pinned section, which mixes machines). */
  showMachine: boolean;
  onOpen: (s: SessionState) => void;
  onResume: (s: SessionState) => void;
};

const SessionItem = ({ s, machine, active, pinned, showMachine, onOpen, onResume }: ItemProps) => (
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
      <span className={`badge ${s.activity}`}>{ACTIVITY_LABEL[s.activity]}</span>
      {s.activity !== "exited" && (
        <button
          className={`pin ${pinned ? "on" : ""}`}
          title={pinned ? "unpin" : "pin to the front"}
          onClick={(e) => {
            e.stopPropagation();
            togglePin(s.key);
          }}
        >
          📌
        </button>
      )}
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
    <UsageBadge s={s} />
    {active && <TokenDetail s={s} />}
    {s.activity === "exited" && s.meta.agent_session_id && machine?.online && (
      <button
        className="session-resume"
        onClick={(e) => {
          e.stopPropagation();
          onResume(s);
        }}
      >
        resume
      </button>
    )}
  </div>
);

type Props = {
  machines: MachineInfo[];
  sessions: SessionState[];
  activeKey: string | null;
  onOpen: (s: SessionState) => void;
  onNew: (machine: string) => void;
  onResume: (s: SessionState) => void;
  pins: Set<string>;
};

/** Task list: pinned sessions first, then sessions grouped by machine, live ones first. */
export const SessionList = ({ machines, sessions, activeKey, onOpen, onNew, onResume, pins }: Props) => {
  const pinned = sessions.filter((s) => s.activity !== "exited" && isPinned(s, pins));
  const machineOf = (s: SessionState) => machines.find((m) => m.id === s.machine);
  return (
    <nav className="session-list">
      {pinned.length > 0 && (
        <section className="machine pinned">
          <header className="machine-header">Pinned</header>
          {pinned.map((s) => (
            <SessionItem
              key={s.key}
              s={s}
              machine={machineOf(s)}
              active={s.key === activeKey}
              pinned
              showMachine
              onOpen={onOpen}
              onResume={onResume}
            />
          ))}
        </section>
      )}
      {machines.map((m) => {
        const mine = sessions.filter((s) => s.machine === m.id && !pinned.includes(s));
        const live = mine.filter((s) => s.activity !== "exited");
        const exited = mine.filter((s) => s.activity === "exited").slice(-8).reverse();
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
            {live.length === 0 && <div className="empty">no running sessions</div>}
            {[...live, ...exited].map((s) => (
              <SessionItem
                key={s.key}
                s={s}
                machine={m}
                active={s.key === activeKey}
                pinned={false}
                showMachine={false}
                onOpen={onOpen}
                onResume={onResume}
              />
            ))}
          </section>
        );
      })}
    </nav>
  );
};
