import { PanelLeft, PanelRight } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { connectBackend, getMachines, getReplay, launchSession, refreshSessions, restartDaemon, sessionKey, terminate, type Agent } from "./api";
import { ApprovalCards, pendingApprovals, promptKey } from "./components/approvals";
import { RightSidebar, SidebarResizer } from "./components/right-sidebar";
import { NewSessionDialog } from "./components/new-session-dialog";
import { SessionList } from "./components/session-list";
import { Workspace } from "./components/workspace";
import {
  activeTab,
  addTab,
  closePane,
  closeTab,
  cycleTab,
  dropSessions,
  focusedSession as focusedSessionKey,
  moveFocus,
  openInActiveTab,
  renameTab,
  selectTab,
  splitFocused,
  toggleZoom,
  useLayout,
  type Side,
} from "./layout";
import { BUILTIN_COMMANDS, BUILTIN_KEYS } from "./builtins";
import { invokePluginCommand, isRecording, targetKey, onStartCommands, pinnedNames, resolveKeymap, setActiveChords, startCommandSync, useCommandState } from "./commands";
import { CommandPalette } from "./components/command-palette";
import { ConfirmDialog, askConfirm } from "./components/confirm-dialog";
import { KeybindingsView } from "./components/keybindings-view";
import { chordOfEvent } from "./keys";
import { togglePin, usePins } from "./pins";
import { toggleSidebar, usePrefs } from "./prefs";
import { applyEvent, displayName, markExited, setMachines, setSessions, useStore } from "./store";
import type { SessionState } from "./types";

const sideOf = (command: string): Side => {
  const dir = command.replace(/^pane\.(split|focus)/, "").toLowerCase();
  return dir === "left" || dir === "right" || dir === "up" ? dir : "down";
};

