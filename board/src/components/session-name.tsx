import { useRef, useState, type SyntheticEvent } from "react";
import { renameSession } from "../api";
import { displayName, useStore } from "../store";
import type { SessionState } from "../types";

const stop = (e: SyntheticEvent) => e.stopPropagation();

/**
 * A session's name; clicking it opens a text box to rename the session (Enter or leaving the box
 * saves, Escape cancels, an empty name clears it). A name another running session on the same
 * machine has is refused in place, as the session's host would refuse it.
 */
export const SessionName = ({ s, className }: { s: SessionState; className: string }) => {
  const { sessions } = useStore();
  const [editing, setEditing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const cancelled = useRef(false);
  if (!editing) {
    return (
      <span
        className={`${className} renamable`}
        title="Click to rename"
        onClick={(e) => {
          e.stopPropagation();
          cancelled.current = false;
          setError(null);
          setEditing(true);
        }}
      >
        {displayName(s)}
      </span>
    );
  }
  const commit = (value: string) => {
    const name = value.trim();
    const taken = sessions.find((o) => o.key !== s.key && o.machine === s.machine && o.activity !== "exited" && o.meta.name === name);
    if (name && taken) return setError(`${taken.meta.id.slice(0, 8)} is already named "${name}"`);
    if (name !== (s.meta.name ?? "")) void renameSession(s.machine, s.meta.id, name).catch((e: unknown) => setError(String(e)));
    setError(null);
    setEditing(false);
  };
  return (
    <input
      className={`session-name-input ${error ? "invalid" : ""}`}
      autoFocus
      defaultValue={s.meta.name ?? ""}
      placeholder={s.meta.id.slice(0, 8)}
      title={error ?? "Enter to save, Escape to cancel; empty clears the name"}
      onClick={stop}
      onPointerDown={stop}
      onFocus={(e) => e.currentTarget.select()}
      onChange={() => setError(null)}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Enter") commit(e.currentTarget.value);
        if (e.key === "Escape") {
          cancelled.current = true;
          setError(null);
          setEditing(false);
        }
      }}
      onBlur={(e) => {
        if (!cancelled.current && !error) commit(e.currentTarget.value);
        else if (!cancelled.current) setEditing(false);
      }}
    />
  );
};
