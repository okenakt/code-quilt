import { useCallback, useEffect, useRef, useState } from "react";
import { DockviewReact, type DockviewApi, type DockviewReadyEvent } from "dockview-react";
import "dockview-react/dist/styles/dockview.css";
import { connectBackend, getMachines, getReplay, launchSession, refreshSessions, sessionKey, type Agent } from "./api";
import { ApprovalDialog } from "./components/approval-dialog";
import { NewSessionDialog } from "./components/new-session-dialog";
import { SessionList } from "./components/session-list";
import { TerminalPanel } from "./components/terminal-panel";
import { UsageBar } from "./components/usage-bar";
import { notify } from "./notify";
import { SHORTCUT_HELP, matchShortcut, type ShortcutAction } from "./shortcuts";
import { isPinned, usePins } from "./pins";
import { applyEvent, displayName, markExited, onSignal, setMachines, setSessions, useStore } from "./store";
import type { SessionState } from "./types";

const LAYOUT_KEY = "cq.layout.v2";
const components = { terminal: TerminalPanel };

const loadLayout = (): unknown => {
  try {
    const raw = localStorage.getItem(LAYOUT_KEY);
    return raw ? JSON.parse(raw) : null;
  } catch {
    return null;
  }
};

const saveLayout = (api: DockviewApi) => {
  try {
    localStorage.setItem(LAYOUT_KEY, JSON.stringify(api.toJSON()));
  } catch {
    // Layout persistence is a convenience only.
  }
};

