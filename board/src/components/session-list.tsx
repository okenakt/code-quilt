import { ChevronDown, ChevronRight, Pin, Plus, Trash2 } from "lucide-react";
import { useState, type ReactNode } from "react";
import { focusPane, panesOf, selectTab, useLayout } from "../layout";
import { toggleFold, usePrefs, type Prefs } from "../prefs";
import { Fold, SidebarSection, isExpanded } from "./sidebar-section";
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

type ItemProps = {
  s: SessionState;
  machine: MachineInfo | undefined;
  /** Fold key of the details, unique per place the session is listed so each expands on its own. */
  detailsKey: string;
  active: boolean;
  pinned: boolean;
  onPin: (s: SessionState) => void;
  onOpen: () => void;
};

/** A running session: name, id and state, with pin and end buttons and foldable details. */
const SessionItem = ({ s, machine, detailsKey, active, pinned, onPin, onOpen }: ItemProps) => {
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
        <span className="session-name">{displayName(s)}</span>
        <SessionId id={s.meta.id} />
        {s.activity !== "exited" && <span className={`badge ${s.activity}`}>{ACTIVITY_LABEL[s.activity]}</span>}
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
            if (confirm(`End session "${displayName(s)}" (${s.meta.id.slice(0, 8)})?`)) {
              terminate(s.machine, s.meta.id).catch((err: unknown) => alert(String(err)));
            }
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

/** A foldable group inside a machine (Active, Ended). */
const Group = ({ title, foldKey, byDefault, prefs, children }: { title: string; foldKey: string; byDefault: boolean; prefs: Prefs; children: ReactNode }) => {
  const open = isExpanded(prefs, foldKey, byDefault);
  return (
    <div className="group">
      <button className="section-toggle" onClick={() => toggleFold(foldKey)}>
        <Fold open={open} /> {title}
      </button>
      {open && <div className="tree">{children}</div>}
    </div>
  );
};

/** Ended sessions of a machine, newest first. */
const EndedList = ({ machine, sessions, onResume }: { machine: MachineInfo; sessions: SessionState[]; onResume: (s: SessionState) => void }) => {
  const [all, setAll] = useState(false);
  const sorted = [...sessions].sort(
    (a, b) => (b.meta.ended_at_ms ?? b.meta.started_at_ms) - (a.meta.ended_at_ms ?? a.meta.started_at_ms),
  );
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
};

/** Each tab with the sessions shown in it; selecting one switches to its tab and pane. */
const TabsSection = ({ prefs, sessions, machineOf, pinOf, onPin }: ListProps) => {
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
  pins: Pins;
  /** Names pinned by default (`pinned` in ui.toml). */
  pinNames: string[];
};

/** Left sidebar: Pinned, Tabs and Machines, each of which can be folded. */
export const SessionList = ({ machines, sessions, activeKey, onOpen, onNew, onResume, pins, pinNames }: Props) => {
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
      onOpen={() => onOpen(s)}
    />
  );
  const pinned = running.filter(pinOf);
  return (
    <nav className="session-list">
      <SidebarSection title="Pinned" foldKey="pinned" prefs={prefs}>
        <div className="tree deep">
          {pinned.length === 0 && <div className="empty">No pinned sessions</div>}
          {pinned.map((s) => item(s, "pinned"))}
        </div>
      </SidebarSection>
      <SidebarSection title="Tabs" foldKey="tabs" prefs={prefs}>
        <TabsSection prefs={prefs} sessions={new Map(sessions.map((s) => [s.key, s]))} machineOf={machineOf} pinOf={pinOf} onPin={onPin} />
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
                      <EndedList machine={m} sessions={ended} onResume={onResume} />
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
