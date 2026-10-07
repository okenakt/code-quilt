import { useRef, useState, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";
import { Plus, X } from "lucide-react";
import {
  addTab,
  closePane,
  closeTab,
  focusPane,
  currentLayout,
  holdsSessionOf,
  movePane,
  moveTab,
  openInActiveTab,
  panesOf,
  renameTab,
  resizeSplit,
  selectTab,
  useLayout,
  type MoveTarget,
  type Side,
  type LayoutNode,
  type PaneNode,
  type SplitNode,
  type Tab,
} from "../layout";
import { trackPointer } from "../drag";
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
/** How long a dragged pane rests on a tab before that tab is shown. */
const HOLD_MS = 500;

type Box = { left: number; top: number; width: number; height: number };

/** What a drag would do if released now. */
type DropTarget =
  | { kind: "pane"; id: string; side: Side; preview: Box }
  | { kind: "tab"; id: string | null }
  | { kind: "self" }
  | null;

/** A drag in progress; `index` is where a dragged tab would land. */
type Drag = { kind: "pane" | "tab"; id: string; label: string; x: number; y: number; target: DropTarget; index: number };

type Ctx = {
  tab: Tab;
  sessions: Map<string, SessionState>;
  drag: Drag | null;
  startDrag: (e: ReactPointerEvent, kind: Drag["kind"], id: string, label: string) => void;
  onNewSession: () => void;
  onGone: (pane: PaneNode) => void;
};

/** The edge of a pane nearest to the pointer, so the diagonals split the pane into four zones. */
const sideAt = (r: DOMRect, x: number, y: number): Side => {
  const fx = (x - r.left) / r.width;
  const fy = (y - r.top) / r.height;
  const edges: [Side, number][] = [
    ["left", fx],
    ["right", 1 - fx],
    ["up", fy],
    ["down", 1 - fy],
  ];
  return edges.reduce((a, b) => (b[1] < a[1] ? b : a))[0];
};

/** The half of a pane a drop towards `side` would occupy, relative to `origin`. */
const previewBox = (r: DOMRect, side: Side, origin: DOMRect): Box => {
  const left = r.left - origin.left;
  const top = r.top - origin.top;
  const half = { width: r.width / 2, height: r.height / 2 };
  switch (side) {
    case "left":
      return { left, top, width: half.width, height: r.height };
    case "right":
      return { left: left + half.width, top, width: half.width, height: r.height };
    case "up":
      return { left, top, width: r.width, height: half.height };
    case "down":
      return { left, top: top + half.height, width: r.width, height: half.height };
  }
};

const TabBar = ({ tabs, active, ctx }: { tabs: Tab[]; active: string; ctx: Ctx }) => {
  const [editing, setEditing] = useState<string | null>(null);
  const drag = ctx.drag;
  const target = drag?.kind === "pane" && drag.target?.kind === "tab" ? drag.target.id : undefined;
  const dragged = drag?.kind === "tab" ? tabs.find((t) => t.id === drag.id) : undefined;
  const others = tabs.filter((t) => t !== dragged);
  const shown = dragged && drag ? [...others.slice(0, drag.index), dragged, ...others.slice(drag.index)] : tabs;
  return (
    <nav className="tab-bar">
      {shown.map((t) => (
        <div
          key={t.id}
          data-tab={t.id}
          className={`tab ${t.id === active ? "active" : ""} ${target === t.id ? "drop-target" : ""} ${t === dragged ? "dragging" : ""}`}
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
    trackPointer(move);
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
          title={pane.session ? "Close pane (the session keeps running)" : "Close pane"}
          onPointerDown={(e) => e.stopPropagation()}
          onClick={() => closePane(pane.id)}
        >
          <X size={13} />
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
  /** Tells the user why a drop was refused. */
  onRefuse: (message: string) => void;
  /** A view covering the whole main area (e.g. the keybindings view). */
  overlay?: ReactNode;
};

/** Main area: the tab bar and the active tab's panes. Only the active tab is rendered. */
export const Workspace = ({ sessions, onNewSession, onGone, onRefuse, overlay }: Props) => {
  const { tabs, active } = useLayout();
  const tab = tabs.find((t) => t.id === active) ?? tabs[0];
  const rootRef = useRef<HTMLElement>(null);
  const [drag, setDrag] = useState<Drag | null>(null);

  /**
   * Starts tracking a press on a pane header or tab; it becomes a drag once the pointer moves.
   * A pane resting on another tab shows that tab, so the pane can be placed there.
   */
  const startDrag = (e: ReactPointerEvent, kind: Drag["kind"], id: string, label: string) => {
    if (e.button !== 0) return;
    const sx = e.clientX;
    const sy = e.clientY;
    const srcTab = currentLayout().active;
    let current: Drag | null = null;
    let hold: { tab: string | null; timer?: number } = { tab: null };
    const alreadyOpen = (tabId: string) =>
      `"${label}" is already open in tab "${currentLayout().tabs.find((t) => t.id === tabId)?.name ?? tabId}"`;
    const hit = (x: number, y: number): DropTarget => {
      const el = document.elementFromPoint(x, y);
      const tabEl = el?.closest<HTMLElement>("[data-tab]");
      if (tabEl) return { kind: "tab", id: tabEl.dataset.tab ?? null };
      if (el?.closest("[data-tab-new]")) return { kind: "tab", id: null };
      const paneEl = el?.closest<HTMLElement>("[data-pane]");
      const origin = rootRef.current?.getBoundingClientRect();
      if (!paneEl || !origin) return null;
      if (paneEl.dataset.pane === id) return { kind: "self" };
      const r = paneEl.getBoundingClientRect();
      const side = sideAt(r, x, y);
      return { kind: "pane", id: paneEl.dataset.pane ?? "", side, preview: previewBox(r, side, origin) };
    };
    const tabIndex = (x: number) =>
      [...document.querySelectorAll<HTMLElement>("[data-tab]")].filter((el) => {
        const r = el.getBoundingClientRect();
        return el.dataset.tab !== id && r.left + r.width / 2 < x;
      }).length;
    const armHold = (t: DropTarget) => {
      const tabId = t?.kind === "tab" ? t.id : null;
      if (tabId === hold.tab) return;
      clearTimeout(hold.timer);
      hold = { tab: tabId };
      if (!tabId || tabId === currentLayout().active) return;
      hold.timer = window.setTimeout(() => (holdsSessionOf(srcTab, id, tabId) ? onRefuse(alreadyOpen(tabId)) : selectTab(tabId)), HOLD_MS);
    };
    const move = (ev: PointerEvent) => {
      if (!current && Math.hypot(ev.clientX - sx, ev.clientY - sy) < DRAG_THRESHOLD) return;
      ev.preventDefault();
      if (kind === "tab") {
        current = { kind, id, label, x: ev.clientX, y: ev.clientY, target: null, index: tabIndex(ev.clientX) };
      } else {
        current = { kind, id, label, x: ev.clientX, y: ev.clientY, target: hit(ev.clientX, ev.clientY), index: 0 };
        armHold(current.target);
      }
      setDrag(current);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      clearTimeout(hold.timer);
      setDrag(null);
      if (!current) return;
      if (kind === "tab") return moveTab(id, current.index);
      const t = current.target;
      const refuse = (message: string) => {
        selectTab(srcTab);
        onRefuse(message);
      };
      if (!t) return refuse("Drop the pane on a pane or a tab to move it");
      if (t.kind === "self") return selectTab(srcTab);
      const to: MoveTarget = t.kind === "pane" ? { tab: currentLayout().active, pane: t.id, side: t.side } : { tab: t.id };
      if (to.tab && holdsSessionOf(srcTab, id, to.tab)) return refuse(alreadyOpen(to.tab));
      movePane(srcTab, id, to);
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
      {overlay && <div className="main-overlay">{overlay}</div>}
      {preview && <div className="drop-preview" style={preview} />}
      {drag && (
        <div className="drag-ghost" style={{ left: drag.x + 12, top: drag.y + 12 }}>
          {drag.label}
        </div>
      )}
    </main>
  );
};
