import { useEffect, useState } from "react";
import { setTokenPrefs, toggleFold, usePrefs, type TokenPeriod } from "../prefs";
import type { MachineInfo, SessionState, TokenCounts } from "../types";
import { Fold, isExpanded } from "./sidebar-section";

const PERIOD_MS: Record<TokenPeriod, number> = { "24h": 24 * 3600_000, "7d": 7 * 24 * 3600_000, all: Infinity };
const AGENT_LABEL: Record<string, string> = { claude: "Claude", codex: "Codex" };

type DirUsage = {
  machine: string;
  machineLabel: string;
  cwd: string;
  counts: TokenCounts;
  total: number;
  sessions: number;
  /** Account labels per agent. */
  accounts: Map<string, Set<string>>;
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
 * Sums each directory's hourly tokens within the period. Sessions resumed from the same agent
 * session report the same hours, so those are merged by keeping the larger reading per hour.
 */
const dirUsage = (sessions: SessionState[], machines: MachineInfo[], period: TokenPeriod, now: number): DirUsage[] => {
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
  const dirs = new Map<string, DirUsage>();
  for (const { s, hours } of byAgentSession.values()) {
    const counts = zero();
    for (const [hour, c] of hours) if (inPeriod(hour, period, now)) add(counts, c);
    if (sum(counts) === 0) continue;
    const key = `${s.machine}\n${s.meta.cwd}`;
    const d = dirs.get(key) ?? {
      machine: s.machine,
      machineLabel: machines.find((m) => m.id === s.machine)?.label ?? s.machine,
      cwd: s.meta.cwd,
      counts: zero(),
      total: 0,
      sessions: 0,
      accounts: new Map<string, Set<string>>(),
    };
    add(d.counts, counts);
    d.total = sum(d.counts);
    d.sessions += 1;
    const account = s.account?.label ?? s.meta.account;
    if (account) d.accounts.set(s.meta.agent, (d.accounts.get(s.meta.agent) ?? new Set()).add(account));
    dirs.set(key, d);
  }
  return [...dirs.values()];
};

const baseName = (cwd: string) => cwd.split(/[\\/]/).filter(Boolean).pop() ?? cwd;

const tooltip = (d: DirUsage) =>
  [
    `${d.machineLabel}: ${d.cwd}`,
    `input: ${d.counts.input.toLocaleString()}`,
    `output: ${d.counts.output.toLocaleString()}`,
    `cache read: ${d.counts.cache_read.toLocaleString()}`,
    `cache write: ${d.counts.cache_creation.toLocaleString()}`,
    `sessions: ${d.sessions}`,
  ].join("\n");

const DirRow = ({ d, label, max }: { d: DirUsage; label: string; max: number }) => (
  <div className="token-row" title={tooltip(d)}>
    <div className="token-row-head">
      <span className="token-row-name">{label}</span>
      <span className="token-row-total">{formatTokens(d.total)}</span>
    </div>
    <span className="meter-track">
      <span className="meter-fill" style={{ width: `${(d.total / max) * 100}%` }} />
    </span>
    {d.accounts.size > 0 && (
      <div className="token-row-accounts">
        {[...d.accounts.entries()]
          .sort(([a], [b]) => a.localeCompare(b))
          .map(([agent, labels]) => (
            <span key={agent}>
              {AGENT_LABEL[agent] ?? agent}: {[...labels].join(", ")}
            </span>
          ))}
      </div>
    )}
  </div>
);

/** Tokens used per machine and directory, as a tree or ranked by usage. */
export const TokenUsage = ({ sessions, machines }: { sessions: SessionState[]; machines: MachineInfo[] }) => {
  const prefs = usePrefs();
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 60_000);
    return () => clearInterval(t);
  }, []);
  const dirs = dirUsage(sessions, machines, prefs.tokenPeriod, now).sort((a, b) => b.total - a.total);
  const max = Math.max(1, ...dirs.map((d) => d.total));
  const byMachine = new Map<string, DirUsage[]>();
  for (const d of [...dirs].sort((a, b) => a.cwd.localeCompare(b.cwd))) byMachine.set(d.machine, [...(byMachine.get(d.machine) ?? []), d]);
  const tree = [...byMachine.values()].sort((a, b) => a[0].machineLabel.localeCompare(b[0].machineLabel));
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
      {dirs.length === 0 && <div className="usage-empty">no token usage in this period</div>}
      {prefs.tokenView === "rank"
        ? dirs.map((d) => <DirRow key={`${d.machine}\n${d.cwd}`} d={d} label={`${d.machineLabel} · ${baseName(d.cwd)}`} max={max} />)
        : tree.map((ds) => {
            const foldKey = `tokens:${ds[0].machine}`;
            const open = isExpanded(prefs, foldKey, true);
            return (
              <div key={ds[0].machine} className="token-machine">
                <button className="token-machine-head" onClick={() => toggleFold(foldKey)}>
                  <Fold open={open} />
                  <span className="token-row-name">{ds[0].machineLabel}</span>
                  <span className="token-row-total">{formatTokens(ds.reduce((n, d) => n + d.total, 0))}</span>
                </button>
                {open && ds.map((d) => <DirRow key={d.cwd} d={d} label={baseName(d.cwd)} max={max} />)}
              </div>
            );
          })}
    </div>
  );
};
