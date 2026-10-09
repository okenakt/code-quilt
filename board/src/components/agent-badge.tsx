/** An agent (`claude`, `codex`) as a badge hatched in the agent's color. */
export const AgentBadge = ({ agent }: { agent: string }) => <span className={`agent-badge ${agent}`}>{agent}</span>;
