import { useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { LogOut, Plus, Trash2, X } from "lucide-react";
import { terminate } from "../api";
import {
  addTab,
  closePane,
  closeTab,
  focusPane,
  movePane,
  movePaneToTab,
  moveTab,
  openInActiveTab,
  panesOf,
  renameTab,
  resizeSplit,
  selectTab,
  useLayout,
  type DropZone,
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
/** Pointer travel before a press on a handle becomes a drag. */
const DRAG_THRESHOLD = 5;

type Box = { left: number; top: number; width: number; height: number };

/** What a drag would do if released now. */
type DropTarget =
  | { kind: "pane"; id: string; zone: DropZone; preview: Box }
  | { kind: "tab"; id: string | null }
  | null;

type Drag = { kind: "pane" | "tab"; id: string; label: string; x: number; y: number; target: DropTarget };

type Ctx = {
  tab: Tab;
  sessions: Map<string, SessionState>;
  drag: Drag | null;
  startDrag: (e: ReactPointerEvent, kind: Drag["kind"], id: string, label: string) => void;
  onNewSession: () => void;
  onGone: (pane: PaneNode) => void;
};

/** The zone of a pane under the pointer: an edge within a quarter of the size, else the center. */
const zoneAt = (r: DOMRect, x: number, y: number): DropZone => {
  const fx = (x - r.left) / r.width;
  const fy = (y - r.top) / r.height;
  const edges: [DropZone, number][] = [
    ["left", fx],
    ["right", 1 - fx],
    ["up", fy],
    ["down", 1 - fy],
  ];
  const [zone, dist] = edges.reduce((a, b) => (b[1] < a[1] ? b : a));
  return dist < 0.25 ? zone : "center";
};

/** The part of a pane a drop into `zone` would occupy, relative to `origin`. */
const previewBox = (r: DOMRect, zone: DropZone, origin: DOMRect): Box => {
  const left = r.left - origin.left;
  const top = r.top - origin.top;
  const half = { width: r.width / 2, height: r.height / 2 };
  switch (zone) {
    case "left":
      return { left, top, width: half.width, height: r.height };
    case "right":
      return { left: left + half.width, top, width: half.width, height: r.height };
    case "up":
      return { left, top, width: r.width, height: half.height };
    case "down":
      return { left, top: top + half.height, width: r.width, height: half.height };
    case "center":
      return { left, top, width: r.width, height: r.height };
  }
};

const TabBar = ({ tabs, active, ctx }: { tabs: Tab[]; active: string; ctx: Ctx }) => {
  const [editing, setEditing] = useState<string | null>(null);
  const target = ctx.drag?.target?.kind === "tab" ? ctx.drag.target.id : undefined;
  return (
    <nav className="tab-bar">
      {tabs.map((t) => (
        <div
          key={t.id}
          data-tab={t.id}
          className={`tab ${t.id === active ? "active" : ""} ${target === t.id ? "drop-target" : ""}`}
          onClick={() => selectTab(t.id)}
          onDoubleClick={() => setEditing(t.id)}
          onPointerDown={(e) => editing !== t.id && ctx.startDrag(e, "tab", t.id, t.name)}
        >
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
            onPointerDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              e.stopPropagation();
              closeTab(t.id);
            }}
          >
            <X size={12} />
          </button>
        </div>
      ))}
      <button className={`tab-add ${target === null ? "drop-target" : ""}`} data-tab-new title="New tab (drop a pane here to move it to a new tab)" onClick={() => addTab()}>
        <Plus size={14} />
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
  const slash = pane.session?.indexOf("/") ?? -1;
  const [machine, id] = pane.session ? [pane.session.slice(0, slash), pane.session.slice(slash + 1)] : ["", ""];
  const focused = ctx.tab.focus === pane.id;
  const title = s ? displayName(s) : pane.session ? id.slice(0, 8) : "empty";
  return (
    <div
      data-pane={pane.id}
      className={`pane ${focused ? "focused" : ""} ${s?.activity === "approval" ? "approval" : ""} ${ctx.drag?.id === pane.id ? "dragging" : ""}`}
      onPointerDown={() => !focused && focusPane(pane.id)}
    >
      <div className="pane-header" title="Drag to move this pane" onPointerDown={(e) => ctx.startDrag(e, "pane", pane.id, title)}>
        <span className={`pane-title ${pane.session ? "" : "muted"}`}>{title}</span>
        {pane.session && <span className="session-id">{id.slice(0, 8)}</span>}
        {s && <span className={`badge ${s.activity}`}>{ACTIVITY_LABEL[s.activity]}</span>}
        <span className="pane-header-spacer" />
        <button
          className="pane-button"
          title={pane.session ? "Detach (close the pane; the session keeps running)" : "Close pane"}
          onPointerDown={(e) => e.stopPropagation()}
          onClick={() => closePane(pane.id)}
        >
          {pane.session ? <LogOut size={13} /> : <X size={13} />}
        </button>
        {s && (
          <button
            className="pane-button trash"
            title="End session"
            onPointerDown={(e) => e.stopPropagation()}
            onClick={() => {
              if (confirm(`End session "${displayName(s)}" (${s.meta.id.slice(0, 8)})?`)) {
                terminate(s.machine, s.meta.id).catch((e: unknown) => alert(String(e)));
              }
            }}
          >
            <Trash2 size={13} />
          </button>
        )}
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
  const rootRef = useRef<HTMLElement>(null);
  const [drag, setDrag] = useState<Drag | null>(null);

  /** Starts tracking a press on a pane header or tab; it becomes a drag once the pointer moves. */
  const startDrag = (e: ReactPointerEvent, kind: Drag["kind"], id: string, label: string) => {
    if (e.button !== 0) return;
    const sx = e.clientX;
    const sy = e.clientY;
    let current: Drag | null = null;
    const hit = (x: number, y: number): DropTarget => {
      const el = document.elementFromPoint(x, y);
      const tabEl = el?.closest<HTMLElement>("[data-tab]");
      if (tabEl) return { kind: "tab", id: tabEl.dataset.tab ?? null };
      if (el?.closest("[data-tab-new]")) return { kind: "tab", id: null };
      if (kind !== "pane") return null;
      const paneEl = el?.closest<HTMLElement>("[data-pane]");
      const origin = rootRef.current?.getBoundingClientRect();
      if (!paneEl || !origin || paneEl.dataset.pane === id) return null;
      const r = paneEl.getBoundingClientRect();
      const zone = zoneAt(r, x, y);
      return { kind: "pane", id: paneEl.dataset.pane ?? "", zone, preview: previewBox(r, zone, origin) };
    };
    const move = (ev: PointerEvent) => {
      if (!current && Math.hypot(ev.clientX - sx, ev.clientY - sy) < DRAG_THRESHOLD) return;
      ev.preventDefault();
      current = { kind, id, label, x: ev.clientX, y: ev.clientY, target: hit(ev.clientX, ev.clientY) };
      setDrag(current);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      const t = current?.target;
      setDrag(null);
      if (!t) return;
      if (kind === "tab") {
        if (t.kind === "tab" && t.id) moveTab(id, tabs.findIndex((x) => x.id === t.id));
      } else if (t.kind === "pane") {
        movePane(id, t.id, t.zone);
      } else {
        movePaneToTab(id, t.id);
      }
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  const ctx: Ctx = {
    tab,
    sessions: new Map(sessions.map((s) => [s.key, s])),
    drag,
    startDrag,
    onNewSession,
    onGone: (p) => p.session && onGone(p.session),
  };
  const zoomed = tab.zoom ? panesOf(tab.root).find((p) => p.id === tab.zoom) : undefined;
  const preview = drag?.target?.kind === "pane" ? drag.target.preview : null;
  return (
    <main className={`workspace ${drag ? "dragging" : ""}`} ref={rootRef}>
      <TabBar tabs={tabs} active={tab.id} ctx={ctx} />
      <div className="tab-body" key={tab.id}>
        {zoomed ? <PaneView pane={zoomed} ctx={ctx} /> : <NodeView node={tab.root} ctx={ctx} />}
      </div>
      {preview && <div className="drop-preview" style={preview} />}
      {drag && (
        <div className="drag-ghost" style={{ left: drag.x + 12, top: drag.y + 12 }}>
          {drag.label}
        </div>
      )}
    </main>
  );
};
