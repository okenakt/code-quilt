import { ChevronDown, ChevronRight, Pin, Plus, Trash2 } from "lucide-react";
import { useState, type CSSProperties, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";
import { trackPointer } from "../drag";
import { focusPane, panesOf, selectTab, useLayout } from "../layout";
import { setColumn, toggleFold, usePrefs, type Column, type Prefs } from "../prefs";
import { Fold, SidebarSection, isExpanded } from "./sidebar-section";
import { baseName } from "../path";
import { isPinned, togglePin, type Pins } from "../pins";
import { AgentBadge } from "./agent-badge";
import { SessionName } from "./session-name";
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

/** Details of a running session as `name: value` lines. */
const SessionDetails = ({ s, machine }: { s: SessionState; machine: MachineInfo | undefined }) => {
  const st = s.status;
  const t = s.tokens;
  const tokens = t ? t.input + t.output + t.cacheRead + t.cacheCreation : (st?.inputTokens ?? 0) + (st?.outputTokens ?? 0);
  const ctx = st?.contextPercent ?? null;
  const rows: [string, ReactNode][] = [
    ["machine", machine?.label ?? s.machine],
    ["cwd", s.meta.cwd],
    ...(st?.model ? [["model", st.model] as [string, ReactNode]] : []),
    ...(ctx !== null
      ? [
          [
            "context",
            <span className="ctx">
              <span className="ctx-track">
                <span className="ctx-fill" style={{ width: `${Math.min(100, ctx)}%` }} />
              </span>
              {Math.round(ctx)}%
            </span>,
          ] as [string, ReactNode],
        ]
      : []),
    ...(tokens > 0 ? [["tokens", formatTokens(tokens)] as [string, ReactNode]] : []),
  ];
  return (
    <dl className="session-details">
      {rows.map(([k, v]) => (
        <div key={k}>
          <dt>{k}:</dt>
          <dd>{v}</dd>
        </div>
      ))}
    </dl>
  );
};

/** Short session id; clicking copies the full id. */
const SessionId = ({ id }: { id: string }) => {
  const [copied, setCopied] = useState(false);
  return (
    <span
      role="button"
      tabIndex={-1}
      className={`session-id copyable ${copied ? "copied" : ""}`}
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

/** Drag handle on a column's right edge; the width is shared by every row. */
const ColResizer = ({ col }: { col: Column }) => {
  const prefs = usePrefs();
  const onPointerDown = (e: ReactPointerEvent) => {
    e.preventDefault();
    e.stopPropagation();
    const start = e.clientX;
    const width = prefs.columns[col];
    trackPointer((ev) => setColumn(col, width + ev.clientX - start));
  };
  return <span className="col-resizer" title="Drag to resize this column" onPointerDown={onPointerDown} onClick={(e) => e.stopPropagation()} />;
};

/** A fixed-width cell of a session row, so state changes never shift the columns. */
const Col = ({ col, children }: { col: Column; children: ReactNode }) => (
  <span className={`col col-${col}`}>
    {children}
    <ColResizer col={col} />
  </span>
);

type ItemProps = {
  s: SessionState;
  machine: MachineInfo | undefined;
  /** Fold key of the details, unique per place the session is listed so each expands on its own. */
  detailsKey: string;
  active: boolean;
  pinned: boolean;
  onPin: (s: SessionState) => void;
  onEnd: (s: SessionState) => void;
  onOpen: () => void;
};

/** A running session: name, id and state, with pin and end buttons and foldable details. */
const SessionItem = ({ s, machine, detailsKey, active, pinned, onPin, onEnd, onOpen }: ItemProps) => {
  const open = isExpanded(usePrefs(), detailsKey, false);
  return (
    <div
      role="button"
      tabIndex={0}
      className={`session ${s.activity} ${active ? "active" : ""}`}
      onClick={onOpen}
      onKeyDown={(e) => e.key === "Enter" && onOpen()}
      title={s.meta.cwd}
    >
      <div className="session-row">
        <button
          className="session-expand"
          title={open ? "hide details" : "show details"}
          onClick={(e) => {
            e.stopPropagation();
            toggleFold(detailsKey);
          }}
        >
          {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
        </button>
        <AgentBadge agent={s.meta.agent} />
        <Col col="name">
          <SessionName s={s} className="session-name" />
        </Col>
        <Col col="id">
          <SessionId id={s.meta.id} />
        </Col>
        <span className="session-state">
          {s.activity !== "exited" && (
            <span className={`badge ${s.activity}`} title={ACTIVITY_LABEL[s.activity]}>
              {ACTIVITY_LABEL[s.activity]}
            </span>
          )}
        </span>
        <button
          className={`pin ${pinned ? "on" : ""}`}
          title={pinned ? "unpin" : "pin to the front"}
          onClick={(e) => {
            e.stopPropagation();
            onPin(s);
          }}
        >
          <Pin size={12} />
        </button>
        <button
          className="end-session"
          title="end session (the agent exits; killed after 10s)"
          onClick={(e) => {
            e.stopPropagation();
            onEnd(s);
          }}
        >
          <Trash2 size={12} />
        </button>
      </div>
      {open && <SessionDetails s={s} machine={machine} />}
    </div>
  );
};

const EndedItem = ({ s, canResume, onResume }: { s: SessionState; canResume: boolean; onResume: (s: SessionState) => void }) => (
  <div className="ended" title={s.meta.cwd}>
    <span className="session-expand" />
    <AgentBadge agent={s.meta.agent} />
    <Col col="name">
      <span className="session-name">{displayName(s)}</span>
    </Col>
    <Col col="id">
      <SessionId id={s.meta.id} />
    </Col>
    <span className="ended-when">{ago(s.meta.ended_at_ms ?? s.meta.started_at_ms)}</span>
    {canResume && (
      <button className="session-resume" onClick={() => onResume(s)}>
        Resume
      </button>
    )}
  </div>
);

type GroupProps = { title: string; hint?: string; foldKey: string; byDefault: boolean; prefs: Prefs; children: ReactNode };

/** A foldable group inside a machine (Active, Ended, and Ended's directories). */
const Group = ({ title, hint, foldKey, byDefault, prefs, children }: GroupProps) => {
  const open = isExpanded(prefs, foldKey, byDefault);
  return (
    <div className="group">
      <button className="section-toggle" title={hint} onClick={() => toggleFold(foldKey)}>
        <Fold open={open} /> {title}
      </button>
      {open && <div className="tree">{children}</div>}
    </div>
  );
};

type EndedProps = { machine: MachineInfo; sessions: SessionState[]; onResume: (s: SessionState) => void };

const endedAt = (s: SessionState) => s.meta.ended_at_ms ?? s.meta.started_at_ms;

/** Ended sessions of a machine by working directory; the most recently used directory first. */
const EndedByDir = ({ prefs, ...props }: EndedProps & { prefs: Prefs }) => {
  const dirs = new Map<string, SessionState[]>();
  for (const s of [...props.sessions].sort((a, b) => endedAt(b) - endedAt(a))) {
    dirs.set(s.meta.cwd, [...(dirs.get(s.meta.cwd) ?? []), s]);
  }
  return [...dirs].map(([cwd, sessions]) => (
    <Group key={cwd} title={`${baseName(cwd)} (${sessions.length})`} hint={cwd} foldKey={`ended:${props.machine.id}:${cwd}`} byDefault={false} prefs={prefs}>
      <EndedList {...props} sessions={sessions} />
    </Group>
  ));
};

/** Ended sessions, newest first, the older ones behind "show all". */
const EndedList = ({ machine, sessions, onResume }: EndedProps) => {
  const [all, setAll] = useState(false);
  const sorted = [...sessions].sort((a, b) => endedAt(b) - endedAt(a));
  return (
    <>
      {(all ? sorted : sorted.slice(0, ENDED_LIMIT)).map((s) => (
        <EndedItem key={s.key} s={s} canResume={machine.online} onResume={onResume} />
      ))}
      {!all && sorted.length > ENDED_LIMIT && (
        <button className="section-more" onClick={() => setAll(true)}>
          Show all {sorted.length}
        </button>
      )}
    </>
  );
};

type ListProps = {
  prefs: Prefs;
  sessions: Map<string, SessionState>;
  machineOf: (s: SessionState) => MachineInfo | undefined;
  pinOf: (s: SessionState) => boolean;
  onPin: (s: SessionState) => void;
  onEnd: (s: SessionState) => void;
};

/** Each tab with the sessions shown in it; selecting one switches to its tab and pane. */
const TabsSection = ({ prefs, sessions, machineOf, pinOf, onPin, onEnd }: ListProps) => {
  const { tabs, active } = useLayout();
  return (
    <>
      {tabs.map((t) => {
        const key = `tab:${t.id}`;
        const open = isExpanded(prefs, key, true);
        const panes = panesOf(t.root);
        const empty = panes.filter((p) => p.session === null).length;
        return (
          <div key={t.id} className={`tab-group ${t.id === active ? "active" : ""}`}>
            <div className="tab-group-header">
              <button className="fold-button" onClick={() => toggleFold(key)}>
                <Fold open={open} />
              </button>
              <button className="tab-group-name" onClick={() => selectTab(t.id)}>
                {t.name}
              </button>
            </div>
            {open && (
              // Sessions line up with Machines (machine > Active > session) so rows have the same width.
              <div className="tree deep">
                {panes.map((p) => {
                  if (p.session === null) return null;
                  const s = sessions.get(p.session);
                  if (!s)
                    return (
                      <div key={p.id} className="empty">
                        {p.session.slice(p.session.indexOf("/") + 1, p.session.indexOf("/") + 9)} (unavailable)
                      </div>
                    );
                  return (
                    <SessionItem
                      key={p.id}
                      s={s}
                      machine={machineOf(s)}
                      detailsKey={`details:tab:${t.id}:${p.id}`}
                      active={t.id === active && t.focus === p.id}
                      pinned={pinOf(s)}
                      onPin={onPin}
                      onEnd={onEnd}
                      onOpen={() => {
                        selectTab(t.id);
                        focusPane(p.id);
                      }}
                    />
                  );
                })}
                {empty > 0 && <div className="empty">{empty} empty pane{empty > 1 ? "s" : ""}</div>}
              </div>
            )}
          </div>
        );
      })}
    </>
  );
};

type Props = {
  machines: MachineInfo[];
  sessions: SessionState[];
  activeKey: string | null;
  onOpen: (s: SessionState) => void;
  onNew: (machine: string) => void;
  onResume: (s: SessionState) => void;
  onEnd: (s: SessionState) => void;
  pins: Pins;
  /** Names pinned by default (`pinned` in ui.toml). */
  pinNames: string[];
};

/** Left sidebar: Pinned, Tabs and Machines, each of which can be folded. */
export const SessionList = ({ machines, sessions, activeKey, onOpen, onNew, onResume, onEnd, pins, pinNames }: Props) => {
  const prefs = usePrefs();
  const running = sessions.filter((s) => s.activity !== "exited");
  const pinOf = (s: SessionState) => isPinned(s, pins, pinNames);
  const onPin = (s: SessionState) => togglePin(s, pinNames);
  const machineOf = (s: SessionState) => machines.find((m) => m.id === s.machine);
  const item = (s: SessionState, place: string) => (
    <SessionItem
      key={s.key}
      s={s}
      machine={machineOf(s)}
      detailsKey={`details:${place}:${s.key}`}
      active={s.key === activeKey}
      pinned={pinOf(s)}
      onPin={onPin}
      onEnd={onEnd}
      onOpen={() => onOpen(s)}
    />
  );
  const pinned = running.filter(pinOf);
  return (
    <nav className="session-list" style={{ "--col-name": `${prefs.columns.name}px`, "--col-id": `${prefs.columns.id}px` } as CSSProperties}>
      <SidebarSection title="Pinned" foldKey="pinned" prefs={prefs}>
        <div className="tree deep">
          {pinned.length === 0 && <div className="empty">No pinned sessions</div>}
          {pinned.map((s) => item(s, "pinned"))}
        </div>
      </SidebarSection>
      <SidebarSection title="Tabs" foldKey="tabs" prefs={prefs}>
        <TabsSection prefs={prefs} sessions={new Map(sessions.map((s) => [s.key, s]))} machineOf={machineOf} pinOf={pinOf} onPin={onPin} onEnd={onEnd} />
      </SidebarSection>
      <SidebarSection title="Machines" foldKey="machines" prefs={prefs}>
        {machines.map((m) => {
          const active = running.filter((s) => s.machine === m.id);
          const ended = sessions.filter((s) => s.machine === m.id && s.activity === "exited");
          const key = `machine:${m.id}`;
          const open = isExpanded(prefs, key, true);
          return (
            <div key={m.id} className="machine">
              <header className="machine-header" title={m.error ?? undefined}>
                <button className="fold-button" onClick={() => toggleFold(key)}>
                  <Fold open={open} />
                </button>
                <span className={`dot ${m.online ? "online" : "offline"}`} />
                {m.label}
                {!m.online && <span className="machine-offline">offline</span>}
                {m.online && (
                  <button className="machine-new" title="New session" onClick={() => onNew(m.id)}>
                    <Plus size={12} />
                  </button>
                )}
              </header>
              {open && (
                <div className="tree">
                  <Group title={`Active (${active.length})`} foldKey={`active:${m.id}`} byDefault prefs={prefs}>
                    {active.length === 0 && <div className="empty">No active sessions</div>}
                    {active.map((s) => item(s, "machine"))}
                  </Group>
                  {ended.length > 0 && (
                    <Group title={`Ended (${ended.length})`} foldKey={`ended:${m.id}`} byDefault={false} prefs={prefs}>
                      <EndedByDir machine={m} sessions={ended} onResume={onResume} prefs={prefs} />
                    </Group>
                  )}
                </div>
              )}
            </div>
          );
        })}
      </SidebarSection>
    </nav>
  );
};
