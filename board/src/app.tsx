import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { DockviewReact, type DockviewApi, type DockviewReadyEvent } from "dockview-react";
import "dockview-react/dist/styles/dockview.css";
import { connectBackend, getMachines, getReplay, launchSession, refreshSessions, restartDaemon, sessionKey, terminate, type Agent } from "./api";
import { ApprovalDialog } from "./components/approval-dialog";
import { NewSessionDialog } from "./components/new-session-dialog";
import { SessionList } from "./components/session-list";
import { TerminalPanel } from "./components/terminal-panel";
import { UsagePanel } from "./components/usage-panel";
import { notify } from "./notify";
import { BUILTIN_COMMANDS, BUILTIN_KEYS } from "./builtins";
import { invokePluginCommand, onStartCommands, resolveKeymap, setActiveChords, startCommandSync, useCommandState } from "./commands";
import { CommandPalette } from "./components/command-palette";
import { chordOfEvent, formatChord } from "./keys";
import { isPinned, togglePin, usePins } from "./pins";
import { applyEvent, displayName, markExited, onSignal, setMachines, setSessions, tileTitle, useStore } from "./store";
import type { SessionState } from "./types";

const LAYOUT_KEY = "cq.layout.v2";
const USAGE_OPEN_KEY = "cq.usage-open";

