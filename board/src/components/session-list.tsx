import { useState, type ReactNode } from "react";
import { focusPane, panesOf, selectTab, useLayout } from "../layout";
import { toggleFold, toggleSection, usePrefs, type LeftSection, type Prefs } from "../prefs";
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

type ActiveProps = {
  s: SessionState;
  machine: MachineInfo | undefined;
  active: boolean;
  pinned: boolean;
  onPin: (s: SessionState) => void;
  onOpen: (s: SessionState) => void;
};

const ActiveItem = ({ s, machine, active, pinned, onPin, onOpen }: ActiveProps) => {
  const detailsKey = `details:${s.key}`;
  const open = isExpanded(usePrefs(), detailsKey, false);
  return (
    <div
      role="button"
      tabIndex={0}
      className={`session ${s.activity} ${active ? "active" : ""}`}
      onClick={() => onOpen(s)}
      onKeyDown={(e) => e.key === "Enter" && onOpen(s)}
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
          {open ? "▾" : "▸"}
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


/** Ended sessions of a machine, newest first, collapsed by default. */
const EndedSection = ({ machine, sessions, prefs, onResume }: { machine: MachineInfo; sessions: SessionState[]; prefs: Prefs; onResume: (s: SessionState) => void }) => {
  const [all, setAll] = useState(false);
  if (sessions.length === 0) return null;
  const key = `ended:${machine.id}`;
  const open = isExpanded(prefs, key, false);
  const sorted = [...sessions].sort(
    (a, b) => (b.meta.ended_at_ms ?? b.meta.started_at_ms) - (a.meta.ended_at_ms ?? a.meta.started_at_ms),
  );
  const shown = all ? sorted : sorted.slice(0, ENDED_LIMIT);
  return (
    <div className="ended-section">
      <button className="section-toggle" onClick={() => toggleFold(key)}>
        <Fold open={open} /> Ended ({sessions.length})
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

const SECTION_TITLE: Record<LeftSection, string> = { pinned: "Pinned", tabs: "Tabs", machines: "Machines" };

/** Each tab with the sessions shown in it; selecting one switches to its tab and pane. */
const TabsSection = ({ prefs, sessions }: { prefs: Prefs; sessions: Map<string, SessionState> }) => {
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
              <ul className="tab-group-panes">
                {panes.map((p) => {
                  if (p.session === null) return null;
                  const s = sessions.get(p.session);
                  const id = p.session.slice(p.session.indexOf("/") + 1);
                  return (
                    <li key={p.id}>
                      <button
                        className={t.id === active && t.focus === p.id ? "focused" : ""}
                        onClick={() => {
                          selectTab(t.id);
                          focusPane(p.id);
                        }}
                      >
                        {s ? displayName(s) : id.slice(0, 8)} <span className="session-id">{id.slice(0, 8)}</span>
                        {s && s.activity !== "exited" && <span className={`badge ${s.activity}`}>{ACTIVITY_LABEL[s.activity]}</span>}
                      </button>
                    </li>
                  );
                })}
                {empty > 0 && <li className="empty">{empty} empty pane{empty > 1 ? "s" : ""}</li>}
              </ul>
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

/** Left sidebar: Pinned, Tabs and Machines, each of which can be hidden and folded. */
export const SessionList = ({ machines, sessions, activeKey, onOpen, onNew, onResume, pins, pinNames }: Props) => {
  const prefs = usePrefs();
  const running = sessions.filter((s) => s.activity !== "exited");
  const pinOf = (s: SessionState) => isPinned(s, pins, pinNames);
  const onPin = (s: SessionState) => togglePin(s, pinNames);
  const pinned = running.filter(pinOf);
  const machineOf = (s: SessionState) => machines.find((m) => m.id === s.machine);
  const shown = (s: LeftSection) => !prefs.hidden.includes(s);
  return (
    <nav className="session-list">
      <div className="section-switches">
        {(Object.keys(SECTION_TITLE) as LeftSection[]).map((s) => (
          <button key={s} className={shown(s) ? "on" : ""} title={`${shown(s) ? "Hide" : "Show"} ${SECTION_TITLE[s]}`} onClick={() => toggleSection(s)}>
            {SECTION_TITLE[s]}
          </button>
        ))}
      </div>
      {shown("pinned") && (
        <SidebarSection title={SECTION_TITLE.pinned} foldKey="pinned" prefs={prefs} onHide={() => toggleSection("pinned")}>
          {pinned.length === 0 && <div className="empty">No pinned sessions</div>}
          {pinned.map((s) => (
            <ActiveItem key={s.key} s={s} machine={machineOf(s)} active={s.key === activeKey} pinned onPin={onPin} onOpen={onOpen} />
          ))}
        </SidebarSection>
      )}
      {shown("tabs") && (
        <SidebarSection title={SECTION_TITLE.tabs} foldKey="tabs" prefs={prefs} onHide={() => toggleSection("tabs")}>
          <TabsSection prefs={prefs} sessions={new Map(sessions.map((s) => [s.key, s]))} />
        </SidebarSection>
      )}
      {shown("machines") && (
        <SidebarSection title={SECTION_TITLE.machines} foldKey="machines" prefs={prefs} onHide={() => toggleSection("machines")}>
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
                      +
                    </button>
                  )}
                </header>
                {open && (
                  <>
                    {active.length === 0 && <div className="empty">No active sessions</div>}
                    {active.map((s) => (
                      <ActiveItem key={s.key} s={s} machine={m} active={s.key === activeKey} pinned={pinOf(s)} onPin={onPin} onOpen={onOpen} />
                    ))}
                    <EndedSection machine={m} sessions={ended} prefs={prefs} onResume={onResume} />
                  </>
                )}
              </div>
            );
          })}
        </SidebarSection>
      )}
    </nav>
  );
};
