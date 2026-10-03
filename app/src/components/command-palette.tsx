import { useEffect, useMemo, useRef, useState } from "react";
import type { Command } from "../commands";
import { formatChord } from "../keys";

type Props = {
  commands: Command[];
  keysOf: (name: string) => string[];
  warnings: string[];
  onRun: (command: Command, args: Record<string, string>) => void;
  onClose: () => void;
};

/** Scores `text` against a subsequence query; higher is better, `null` means no match. */
const fuzzyScore = (query: string, text: string): number | null => {
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  if (q === "") return 0;
  const direct = t.indexOf(q);
  if (direct >= 0) return 1000 - direct;
  let score = 0;
  let pos = -1;
  for (const ch of q) {
    const next = t.indexOf(ch, pos + 1);
    if (next < 0) return null;
    score -= next - pos;
    pos = next;
  }
  return score;
};

/** Command palette: fuzzy search over commands, then prompts for their arguments. */
export const CommandPalette = ({ commands, keysOf, warnings, onRun, onClose }: Props) => {
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const [picked, setPicked] = useState<Command | null>(null);
  const [args, setArgs] = useState<Record<string, string>>({});
  const inputRef = useRef<HTMLInputElement>(null);

  const matches = useMemo(
    () =>
      commands
        .map((c) => ({ c, score: Math.max(fuzzyScore(query, c.title) ?? -Infinity, fuzzyScore(query, c.name) ?? -Infinity) }))
        .filter((m) => m.score > -Infinity)
        .sort((a, b) => b.score - a.score || a.c.title.localeCompare(b.c.title))
        .map((m) => m.c),
    [commands, query],
  );

  const argIndex = picked ? Object.keys(args).length : 0;
  const currentArg = picked?.args[argIndex] ?? null;

  useEffect(() => {
    inputRef.current?.focus();
  }, [picked, argIndex]);

  const choose = (c: Command) => {
    if (c.args.length === 0) return onRun(c, {});
    setPicked(c);
    setArgs({});
    setQuery(c.args[0].default ?? "");
  };

  const submitArg = () => {
    if (!picked || !currentArg) return;
    const value = query.trim() === "" ? (currentArg.default ?? "") : query.trim();
    const next = { ...args, [currentArg.name]: value };
    if (Object.keys(next).length === picked.args.length) return onRun(picked, next);
    setArgs(next);
    setQuery(picked.args[Object.keys(next).length].default ?? "");
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.nativeEvent.isComposing) return;
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    } else if (e.key === "Enter") {
      e.preventDefault();
      if (picked) submitArg();
      else if (matches[index]) choose(matches[index]);
    } else if (!picked && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
      e.preventDefault();
      const step = e.key === "ArrowDown" ? 1 : -1;
      setIndex((i) => (matches.length === 0 ? 0 : (i + step + matches.length) % matches.length));
    }
  };

  return (
    <div className="palette-backdrop" onMouseDown={onClose}>
      <div className="palette" onMouseDown={(e) => e.stopPropagation()}>
        {picked && currentArg && (
          <div className="palette-step">
            {picked.title} · {currentArg.title || currentArg.name}
            {currentArg.default ? ` (default: ${currentArg.default})` : ""}
          </div>
        )}
        <input
          ref={inputRef}
          className="palette-input"
          value={query}
          placeholder={picked ? (currentArg?.title ?? currentArg?.name ?? "") : "Type a command"}
          onChange={(e) => {
            setQuery(e.target.value);
            setIndex(0);
          }}
          onKeyDown={onKeyDown}
          spellCheck={false}
        />
        {!picked && (
          <ul className="palette-list">
            {matches.map((c, i) => (
              <li
                key={c.name}
                className={i === index ? "selected" : ""}
                onMouseEnter={() => setIndex(i)}
                onClick={() => choose(c)}
              >
                <span className="palette-title">{c.title}</span>
                <span className="palette-meta">
                  {c.plugin && <span className="palette-plugin">{c.plugin}</span>}
                  {keysOf(c.name).slice(0, 2).map((k) => (
                    <kbd key={k}>{formatChord(k)}</kbd>
                  ))}
                </span>
              </li>
            ))}
            {matches.length === 0 && <li className="palette-empty">No matching commands</li>}
          </ul>
        )}
        {!picked && warnings.length > 0 && (
          <div className="palette-warnings">
            {warnings.map((w) => (
              <div key={w}>{w}</div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
};