/** Root component: session list, tabs of panes, usage, approvals and toasts. */
export const App = () => {
  const { machines, sessions } = useStore();
  const layout = useLayout();
  const activeKey = useMemo(() => focusedSessionKey(), [layout]);
  const [newSessionFor, setNewSessionFor] = useState<string | null>(null);
  const prefs = usePrefs();
  const [minimized, setMinimized] = useState<Set<string>>(new Set());
  const [approvalFocus, setApprovalFocus] = useState<{ key: string; token: number } | null>(null);
  /** Brings an approval card back and gives it the keyboard focus. */
  const showApproval = useCallback((key: string) => {
    setMinimized((m) => {
      const next = new Set(m);
      next.delete(key);
      return next;
    });
    setApprovalFocus((f) => ({ key, token: (f?.token ?? 0) + 1 }));
  }, []);
  const sessionsRef = useRef<SessionState[]>(sessions);
  sessionsRef.current = sessions;

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

  const openSession = useCallback((s: SessionState) => openInActiveTab(s.key), []);

  const launch = useCallback(
    async (machine: string, agent: Agent, cwd: string, args: string[], resume: string | null = null) => {
      const id = await launchSession(machine, agent, cwd, args, resume);
      openInActiveTab(sessionKey(machine, id));
    },
    [],
  );

  const pins = usePins();
  const uiConfig = useCommandState().config;
  const pinNames = useMemo(() => pinnedNames(uiConfig), [uiConfig]);

  // Panes of ended sessions close themselves.
  useEffect(() => {
    const ended = new Set(sessions.filter((s) => s.activity === "exited").map((s) => s.key));
    if (ended.size > 0) dropSessions((k) => ended.has(k));
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
  const [keybindingsOpen, setKeybindingsOpen] = useState(false);
  const [toasts, setToasts] = useState<{ id: number; ok: boolean; text: string }[]>([]);
  const toastId = useRef(0);
  const showToast = useCallback((ok: boolean, text: string) => {
    const id = ++toastId.current;
    setToasts((ts) => [...ts, { id, ok, text }]);
    setTimeout(() => setToasts((ts) => ts.filter((t) => t.id !== id)), ok ? 4000 : 10000);
  }, []);

  const resumeSession = useCallback(
    (s: SessionState) =>
      void launch(s.machine, s.meta.agent === "codex" ? "codex" : "claude", s.meta.cwd, [], s.meta.agent_session_id).catch((e: unknown) =>
        showToast(false, `Resume: ${String(e)}`),
      ),
    [launch, showToast],
  );

  /** Ends a session after the user confirms; the agent exits and is killed if it has not exited after 10s. */
  const endSession = useCallback(
    (s: SessionState) =>
      void askConfirm("End session", `End "${displayName(s)}" (${s.meta.id.slice(0, 8)})? The agent is asked to exit and is killed if it has not exited after 10 seconds.`, "End session").then(
        (ok) => {
          if (ok) terminate(s.machine, s.meta.id).catch((e: unknown) => showToast(false, String(e)));
        },
      ),
    [showToast],
  );

  /** The session of the focused pane, which session commands act on. */
  const focusedSession = (): SessionState | null => {
    const key = focusedSessionKey();
    return sessionsRef.current.find((s) => s.key === key) ?? null;
  };

  const runCommand = useCallback(
    (name: string, args: Record<string, string>) => {
      switch (name) {
        case "ui.palette":
          setPaletteOpen(true);
          return;
        case "ui.keybindings":
          setKeybindingsOpen((open) => !open);
          return;
        case "tab.new":
          addTab();
          return;
        case "tab.close":
          closeTab(activeTab().id);
          return;
        case "tab.rename":
          renameTab(activeTab().id, args.name ?? "");
          return;
        case "tab.next":
        case "tab.prev":
          cycleTab(name === "tab.next" ? 1 : -1);
          return;
        case "tab.goTo": {
          const t = layout.tabs[Number(args.index) - 1];
          if (t) selectTab(t.id);
          return;
        }
        case "pane.splitLeft":
        case "pane.splitRight":
        case "pane.splitUp":
        case "pane.splitDown":
          splitFocused(sideOf(name));
          return;
        case "pane.focusLeft":
        case "pane.focusRight":
        case "pane.focusUp":
        case "pane.focusDown":
          moveFocus(sideOf(name));
          return;
        case "pane.close":
          closePane(activeTab().focus);
          return;
        case "pane.zoom":
          toggleZoom();
          return;
        case "ui.approvals": {
          const newest = pendingApprovals(sessionsRef.current).at(-1);
          if (newest) showApproval(promptKey(newest));
          return;
        }
        case "ui.toggleLeftSidebar":
        case "ui.toggleRightSidebar":
          toggleSidebar(name === "ui.toggleLeftSidebar" ? "left" : "right");
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
          if (!s) return showToast(false, "No session pane is focused");
          if (name === "session.pin") return togglePin(s, pinNames);
          if (name === "session.copyId") {
            void navigator.clipboard.writeText(s.meta.id).then(() => showToast(true, `Copied ${s.meta.id}`));
            return;
          }
          endSession(s);
          return;
        }
      }
      invokePluginCommand(name, args)
        .then((msg) => showToast(true, msg && msg !== "delivered" ? msg : `${name}: done`))
        .catch((e: unknown) => showToast(false, `${name}: ${String(e)}`));
    },
    [showToast, showApproval, pinNames, layout],
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

  // Keys bound to commands are handled here, before the terminal sees them.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // The keybindings view is recording a new key; it handles the press itself.
      if (isRecording() || e.isComposing) return;
      const chord = chordOfEvent(e);
      const b = chord ? keymap.byChord.get(chord) : undefined;
      if (!b) return;
      e.preventDefault();
      e.stopPropagation();
      if (!e.repeat) runCommand(b.command, b.args);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [keymap, runCommand]);

  const waiting = pendingApprovals(sessions).length;

  return (
    <div
      className="app"
      style={{
        gridTemplateColumns: `${prefs.leftOpen ? `${prefs.leftWidth}px 4px ` : ""}1fr${prefs.rightOpen ? ` 4px ${prefs.rightWidth}px` : ""}`,
      }}
    >
      <header className="topbar">
        <div className="brand">
          Code<span>Quilt</span>
        </div>
        <div className="topbar-spacer" />
        {waiting > 0 && (
          <button className="approval-chip" onClick={() => runCommand("ui.approvals", {})}>
            {waiting} approval{waiting > 1 ? "s" : ""}
          </button>
        )}
        <button
          className={`sidebar-toggle ${prefs.leftOpen ? "on" : ""}`}
          title={prefs.leftOpen ? "Hide left sidebar" : "Show left sidebar"}
          aria-label="Toggle left sidebar"
          onClick={() => toggleSidebar("left")}
        >
          <PanelLeft size={16} />
        </button>
        <button
          className={`sidebar-toggle ${prefs.rightOpen ? "on" : ""}`}
          title={prefs.rightOpen ? "Hide right sidebar" : "Show right sidebar"}
          aria-label="Toggle right sidebar"
          onClick={() => toggleSidebar("right")}
        >
          <PanelRight size={16} />
        </button>
      </header>
      {prefs.leftOpen && (
        <aside className="sidebar">
          <SessionList
            machines={machines}
            sessions={sessions}
            activeKey={activeKey}
            onOpen={openSession}
            onNew={setNewSessionFor}
            onResume={resumeSession}
            onEnd={endSession}
            pins={pins}
            pinNames={pinNames}
          />
        </aside>
      )}
      {prefs.leftOpen && <SidebarResizer side="left" />}
      <Workspace
        sessions={sessions}
        onNewSession={() => setNewSessionFor("local")}
        onGone={(key) => dropSessions((k) => k === key)}
        onRefuse={(m) => showToast(false, m)}
        overlay={
          keybindingsOpen && (
            <KeybindingsView
              commands={allCommands.filter((c) => c.palette)}
              keymap={keymap}
              warnings={warnings}
              onError={(m) => showToast(false, m)}
              onClose={() => setKeybindingsOpen(false)}
            />
          )
        }
      />
      {prefs.rightOpen && <SidebarResizer side="right" />}
      {prefs.rightOpen && <RightSidebar sessions={sessions} machines={machines} onShowApproval={showApproval} />}
      <ConfirmDialog />
      {paletteOpen && (
        <CommandPalette
          commands={allCommands.filter((c) => c.palette)}
          keysOf={(name) => keymap.byTarget.get(targetKey(name, {})) ?? []}
          warnings={warnings}
          onRun={(c, args) => {
            setPaletteOpen(false);
            runCommand(c.name, args);
          }}
          onClose={() => setPaletteOpen(false)}
        />
      )}
      <div className="corner">
        <ApprovalCards
          sessions={sessions}
          machines={machines}
          minimized={minimized}
          onMinimize={(key) => setMinimized((m) => new Set(m).add(key))}
          focus={approvalFocus}
        />
        <div className="toasts">
          {toasts.map((t) => (
            <div key={t.id} className={`toast ${t.ok ? "ok" : "error"}`} onClick={() => setToasts((ts) => ts.filter((x) => x.id !== t.id))}>
              {t.text}
            </div>
          ))}
        </div>
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
