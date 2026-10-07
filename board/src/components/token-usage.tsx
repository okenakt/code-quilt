import { useEffect, useState } from "react";
import { setTokenPrefs, toggleFold, usePrefs, type TokenPeriod } from "../prefs";
import type { MachineInfo, SessionState, TokenCounts } from "../types";
import { Fold, isExpanded } from "./sidebar-section";

const PERIOD_MS: Record<TokenPeriod, number> = { "24h": 24 * 3600_000, "7d": 7 * 24 * 3600_000, all: Infinity };
const AGENT_LABEL: Record<string, string> = { claude: "Claude", codex: "Codex" };

/** Tokens one agent used in one directory of one machine. */
type Usage = {
  machine: string;
  machineLabel: string;
  cwd: string;
  agent: string;
  counts: TokenCounts;
  total: number;
  sessions: number;
  accounts: Set<string>;
};

const zero = (): TokenCounts => ({ input: 0, output: 0, cache_read: 0, cache_creation: 0 });
const sum = (c: TokenCounts) => c.input + c.output + c.cache_read + c.cache_creation;
const add = (a: TokenCounts, b: TokenCounts) => {
  a.input += b.input;
  a.output += b.output;
  a.cache_read += b.cache_read;
  a.cache_creation += b.cache_creation;
};

