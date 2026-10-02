/** Session metadata as persisted by `cq host` (mirrors `cq_proto::SessionMeta`). */
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
  agent_session_id: string | null;
  name: string | null;
  tags: string[];
};

/** Agent event relayed from hooks, the status line, or the host itself. */
export type AgentEvent = {
  source: "hook" | "statusline" | "cq" | string;
  received_at_ms: number;
  payload: unknown;
};

export type MachineInfo = {
  id: string;
  label: string;
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
  inputTokens: number | null;
  outputTokens: number | null;
  costUsd: number | null;
  fiveHour: RateWindow | null;
  sevenDay: RateWindow | null;
};

/** Account the agent is logged in with, reported by the host. */
export type Account = { id: string; label: string };

/** Token totals aggregated from the transcript, per model. */
export type TokenUsage = {
  input: number;
  output: number;
  cacheRead: number;
  cacheCreation: number;
  byModel: Record<string, { input: number; output: number; cacheRead: number; cacheCreation: number }>;
};

export type SessionState = {
  key: string;
  machine: string;
  meta: SessionMeta;
  activity: Activity;
  prompt: PermissionPrompt | null;
  status: StatusLine | null;
  account: Account | null;
  tokens: TokenUsage | null;
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
