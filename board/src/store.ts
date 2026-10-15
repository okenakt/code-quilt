import { useSyncExternalStore } from "react";
import { sessionKey } from "./api";
import { asRecord, num, str } from "./json";
import type {
  Activity,
  AgentEvent,
  MachineInfo,
  PermissionPrompt,
  RateWindow,
  SessionMeta,
  SessionState,
  StatusLine,
} from "./types";

type Snapshot = {
  machines: MachineInfo[];
  sessions: SessionState[];
};

let machines: MachineInfo[] = [];
const sessions = new Map<string, SessionState>();
let snapshot: Snapshot = { machines, sessions: [] };
const listeners = new Set<() => void>();

const emit = () => {
  snapshot = {
    machines,
    sessions: [...sessions.values()].sort((a, b) =>
      a.machine === b.machine ? a.meta.started_at_ms - b.meta.started_at_ms : a.machine.localeCompare(b.machine),
    ),
  };
  listeners.forEach((l) => l());
};

/** React hook returning the current machines and sessions. */
export const useStore = (): Snapshot =>
  useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => snapshot,
  );

/** Returns a session's display name. */
export const displayName = (s: SessionState): string => {
  if (s.meta.name) return s.meta.name;
  if (s.status?.sessionName) return s.status.sessionName;
  const base = s.meta.cwd.split(/[\\/]/).filter(Boolean).pop() ?? s.meta.cwd;
  return `${base} #${s.meta.id.slice(0, 4)}`;
};

/** Returns the name of machine `id` (the id itself when unknown). */
export const machineName = (list: MachineInfo[], id: string): string => list.find((m) => m.id === id)?.name ?? id;

/** Replaces the machine list. */
export const setMachines = (m: MachineInfo[]): void => {
  machines = m;
  emit();
};

/** Replaces a machine's session list, keeping derived state for known sessions. */
/** Codex fires SessionStart only with its first turn, so it starts out idle. */
const initialActivity = (meta: SessionMeta): Activity => (meta.agent === "codex" ? "idle" : "starting");

export const setSessions = (machine: string, list: SessionMeta[]): void => {
  const seen = new Set<string>();
  for (const meta of list) {
    const key = sessionKey(machine, meta.id);
    seen.add(key);
    const prev = sessions.get(key);
    const exited = meta.status === "exited";
    sessions.set(key, {
      key,
      machine,
      meta,
      activity: exited ? "exited" : prev && prev.activity !== "exited" ? prev.activity : initialActivity(meta),
      prompt: exited ? null : (prev?.prompt ?? null),
      status: prev?.status ?? null,
      account: prev?.account ?? null,
      lastActivityAt: prev?.lastActivityAt ?? meta.started_at_ms,
    });
  }
  for (const [key, s] of sessions) {
    if (s.machine === machine && !seen.has(key)) sessions.delete(key);
  }
  emit();
};

const rateWindow = (v: unknown): RateWindow | null => {
  const used = num(v, "used_percentage");
  return used === null ? null : { usedPercentage: used, resetsAt: num(v, "resets_at") };
};

const parseStatus = (payload: unknown, receivedAt: number): StatusLine => ({
  receivedAt,
  model: str(payload, "model", "display_name") ?? str(payload, "model", "id"),
  sessionName: str(payload, "session_name"),
  contextPercent: num(payload, "context_window", "used_percentage"),
  fiveHour: rateWindow(asRecord(asRecord(payload)?.rate_limits)?.five_hour),
  sevenDay: rateWindow(asRecord(asRecord(payload)?.rate_limits)?.seven_day),
});

const parsePrompt = (v: unknown): PermissionPrompt | null => {
  const id = str(v, "id") ?? "";
  const kind = str(v, "kind") === "dialog" ? "dialog" : "permission";
  const text = str(v, "text");
  const opts = asRecord(v)?.options;
  if (text === null || !Array.isArray(opts)) return null;
  const options = opts.flatMap((o) => {
    const n = num(o, "number");
    const label = str(o, "label");
    return n !== null && label !== null ? [{ number: n, label }] : [];
  });
  return { id, kind, text, options };
};

/** Applies an agent event to the session's derived state. */
export const applyEvent = (machine: string, session: string, ev: AgentEvent): void => {
  const s = sessions.get(sessionKey(machine, session));
  if (!s) return;
  let activity: Activity = s.activity;
  let prompt = s.prompt;
  let status = s.status;
  let account = s.account;
  let meta = s.meta;
  if (ev.source === "statusline") {
    status = parseStatus(ev.payload, ev.received_at_ms);
  } else if (ev.source === "hook") {
    switch (str(ev.payload, "hook_event_name")) {
      case "SessionStart":
        activity = "idle";
        break;
      case "UserPromptSubmit":
        activity = "working";
        break;
      case "Stop":
        activity = "idle";
        break;
      case "Notification":
        if (str(ev.payload, "notification_type") === "permission_prompt") activity = "approval";
        break;
    }
  } else if (ev.source === "cq") {
    const type = str(ev.payload, "type");
    if (type === "permission_prompt") {
      prompt = parsePrompt(asRecord(ev.payload)?.prompt);
      activity = "approval";
    } else if (type === "permission_resolved") {
      prompt = null;
      if (activity === "approval") activity = asRecord(ev.payload)?.idle === true ? "idle" : "working";
    } else if (type === "meta") {
      meta = { ...meta, name: str(ev.payload, "name") };
    } else if (type === "account") {
      const id = str(ev.payload, "account", "id");
      account = id ? { id, label: str(ev.payload, "account", "label") ?? id } : null;
    }
  }
  if (meta.status === "exited") activity = "exited";
  const next = { ...s, meta, activity, prompt, status, account, lastActivityAt: Math.max(s.lastActivityAt, ev.received_at_ms) };
  sessions.set(s.key, next);
  emit();
};

/** Marks a session as exited after the host reports it. */
export const markExited = (machine: string, session: string): void => {
  const s = sessions.get(sessionKey(machine, session));
  if (!s) return;
  sessions.set(s.key, { ...s, activity: "exited", prompt: null, meta: { ...s.meta, status: "exited" } });
  emit();
};

/** Shows a resumed session as running again before the next session list confirms it. */
export const markResumed = (machine: string, session: string): void => {
  const s = sessions.get(sessionKey(machine, session));
  if (!s) return;
  sessions.set(s.key, { ...s, activity: initialActivity(s.meta), meta: { ...s.meta, status: "running", ended_at_ms: null } });
  emit();
};
