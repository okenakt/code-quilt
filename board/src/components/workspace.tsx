import { useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { terminate } from "../api";
import {
  addTab,
  closePane,
  closeTab,
  focusPane,
  openInActiveTab,
  panesOf,
  renameTab,
  resizeSplit,
  selectTab,
  useLayout,
  type LayoutNode,
  type PaneNode,
  type SplitNode,
  type Tab,
} from "../layout";
import { displayName } from "../store";
import type { Activity, SessionState } from "../types";
import { TerminalView } from "./terminal-view";

const ACTIVITY_LABEL: Record<Activity, string> = {
  starting: "starting",
  working: "working",
  idle: "idle",
  approval: "needs approval",
  exited: "exited",
};

/** Smallest share of a split a pane may be dragged down to. */
const MIN_SHARE = 0.08;

type Ctx = {
  tab: Tab;
  sessions: Map<string, SessionState>;
  onNewSession: () => void;
  onGone: (pane: PaneNode) => void;
};

const TabBar = ({ tabs, active }: { tabs: Tab[]; active: string }) => {
  const [editing, setEditing] = useState<string | null>(null);
  return (
    <nav className="tab-bar">
      {tabs.map((t) => (
        <div key={t.id} className={`tab ${t.id === active ? "active" : ""}`} onClick={() => selectTab(t.id)} onDoubleClick={() => setEditing(t.id)}>
          {editing === t.id ? (
            <input
              autoFocus
              defaultValue={t.name}
              onBlur={(e) => {
                renameTab(t.id, e.target.value);
                setEditing(null);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") e.currentTarget.blur();
                if (e.key === "Escape") setEditing(null);
              }}
            />
          ) : (
            <span className="tab-name">{t.name}</span>
          )}
          <button
            className="tab-close"
            title="Close tab (sessions keep running)"
            onClick={(e) => {
              e.stopPropagation();
              closeTab(t.id);
            }}
          >
            ✕
          </button>
        </div>
      ))}
      <button className="tab-add" title="New tab" onClick={() => addTab()}>
        +
      </button>
    </nav>
  );
};

/** Drag handle between two children of a split; moves the boundary between them. */
const Splitter = ({ split, index }: { split: SplitNode; index: number }) => {
  const onPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    const container = e.currentTarget.parentElement;
    if (!container) return;
    e.preventDefault();
    const box = container.getBoundingClientRect();
    const total = split.dir === "row" ? box.width : box.height;
    const start = split.dir === "row" ? e.clientX : e.clientY;
    const pair = split.sizes[index] + split.sizes[index + 1];
    const move = (ev: PointerEvent) => {
      const delta = ((split.dir === "row" ? ev.clientX : ev.clientY) - start) / total;
      const a = Math.min(pair - MIN_SHARE, Math.max(MIN_SHARE, split.sizes[index] + delta));
      const sizes = [...split.sizes];
      sizes[index] = a;
      sizes[index + 1] = pair - a;
      resizeSplit(split.id, sizes);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };
  return <div className={`splitter ${split.dir}`} onPointerDown={onPointerDown} />;
};

const EmptyPane = ({ pane, ctx }: { pane: PaneNode; ctx: Ctx }) => {
  const shown = new Set(panesOf(ctx.tab.root).map((p) => p.session));
  const candidates = [...ctx.sessions.values()].filter((s) => s.activity !== "exited" && !shown.has(s.key));
  return (
    <div className="pane-empty">
      <button className="primary" onClick={ctx.onNewSession}>
        New session
      </button>
      {candidates.length > 0 && <div className="pane-empty-label">or open a running session</div>}
      <ul>
        {candidates.map((s) => (
          <li key={s.key}>
            <button
              onClick={() => {
                focusPane(pane.id);
                openInActiveTab(s.key);
              }}
            >
              {displayName(s)} <span className="session-id">{s.meta.id.slice(0, 8)}</span>
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
};

const PaneView = ({ pane, ctx }: { pane: PaneNode; ctx: Ctx }) => {
  const s = pane.session ? ctx.sessions.get(pane.session) : undefined;
  const [machine, id] = pane.session ? [pane.session.slice(0, pane.session.indexOf("/")), pane.session.slice(pane.session.indexOf("/") + 1)] : ["", ""];
  const focused = ctx.tab.focus === pane.id;
  return (
    <div className={`pane ${focused ? "focused" : ""} ${s?.activity === "approval" ? "approval" : ""}`} onPointerDown={() => !focused && focusPane(pane.id)}>
      <div className="pane-header">
        {pane.session ? (
          <>
            <span className="pane-title">{s ? displayName(s) : id.slice(0, 8)}</span>
            <span className="session-id">{id.slice(0, 8)}</span>
            {s && <span className={`badge ${s.activity}`}>{ACTIVITY_LABEL[s.activity]}</span>}
          </>
        ) : (
          <span className="pane-title muted">empty</span>
        )}
        <span className="pane-header-spacer" />
        {s && (
          <button
            className="pane-button"
            title="End session"
            onClick={() => {
              if (confirm(`End session "${displayName(s)}" (${s.meta.id.slice(0, 8)})?`)) {
                terminate(s.machine, s.meta.id).catch((e: unknown) => alert(String(e)));
              }
            }}
          >
            ⏻
          </button>
        )}
        <button className="pane-button" title={pane.session ? "Close pane (detach)" : "Close pane"} onClick={() => closePane(pane.id)}>
          ✕
        </button>
      </div>
      <div className="pane-body">
        {pane.session ? (
          <TerminalView machine={machine} session={id} focused={focused} onFocus={() => focusPane(pane.id)} onGone={() => ctx.onGone(pane)} />
        ) : (
          <EmptyPane pane={pane} ctx={ctx} />
        )}
      </div>
    </div>
  );
};

const NodeView = ({ node, ctx }: { node: LayoutNode; ctx: Ctx }) => {
  if (node.kind === "pane") return <PaneView pane={node} ctx={ctx} />;
  return (
    <div className={`split ${node.dir}`}>
      {node.children.flatMap((c, i) => [
        ...(i > 0 ? [<Splitter key={`${c.id}-s`} split={node} index={i - 1} />] : []),
        <div key={c.id} className="split-child" style={{ flexGrow: node.sizes[i], flexBasis: 0 }}>
          <NodeView node={c} ctx={ctx} />
        </div>,
      ])}
    </div>
  );
};

type Props = {
  sessions: SessionState[];
  onNewSession: () => void;
  onGone: (session: string) => void;
};

/** Main area: the tab bar and the active tab's panes. Only the active tab is rendered. */
export const Workspace = ({ sessions, onNewSession, onGone }: Props) => {
  const { tabs, active } = useLayout();
  const tab = tabs.find((t) => t.id === active) ?? tabs[0];
  const bySession = useRef(new Map<string, SessionState>());
  bySession.current = new Map(sessions.map((s) => [s.key, s]));
  const ctx: Ctx = { tab, sessions: bySession.current, onNewSession, onGone: (p) => p.session && onGone(p.session) };
  const zoomed = tab.zoom ? panesOf(tab.root).find((p) => p.id === tab.zoom) : undefined;
  return (
    <main className="workspace">
      <TabBar tabs={tabs} active={tab.id} />
      <div className="tab-body" key={tab.id}>
        {zoomed ? <PaneView pane={zoomed} ctx={ctx} /> : <NodeView node={tab.root} ctx={ctx} />}
      </div>
    </main>
  );
};
