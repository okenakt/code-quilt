/** Session metadata as persisted by `cq host` (mirrors `cq_protocol::SessionMeta`). */
export type SessionMeta = {
  id: string;
  agent: string;
  cwd: string;
  pipe: string;
  host_pid: number;
  child_pid: number | null;
  started_at_ms: number;
  status: "running" | "exited";
  exit_code: number | null;
  ended_at_ms?: number | null;
  agent_session_id: string | null;
  name: string | null;
  account?: string | null;
  /** Files the agent shared for the user to view, oldest first. */
  files?: SharedFile[];
};

/** A file an agent shared with the user (mirrors `cq_protocol::SharedFile`). */
export type SharedFile = { path: string; caption: string | null; shared_at_ms: number };

/** Agent event relayed from hooks, the status line, or the host itself. */
export type AgentEvent = {
  source: "hook" | "statusline" | "cq" | string;
  received_at_ms: number;
  payload: unknown;
};

export type MachineInfo = {
  id: string;
  name: string;
  online: boolean;
  error: string | null;
};

export type PromptOption = { number: number; label: string };

/** A numbered-choice prompt: a tool permission request or another confirmation dialog. */
export type PermissionPrompt = { id: string; kind: "permission" | "dialog"; text: string; options: PromptOption[] };

/** Coarse session activity derived from events. */
export type Activity = "starting" | "working" | "idle" | "approval" | "exited";

export type RateWindow = { usedPercentage: number; resetsAt: number | null };

/** Fields of interest from Claude Code's status line payload. */
export type StatusLine = {
  receivedAt: number;
  model: string | null;
  sessionName: string | null;
  contextPercent: number | null;
  fiveHour: RateWindow | null;
  sevenDay: RateWindow | null;
};

/** Account the agent is logged in with, reported by the host. */
export type Account = { id: string; label: string };

export type SessionState = {
  key: string;
  machine: string;
  meta: SessionMeta;
  activity: Activity;
  prompt: PermissionPrompt | null;
  status: StatusLine | null;
  account: Account | null;
  lastActivityAt: number;
};

/** Raw `session-data` event payload from the backend. */
export type SessionData = {
  machine: string;
  session: string;
  kind: "snapshot" | "output" | "welcome" | "owner" | "exit" | "error";
  data: string | null;
  json: unknown;
};
