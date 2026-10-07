import { useEffect, useState } from "react";
import type { MachineInfo, RateWindow, SessionState } from "../types";

type AccountUsage = {
  key: string;
  label: string;
  fiveHour: RateWindow | null;
  sevenDay: RateWindow | null;
  updatedAt: number;
  machines: string[];
};

type AgentGroup = { agent: string; accounts: AccountUsage[] };

const AGENT_LABEL: Record<string, string> = { claude: "Claude", codex: "Codex" };

/** An agent and its account(s), as both the Limits and Tokens sections show them. */
export const AgentHeading = ({ agent, account }: { agent: string; account: string }) => (
  <header className="agent-heading">
    <strong>{AGENT_LABEL[agent] ?? agent}</strong>
    <span className="agent-account">{account}</span>
  </header>
);

/** Groups sessions by agent, then by account, keeping the latest rate limit reading of each. */
const agentGroups = (sessions: SessionState[], machines: MachineInfo[]): AgentGroup[] => {
  const groups = new Map<string, Map<string, AccountUsage>>();
  for (const s of sessions) {
    if (!s.status) continue;
    const accounts = groups.get(s.meta.agent) ?? new Map<string, AccountUsage>();
    groups.set(s.meta.agent, accounts);
    const key = s.account?.id ?? "unknown";
    const row = accounts.get(key) ?? {
      key: `${s.meta.agent}:${key}`,
      label: s.account?.label ?? "unknown account",
      fiveHour: null,
      sevenDay: null,
      updatedAt: 0,
      machines: [],
    };
    const machineLabel = machines.find((m) => m.id === s.machine)?.label ?? s.machine;
    if (s.activity !== "exited" && !row.machines.includes(machineLabel)) row.machines.push(machineLabel);
    if (s.status.receivedAt > row.updatedAt && (s.status.fiveHour || s.status.sevenDay)) {
      row.fiveHour = s.status.fiveHour;
      row.sevenDay = s.status.sevenDay;
      row.updatedAt = s.status.receivedAt;
    }
    accounts.set(key, row);
  }
  return [...groups.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([agent, accounts]) => ({ agent, accounts: [...accounts.values()] }));
};

const remaining = (resetsAt: number, now: number): string => {
  const mins = Math.max(0, Math.round((resetsAt * 1000 - now) / 60000));
  if (mins >= 24 * 60) return `${Math.floor(mins / 1440)}d ${Math.floor((mins % 1440) / 60)}h`;
  return mins >= 60 ? `${Math.floor(mins / 60)}h ${mins % 60}m` : `${mins}m`;
};

/** Formats a reset time like `/status`: the clock time today, or the date and time later. */
const resetText = (resetsAt: number, now: number): string => {
  const at = new Date(resetsAt * 1000);
  const sameDay = at.toDateString() === new Date(now).toDateString();
  const clock = at.toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" });
  const when = sameDay ? clock : `${at.toLocaleDateString("en-US", { month: "short", day: "numeric" })} ${clock}`;
  return `Resets ${when} (in ${remaining(resetsAt, now)})`;
};

const ago = (t: number, now: number): string => {
  const mins = Math.round((now - t) / 60000);
  return mins < 1 ? "updated just now" : `updated ${mins}m ago`;
};

const Limit = ({ label, w, now }: { label: string; w: RateWindow | null; now: number }) => {
  const pct = w ? Math.min(100, Math.max(0, w.usedPercentage)) : null;
  const level = pct === null ? "unknown" : pct >= 90 ? "high" : pct >= 70 ? "mid" : "low";
  return (
    <div className={`limit ${level}`}>
      <div className="limit-head">
        <span>{label}</span>
        <span className="limit-value">{pct === null ? "?" : `${Math.round(pct)}% used`}</span>
      </div>
      <span className="meter-track">
        <span className="meter-fill" style={{ width: `${pct ?? 0}%` }} />
      </span>
      {w?.resetsAt ? <div className="limit-reset">{resetText(w.resetsAt, now)}</div> : null}
    </div>
  );
};

/** Rate limits grouped by agent and account, similar to Claude Code's `/status`. */
export const LimitsPanel = ({ sessions, machines }: { sessions: SessionState[]; machines: MachineInfo[] }) => {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);
  const groups = agentGroups(sessions, machines);
  return (
    <div className="limits-panel">
      {groups.length === 0 && <div className="usage-empty">no usage data yet</div>}
      {groups.flatMap((g) =>
        g.accounts.map((a) => (
          <section key={a.key} className="usage-account" title={a.machines.length ? `machines: ${a.machines.join(", ")}` : undefined}>
            <AgentHeading agent={g.agent} account={a.label} />
            <Limit label="5h limit" w={a.fiveHour} now={now} />
            <Limit label="Weekly limit" w={a.sevenDay} now={now} />
            {a.updatedAt > 0 && <div className="usage-updated">{ago(a.updatedAt, now)}</div>}
          </section>
        )),
      )}
    </div>
  );
};