const loadUsageOpen = (): boolean => {
  try {
    return localStorage.getItem(USAGE_OPEN_KEY) !== "0";
  } catch {
    return true;
  }
};
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
  const [usageOpen, setUsageOpen] = useState(loadUsageOpen);
  const toggleUsage = useCallback(() => {
    setUsageOpen((open) => {
      try {
        localStorage.setItem(USAGE_OPEN_KEY, open ? "0" : "1");
      } catch {
        // Remembering the toggle is a convenience only.
      }
      return !open;
    });
  }, []);
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
      title: tileTitle(s),
      params: { machine: s.machine, session: s.meta.id },
    });
  }, []);

  /** Opens a tile for a session that may not be in the list yet (just launched). */
  const openById = useCallback((machine: string, id: string) => {
    const api = apiRef.current;
    const key = sessionKey(machine, id);
    if (!api || api.getPanel(key)) return;
    api.addPanel({ id: key, component: "terminal", title: id.slice(0, 8), params: { machine, session: id } });
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
        title: tileTitle(s),
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
      if (p && p.title !== tileTitle(s)) p.api.setTitle(tileTitle(s));
    }
  }, [sessions]);

  const { plugins, pluginKeys, config, loaded } = useCommandState();
  useEffect(() => {
    let off: (() => void) | null = null;
    let cancelled = false;
    void startCommandSync().then((o) => (cancelled ? o() : (off = o)));
    return () => {
      cancelled = true;
      off?.();
    };
  }, []);

  const allCommands = useMemo(() => [...BUILTIN_COMMANDS, ...plugins], [plugins]);
  const keymap = useMemo(
    () => resolveKeymap(BUILTIN_KEYS, pluginKeys, config.keybindings, new Set(allCommands.map((c) => c.name))),
    [pluginKeys, config.keybindings, allCommands],
  );
  setActiveChords(keymap.byChord.keys());
  const warnings = [...config.errors, ...keymap.warnings];

  const [paletteOpen, setPaletteOpen] = useState(false);
  const [toasts, setToasts] = useState<{ id: number; ok: boolean; text: string }[]>([]);
  const toastId = useRef(0);
  const showToast = useCallback((ok: boolean, text: string) => {
    const id = ++toastId.current;
    setToasts((ts) => [...ts, { id, ok, text }]);
    setTimeout(() => setToasts((ts) => ts.filter((t) => t.id !== id)), ok ? 4000 : 10000);
  }, []);

  /** The session of the active tile, which session commands act on. */
  const focusedSession = (): SessionState | null => {
    const id = apiRef.current?.activePanel?.id;
    return sessionsRef.current.find((s) => s.key === id) ?? null;
  };

  const runCommand = useCallback(
    (name: string, args: Record<string, string>) => {
      const api = apiRef.current;
      const panels = api?.panels ?? [];
      const idx = panels.findIndex((p) => p.id === api?.activePanel?.id);
      switch (name) {
        case "ui.palette":
          setPaletteOpen(true);
          return;
        case "ui.nextTile":
        case "ui.prevTile": {
          if (panels.length === 0) return;
          const step = name === "ui.nextTile" ? 1 : -1;
          panels[(idx + step + panels.length) % panels.length].api.setActive();
          return;
        }
        case "ui.closeTile":
          api?.activePanel?.api.close();
          return;
        case "ui.splitRight":
        case "ui.splitDown": {
          const active = api?.activePanel;
          const live = sessionsRef.current.filter((s) => s.activity !== "exited" && !api?.getPanel(s.key));
          const target = live[0];
          if (!api || !target || !active) return;
          api.addPanel({
            id: target.key,
            component: "terminal",
            title: tileTitle(target),
            params: { machine: target.machine, session: target.meta.id },
            position: { referencePanel: active.id, direction: name === "ui.splitRight" ? "right" : "below" },
          });
          return;
        }
        case "ui.openSession": {
          const live = sessionsRef.current.filter((s) => s.activity !== "exited");
          const s = live[Number(args.index) - 1];
          if (s) openSession(s);
          return;
        }
        case "ui.approvals":
          setApprovalFocus((n) => n + 1);
          return;
        case "ui.toggleUsage":
          toggleUsage();
          return;
        case "ui.newSession":
          setNewSessionFor(args.machine || "local");
          return;
        case "daemon.restart": {
          const machine = args.machine || "local";
          restartDaemon(machine)
            .then(() => showToast(true, `Restarting the daemon on ${machine}`))
            .catch((e: unknown) => showToast(false, `daemon.restart: ${String(e)}`));
          return;
        }
        case "session.end":
        case "session.pin":
        case "session.copyId": {
          const s = focusedSession();
          if (!s) return showToast(false, "No session tile is focused");
          if (name === "session.pin") return togglePin(s);
          if (name === "session.copyId") {
            void navigator.clipboard.writeText(s.meta.id).then(() => showToast(true, `Copied ${s.meta.id}`));
            return;
          }
          if (confirm(`End session "${displayName(s)}" (${s.meta.id.slice(0, 8)})?`)) {
            terminate(s.machine, s.meta.id).catch((e: unknown) => showToast(false, String(e)));
          }
          return;
        }
      }
      invokePluginCommand(name, args)
        .then((msg) => showToast(true, msg && msg !== "delivered" ? msg : `${name}: done`))
        .catch((e: unknown) => showToast(false, `${name}: ${String(e)}`));
    },
    [openSession, showToast, toggleUsage],
  );

  // Run ui.toml's [[on_start]] commands once the settings are read and the daemon is reachable.
  const startHooksRan = useRef(false);
  const localOnline = machines.some((m) => m.id === "local" && m.online);
  useEffect(() => {
    if (startHooksRan.current || !loaded || !localOnline) return;
    startHooksRan.current = true;
    const list = onStartCommands(config);
    if (typeof list === "string") return showToast(false, list);
    list.forEach((c) => runCommand(c.command, c.args));
  }, [loaded, localOnline, config, runCommand, showToast]);

  // Keys bound to commands are handled here, before the terminal sees them. Hold commands
  // (push-to-talk) get press on key down and release on key up.
  const held = useRef(new Map<string, string>());
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.type === "keyup") {
        const command = held.current.get(e.code);
        if (command) {
          held.current.delete(e.code);
          void invokePluginCommand(command, {}, "release").catch(() => undefined);
        }
        return;
      }
      if (e.isComposing) return;
      const chord = chordOfEvent(e);
      const b = chord ? keymap.byChord.get(chord) : undefined;
      if (!b) return;
      e.preventDefault();
      e.stopPropagation();
      const cmd = allCommands.find((c) => c.name === b.command);
      if (cmd?.hold) {
        if (!e.repeat) {
          held.current.set(e.code, cmd.name);
          void invokePluginCommand(cmd.name, b.args, "press").catch(() => undefined);
        }
        return;
      }
      if (!e.repeat) runCommand(b.command, b.args);
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("keyup", onKey, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("keyup", onKey, true);
    };
  }, [keymap, allCommands, runCommand]);

  /** Short key help for the sidebar footer, taken from the effective key map. */
  const keyHelp: [string, string][] = [
    ["ui.palette", "commands"],
    ["ui.nextTile", "next tile"],
    ["ui.prevTile", "previous tile"],
    ["ui.splitRight", "split right"],
    ["ui.splitDown", "split down"],
    ["ui.closeTile", "close tile"],
    ["ui.approvals", "approvals"],
  ].flatMap(([command, label]) => {
    const chord = keymap.byCommand.get(command)?.[0];
    return chord ? [[formatChord(chord), label] as [string, string]] : [];
  });
  if (keymap.byChord.get("alt+1")?.command === "ui.openSession") keyHelp.unshift(["Alt+1…9", "open session"]);

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
    <div className={`app ${usageOpen ? "with-usage" : ""}`}>
      <header className="topbar">
        <div className="brand">
          Code<span>Quilt</span>
        </div>
        <div className="topbar-spacer" />
        {pendingApprovals > 0 && (
          <button className="approval-chip" onClick={() => setApprovalFocus((n) => n + 1)}>
            {pendingApprovals} approval{pendingApprovals > 1 ? "s" : ""}
          </button>
        )}
        <button
          className={`usage-toggle ${usageOpen ? "on" : ""}`}
          title={usageOpen ? "Hide usage sidebar" : "Show usage sidebar"}
          aria-label="Toggle usage sidebar"
          onClick={toggleUsage}
        >
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3">
            <rect x="1.5" y="2.5" width="13" height="11" rx="1.5" />
            <line x1="10" y1="2.5" x2="10" y2="13.5" />
            {usageOpen && <rect x="10" y="2.5" width="4.5" height="11" fill="currentColor" stroke="none" opacity="0.5" />}
          </svg>
        </button>
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
          {keyHelp.map(([k, v]) => (
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
      {usageOpen && <UsagePanel sessions={sessions} machines={machines} />}
      <ApprovalDialog sessions={sessions} machines={machines} focusToken={approvalFocus} />
      {paletteOpen && (
        <CommandPalette
          commands={allCommands.filter((c) => c.palette)}
          keysOf={(name) => keymap.byCommand.get(name) ?? []}
          warnings={warnings}
          onRun={(c, args) => {
            setPaletteOpen(false);
            runCommand(c.name, args);
          }}
          onClose={() => setPaletteOpen(false)}
        />
      )}
      <div className="toasts">
        {toasts.map((t) => (
          <div key={t.id} className={`toast ${t.ok ? "ok" : "error"}`} onClick={() => setToasts((ts) => ts.filter((x) => x.id !== t.id))}>
            {t.text}
          </div>
        ))}
      </div>
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