const formatTokens = (n: number): string =>
  n >= 1e9 ? `${(n / 1e9).toFixed(1)}B` : n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${(n / 1e3).toFixed(1)}k` : String(n);

/** Whether the UTC hour `YYYY-MM-DDTHH` overlaps the last `period`. */
const inPeriod = (hour: string, period: TokenPeriod, now: number) => {
  if (period === "all") return true;
  const start = Date.parse(`${hour}:00:00Z`);
  return !Number.isNaN(start) && start + 3600_000 > now - PERIOD_MS[period];
};

/**
 * Sums the hourly tokens within the period per machine, directory and agent. Sessions resumed from
 * the same agent session report the same hours, so those are merged by keeping the larger reading.
 */
const usages = (sessions: SessionState[], machines: MachineInfo[], period: TokenPeriod, now: number): Usage[] => {
  const byAgentSession = new Map<string, { s: SessionState; hours: Map<string, TokenCounts> }>();
  for (const s of sessions) {
    const key = `${s.machine}\n${s.meta.agent_session_id ?? s.meta.id}`;
    const entry = byAgentSession.get(key) ?? { s, hours: new Map<string, TokenCounts>() };
    for (const [hour, c] of Object.entries(s.meta.tokens_by_hour ?? {})) {
      const prev = entry.hours.get(hour);
      if (!prev || sum(c) > sum(prev)) entry.hours.set(hour, c);
    }
    if (s.meta.started_at_ms >= entry.s.meta.started_at_ms) entry.s = s;
    byAgentSession.set(key, entry);
  }
  const out = new Map<string, Usage>();
  for (const { s, hours } of byAgentSession.values()) {
    const counts = zero();
    for (const [hour, c] of hours) if (inPeriod(hour, period, now)) add(counts, c);
    if (sum(counts) === 0) continue;
    const key = `${s.machine}\n${s.meta.cwd}\n${s.meta.agent}`;
    const u = out.get(key) ?? {
      machine: s.machine,
      machineLabel: machines.find((m) => m.id === s.machine)?.label ?? s.machine,
      cwd: s.meta.cwd,
      agent: s.meta.agent,
      counts: zero(),
      total: 0,
      sessions: 0,
      accounts: new Set<string>(),
    };
    add(u.counts, counts);
    u.total = sum(u.counts);
    u.sessions += 1;
    const account = s.account?.label ?? s.meta.account;
    if (account) u.accounts.add(account);
    out.set(key, u);
  }
  return [...out.values()];
};

const baseName = (cwd: string) => cwd.split(/[\\/]/).filter(Boolean).pop() ?? cwd;

/** Groups items by a key, keeping the order of first appearance. */
const groupBy = <T,>(items: T[], key: (item: T) => string): T[][] => {
  const groups = new Map<string, T[]>();
  for (const item of items) groups.set(key(item), [...(groups.get(key(item)) ?? []), item]);
  return [...groups.values()];
};

const tooltip = (u: Usage) =>
  [
    `${u.machineLabel}: ${u.cwd}`,
    `input: ${u.counts.input.toLocaleString()}`,
    `output: ${u.counts.output.toLocaleString()}`,
    `cache read: ${u.counts.cache_read.toLocaleString()}`,
    `cache write: ${u.counts.cache_creation.toLocaleString()}`,
    `sessions: ${u.sessions}`,
  ].join("\n");

/** One agent's usage: the agent and its accounts, then a bar with the total. */
const AgentUsage = ({ u, max }: { u: Usage; max: number }) => (
  <div className="token-agent" title={tooltip(u)}>
    <div className="token-agent-label">
      {AGENT_LABEL[u.agent] ?? u.agent}: {u.accounts.size > 0 ? [...u.accounts].join(", ") : "unknown account"}
    </div>
    <div className="token-bar">
      <span className="meter-track">
        <span className="meter-fill" style={{ width: `${(u.total / max) * 100}%` }} />
      </span>
      <span className="token-total">{formatTokens(u.total)}</span>
    </div>
  </div>
);

/** Tokens used per machine, directory and agent, as a tree or ranked by usage. */
export const TokenUsage = ({ sessions, machines }: { sessions: SessionState[]; machines: MachineInfo[] }) => {
  const prefs = usePrefs();
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 60_000);
    return () => clearInterval(t);
  }, []);
  const all = usages(sessions, machines, prefs.tokenPeriod, now);
  const max = Math.max(1, ...all.map((u) => u.total));
  const byName = [...all].sort((a, b) => a.machineLabel.localeCompare(b.machineLabel) || a.cwd.localeCompare(b.cwd) || a.agent.localeCompare(b.agent));
  return (
    <div className="token-usage">
      <div className="token-controls">
        <span className="segmented">
          {(["tree", "rank"] as const).map((v) => (
            <button key={v} className={prefs.tokenView === v ? "on" : ""} onClick={() => setTokenPrefs({ tokenView: v })}>
              {v === "tree" ? "Tree" : "By usage"}
            </button>
          ))}
        </span>
        <select value={prefs.tokenPeriod} onChange={(e) => setTokenPrefs({ tokenPeriod: e.target.value as TokenPeriod })}>
          <option value="24h">24h</option>
          <option value="7d">7d</option>
          <option value="all">all</option>
        </select>
      </div>
      {all.length === 0 && <div className="usage-empty">no token usage in this period</div>}
      {prefs.tokenView === "rank"
        ? [...all]
            .sort((a, b) => b.total - a.total)
            .map((u) => (
              <div key={`${u.machine}\n${u.cwd}\n${u.agent}`} className="token-entry">
                <div className="token-entry-place" title={u.cwd}>
                  {u.machineLabel} {baseName(u.cwd)}
                </div>
                <AgentUsage u={u} max={max} />
              </div>
            ))
        : groupBy(byName, (u) => u.machine).map((ofMachine) => {
            const foldKey = `tokens:${ofMachine[0].machine}`;
            const open = isExpanded(prefs, foldKey, true);
            return (
              <div key={ofMachine[0].machine} className="token-machine">
                <button className="token-machine-head" onClick={() => toggleFold(foldKey)}>
                  <Fold open={open} />
                  <span className="token-name">{ofMachine[0].machineLabel}</span>
                  <span className="token-total">{formatTokens(ofMachine.reduce((n, u) => n + u.total, 0))}</span>
                </button>
                {open &&
                  groupBy(ofMachine, (u) => u.cwd).map((ofDir) => (
                    <div key={ofDir[0].cwd} className="token-dir">
                      <div className="token-name" title={ofDir[0].cwd}>
                        {baseName(ofDir[0].cwd)}
                      </div>
                      <div className="token-agents">
                        {ofDir.map((u) => (
                          <AgentUsage key={u.agent} u={u} max={max} />
                        ))}
                      </div>
                    </div>
                  ))}
              </div>
            );
          })}
    </div>
  );
};