/** Root component: task list, tiled terminals, approval dialog and notifications. */
export const App = () => {
  const { machines, sessions } = useStore();
  const apiRef = useRef<DockviewApi | null>(null);
  const [activeKey, setActiveKey] = useState<string | null>(null);
  const [approvalFocus, setApprovalFocus] = useState(0);
  const [newSessionFor, setNewSessionFor] = useState<string | null>(null);
  const sessionsRef = useRef<SessionState[]>(sessions);
  sessionsRef.current = sessions;
  const activeKeyRef = useRef(activeKey);
  activeKeyRef.current = activeKey;

  useEffect(() => {
    let off: (() => void) | null = null;
    let cancelled = false;
    void connectBackend({
      machines: setMachines,
      sessions: setSessions,
      event: applyEvent,
      data: (msg) => {
        if (msg.kind === "exit") markExited(msg.machine, msg.session);
      },
    }).then(async (o) => {
      if (cancelled) return o();
      off = o;
      const ms = await getMachines();
      setMachines(ms);
      const replay = await getReplay();
      replay.sessions.forEach((r) => setSessions(r.machine, r.sessions));
      replay.events
        .sort((a, b) => a.event.received_at_ms - b.event.received_at_ms)
        .forEach((r) => applyEvent(r.machine, r.session, r.event));
      void refreshSessions().catch(() => undefined);
    });
    return () => {
      cancelled = true;
      off?.();
    };
  }, []);

  useEffect(
    () =>
      onSignal((s) => {
        const name = displayName(s.session);
        const visible = document.hasFocus() && activeKeyRef.current === s.session.key;
        if (s.type === "approval") void notify(`Approval needed · ${name}`, s.session.prompt?.text.split("\n")[0] ?? "");
        else if (!visible) void notify(`Turn finished · ${name}`, (s.message ?? "").slice(0, 160));
      }),
    [],
  );
  const openSession = useCallback((s: SessionState) => {
    const api = apiRef.current;
    if (!api) return;
    const existing = api.getPanel(s.key);
    if (existing) {
      existing.api.setActive();
      return;
    }
    api.addPanel({
      id: s.key,
      component: "terminal",
      title: displayName(s),
      params: { machine: s.machine, session: s.meta.id },
    });
  }, []);

  /** Opens a tile for a session that may not be in the list yet (just launched). */
  const openById = useCallback((machine: string, id: string) => {
    const api = apiRef.current;
    const key = sessionKey(machine, id);
    if (!api || api.getPanel(key)) return;
    api.addPanel({ id: key, component: "terminal", title: id, params: { machine, session: id } });
  }, []);

  const launch = useCallback(
    async (machine: string, agent: Agent, cwd: string, args: string[], resume: string | null = null) => {
      const id = await launchSession(machine, agent, cwd, args, resume);
      openById(machine, id);
    },
    [openById],
  );

  const resumeSession = useCallback(
    (s: SessionState) => void launch(s.machine, s.meta.agent === "codex" ? "codex" : "claude", s.meta.cwd, [], s.meta.agent_session_id).catch((e: unknown) => alert(String(e))),
    [launch],
  );

  // Pinned sessions (manually, or tagged `queen`) open once per run in a group at the right edge.
  const pins = usePins();
  const autoOpened = useRef<Set<string>>(new Set());
  const [layoutReady, setLayoutReady] = useState(false);
  useEffect(() => {
    const api = apiRef.current;
    if (!api || !layoutReady) return;
    for (const s of sessions) {
      if (s.activity === "exited" || !isPinned(s, pins) || autoOpened.current.has(s.key)) continue;
      autoOpened.current.add(s.key);
      if (api.getPanel(s.key)) continue;
      const pinnedPanel = api.panels.find((p) => {
        const other = sessions.find((o) => o.key === p.id);
        return other !== undefined && isPinned(other, pins);
      });
      api.addPanel({
        id: s.key,
        component: "terminal",
        title: displayName(s),
        params: { machine: s.machine, session: s.meta.id },
        position: pinnedPanel ? { referencePanel: pinnedPanel.id, direction: "within" } : { direction: "right" },
        initialWidth: 560,
        inactive: true,
      });
    }
  }, [sessions, pins, layoutReady]);

  // Keep tab titles in sync with session names.
  useEffect(() => {
    const api = apiRef.current;
    if (!api) return;
    for (const s of sessions) {
      const p = api.getPanel(s.key);
      if (p && p.title !== displayName(s)) p.api.setTitle(displayName(s));
    }
  }, [sessions]);

  const runShortcut = useCallback(
    (a: ShortcutAction) => {
      const api = apiRef.current;
      if (!api) return;
      const panels = api.panels;
      const idx = panels.findIndex((p) => p.id === api.activePanel?.id);
      switch (a.type) {
        case "next-panel":
        case "prev-panel": {
          if (panels.length === 0) return;
          const step = a.type === "next-panel" ? 1 : -1;
          panels[(idx + step + panels.length) % panels.length].api.setActive();
          break;
        }
        case "close-panel":
          api.activePanel?.api.close();
          break;
        case "split-right":
        case "split-down": {
          const active = api.activePanel;
          const live = sessionsRef.current.filter((s) => s.activity !== "exited" && !api.getPanel(s.key));
          const target = live[0];
          if (!target || !active) return;
          api.addPanel({
            id: target.key,
            component: "terminal",
            title: displayName(target),
            params: { machine: target.machine, session: target.meta.id },
            position: { referencePanel: active.id, direction: a.type === "split-right" ? "right" : "below" },
          });
          break;
        }
        case "open-nth": {
          const live = sessionsRef.current.filter((s) => s.activity !== "exited");
          const s = live[a.index];
          if (s) openSession(s);
          break;
        }
        case "focus-approval":
          setApprovalFocus((n) => n + 1);
          break;
      }
    },
    [openSession],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const a = matchShortcut(e);
      if (!a) return;
      e.preventDefault();
      e.stopPropagation();
      runShortcut(a);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [runShortcut]);

  const onReady = (e: DockviewReadyEvent) => {
    apiRef.current = e.api;
    const saved = loadLayout();
    if (saved) {
      try {
        e.api.fromJSON(saved as Parameters<DockviewApi["fromJSON"]>[0]);
      } catch {
        e.api.clear();
      }
    }
    setLayoutReady(true);
    e.api.onDidLayoutChange(() => saveLayout(e.api));
    e.api.onDidActivePanelChange((ev) => setActiveKey(ev.panel?.id ?? null));
  };

  const pendingApprovals = sessions.filter((s) => s.prompt).length;

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          Code<span>Quilt</span>
        </div>
        <UsageBar sessions={sessions} machines={machines} />
        {pendingApprovals > 0 && (
          <button className="approval-chip" onClick={() => setApprovalFocus((n) => n + 1)}>
            {pendingApprovals} approval{pendingApprovals > 1 ? "s" : ""}
          </button>
        )}
      </header>
      <aside className="sidebar">
        <SessionList
          machines={machines}
          sessions={sessions}
          activeKey={activeKey}
          onOpen={openSession}
          onNew={setNewSessionFor}
          onResume={resumeSession}
          pins={pins}
        />
        <footer className="help">
          {SHORTCUT_HELP.map(([k, v]) => (
            <div key={k}>
              <kbd>{k}</kbd> {v}
            </div>
          ))}
        </footer>
      </aside>
      <main className="tiles">
        <DockviewReact
          className="dockview-theme-dark"
          components={components}
          onReady={onReady}
        />
        {apiRef.current?.panels.length === 0 && sessions.length > 0 && (
          <div className="tiles-empty">Select a session to open it as a tile.</div>
        )}
      </main>
      <ApprovalDialog sessions={sessions} machines={machines} focusToken={approvalFocus} />
      {newSessionFor && (
        <NewSessionDialog
          machines={machines}
          initialMachine={newSessionFor}
          onLaunch={(m, agent, cwd, args) => launch(m, agent, cwd, args)}
          onClose={() => setNewSessionFor(null)}
        />
      )}
    </div>
  );
};
