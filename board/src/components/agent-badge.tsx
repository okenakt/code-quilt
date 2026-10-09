const LABEL: Record<string, string> = { claude: "Claude", codex: "Codex" };

/** An agent (`claude`, `codex`) as a badge hatched in the agent's color. */
export const AgentBadge = ({ agent }: { agent: string }) => <span className={`agent-badge ${agent}`}>{LABEL[agent] ?? agent}</span>;
