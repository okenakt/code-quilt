import { useEffect, useState } from "react";
import type { MachineInfo, RateWindow, SessionState } from "../types";

type AccountRow = {
  key: string;
  label: string;
  fiveHour: RateWindow | null;
  sevenDay: RateWindow | null;
  updatedAt: number;
  machines: string[];
};

/** Groups sessions by account and keeps the most recent rate limit reading of each. */
const accountRows = (sessions: SessionState[], machines: MachineInfo[]): AccountRow[] => {
  const rows = new Map<string, AccountRow>();
  for (const s of sessions) {
    if (!s.status) continue;
    const key = `${s.meta.agent}:${s.account?.id ?? "unknown"}`;
    const machineLabel = machines.find((m) => m.id === s.machine)?.label ?? s.machine;
    const row = rows.get(key) ?? {
      key,
      label: s.account?.label ?? "unknown account",
      fiveHour: null,
      sevenDay: null,
      updatedAt: 0,
      machines: [],
    };
    if (s.activity !== "exited" && !row.machines.includes(machineLabel)) row.machines.push(machineLabel);
    if (s.status.receivedAt > row.updatedAt && (s.status.fiveHour || s.status.sevenDay)) {
      row.fiveHour = s.status.fiveHour;
      row.sevenDay = s.status.sevenDay;
      row.updatedAt = s.status.receivedAt;
    }
    rows.set(key, row);
  }
  return [...rows.values()];
};

const until = (resetsAt: number | null, now: number): string => {
  if (resetsAt === null) return "";
  const mins = Math.max(0, Math.round((resetsAt * 1000 - now) / 60000));
  if (mins >= 24 * 60) return `${Math.floor(mins / 1440)}d${Math.floor((mins % 1440) / 60)}h`;
  return mins >= 60 ? `${Math.floor(mins / 60)}h${mins % 60}m` : `${mins}m`;
};

const ago = (t: number, now: number): string => {
  const mins = Math.round((now - t) / 60000);
  return mins < 1 ? "just now" : `${mins}m ago`;
};

const Meter = ({ label, w, now }: { label: string; w: RateWindow | null; now: number }) => {
  const pct = w ? Math.min(100, Math.max(0, w.usedPercentage)) : null;
  const level = pct === null ? "unknown" : pct >= 90 ? "high" : pct >= 70 ? "mid" : "low";
  return (
    <div className={`meter ${level}`} title={w?.resetsAt ? `resets in ${until(w.resetsAt, now)}` : undefined}>
      <span className="meter-label">{label}</span>
      <span className="meter-track">
        <span className="meter-fill" style={{ width: `${pct ?? 0}%` }} />
      </span>
      <span className="meter-value">{pct === null ? "?" : `${Math.round(pct)}%`}</span>
      {w?.resetsAt ? <span className="meter-reset">{until(w.resetsAt, now)}</span> : null}
    </div>
  );
};

/** One row per account with 5h and weekly usage, reset times and last update. */
export const UsageBar = ({ sessions, machines }: { sessions: SessionState[]; machines: MachineInfo[] }) => {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);
  const rows = accountRows(sessions, machines);
  if (rows.length === 0) return <div className="usage-bar empty">no usage data yet</div>;
  return (
    <div className="usage-bar">
      {rows.map((r) => (
        <div key={r.key} className="account-row">
          <span className="account-label" title={r.machines.join(", ")}>
            {r.label}
          </span>
          <Meter label="5h" w={r.fiveHour} now={now} />
          <Meter label="week" w={r.sevenDay} now={now} />
          <span className="account-updated">{r.updatedAt ? ago(r.updatedAt, now) : ""}</span>
        </div>
      ))}
    </div>
  );
};
