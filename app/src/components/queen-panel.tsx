import { useEffect, useMemo, useRef, useState } from "react";
import {
  deliver,
  getAutoEnter,
  getQueenUsage,
  handleOf,
  markRead,
  recentTexts,
  route,
  setAutoEnter,
  useCards,
  type Card,
  type QueenUsage,
} from "../queen";
import { displayName, useStore } from "../store";
import type { SessionState } from "../types";

type Props = { onOpenSession: (s: SessionState) => void };

type Target = { session: SessionState; via: "mention" | "reply" | "queen" };

const STATE_LABEL: Record<Card["state"], string> = {
  unread: "unread",
  answered: "answered",
  "answered-elsewhere": "answered directly",
  read: "read",
};

const formatK = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n));

const time = (t: number) => new Date(t).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

/** Queen chat: cards from every session, and a composer that routes messages to sessions. */
export const QueenPanel = ({ onOpenSession }: Props) => {
  const cards = useCards();
  const { sessions } = useStore();
  const live = useMemo(() => sessions.filter((s) => s.activity !== "exited"), [sessions]);
  const [text, setText] = useState("");
  const [replyTo, setReplyTo] = useState<Card | null>(null);
  const [choices, setChoices] = useState<SessionState[] | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [autoEnter, setAutoEnterState] = useState(getAutoEnter());
  const [usage, setUsage] = useState<QueenUsage | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const feedRef = useRef<HTMLDivElement>(null);

  const byKey = useMemo(() => new Map(sessions.map((s) => [s.key, s])), [sessions]);
  const handles = useMemo(() => live.map((s) => ({ s, handle: handleOf(s, live, displayName) })), [live]);

  useEffect(() => {
    const el = feedRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [cards.length]);

  useEffect(() => {
    const tick = () => void getQueenUsage().then(setUsage, () => undefined);
    tick();
    const t = setInterval(tick, 5000);
    return () => clearInterval(t);
  }, []);

  /** Resolves the explicit target in priority order: @mention, then the card being replied to. */
  const explicitTarget = (msg: string): { target: Target | null; body: string } => {
    const m = /^@(\S+)\s*([\s\S]*)$/.exec(msg.trim());
    if (m) {
      const hit = handles.find((h) => h.handle === m[1].toLowerCase());
      if (hit) return { target: { session: hit.s, via: "mention" }, body: m[2] };
    }
    if (replyTo) {
      const s = byKey.get(replyTo.key);
      if (s && s.activity !== "exited") return { target: { session: s, via: "reply" }, body: msg };
    }
    return { target: null, body: msg };
  };

  const sendTo = async (s: SessionState, body: string) => {
    setBusy(true);
    try {
      await deliver(s.machine, s.meta.id, body, autoEnter);
      setStatus(`sent to @${handleOf(s, live, displayName)}`);
      setText("");
      setReplyTo(null);
      setChoices(null);
    } catch (e) {
      setStatus(`send failed: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const submit = async () => {
    const msg = text.trim();
    if (!msg || busy) return;
    const { target, body } = explicitTarget(msg);
    if (target) return sendTo(target.session, body);
    if (live.length === 1) return sendTo(live[0], msg);
    if (live.length === 0) return setStatus("no running session");
    setBusy(true);
    setStatus("Queen is choosing a session…");
    try {
      const d = await route(
        msg,
        live.map((s) => ({ key: s.key, name: displayName(s), cwd: s.meta.cwd, recent: recentTexts(s.key) })),
      );
      const target = d.target ? byKey.get(d.target) : undefined;
      if (target) {
        setBusy(false);
        return sendTo(target, msg);
      }
      const cands = (d.candidates.length ? d.candidates : live.map((s) => s.key))
        .map((k) => byKey.get(k))
        .filter((s): s is SessionState => !!s);
      setChoices(cands);
      setStatus("Which session is this for?");
    } catch (e) {
      setChoices(live);
      setStatus(`routing failed (${String(e)}); choose a session`);
    } finally {
      setBusy(false);
    }
  };

  const mention = /^@(\S*)$/.exec(text);
  const suggestions = mention ? handles.filter((h) => h.handle.startsWith(mention[1].toLowerCase())) : [];

  return (
    <div className="queen">
      <header className="queen-header">
        <span className="queen-title">Queen</span>
        {usage && (
          <span className="queen-usage" title="Queen's own claude -p consumption since start (counts against the 5h window)">
            {usage.calls} calls · {formatK(usage.input + usage.output + usage.cache_read + usage.cache_creation)} tok · $
            {usage.cost_usd.toFixed(3)}
          </span>
        )}
        <label className="queen-toggle">
          <input
            type="checkbox"
            checked={autoEnter}
            onChange={(e) => {
              setAutoEnter(e.target.checked);
              setAutoEnterState(e.target.checked);
            }}
          />
          auto Enter
        </label>
      </header>
      <div className="queen-feed" ref={feedRef}>
        {cards.length === 0 && <div className="empty">Agent replies appear here as cards.</div>}
        {cards.map((c) => {
          const s = byKey.get(c.key);
          const long = c.text.length > 400;
          const open = expanded.has(c.id);
          const body = open ? c.text : (c.summary ?? (long ? `${c.text.slice(0, 400)}…` : c.text));
          return (
            <article
              key={c.id}
              className={`card ${c.kind} ${c.state} ${c.stale ? "stale" : ""} ${c.resolved ? "resolved" : ""} ${replyTo?.id === c.id ? "replying" : ""}`}
              onClick={() => markRead(c.id)}
            >
              <header>
                <button className="card-session" onClick={() => s && onOpenSession(s)}>
                  {s ? `@${handleOf(s, live, displayName)}` : c.session}
                </button>
                <span className={`card-kind ${c.kind}`}>{c.kind === "blocking" ? "needs you" : "turn done"}</span>
                {c.stale && <span className="card-flag">outdated</span>}
                <span className="card-state">{STATE_LABEL[c.state]}</span>
                <span className="card-time">{time(c.createdAt)}</span>
              </header>
              <div className="card-body">
                {c.summarizing ? <em>summarizing…</em> : body}
                {c.summary && !open && <span className="card-note"> (summary)</span>}
              </div>
              <footer>
                {(long || c.summary) && (
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      setExpanded((x) => {
                        const n = new Set(x);
                        if (n.has(c.id)) n.delete(c.id);
                        else n.add(c.id);
                        return n;
                      });
                    }}
                  >
                    {open ? "collapse" : "full text"}
                  </button>
                )}
                {s && s.activity !== "exited" && c.kind === "info" && (
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      setReplyTo(c);
                    }}
                  >
                    reply
                  </button>
                )}
              </footer>
            </article>
          );
        })}
      </div>
      {choices && (
        <div className="queen-choices">
          {choices.map((s) => (
            <button key={s.key} disabled={busy} onClick={() => void sendTo(s, text.trim())}>
              @{handleOf(s, live, displayName)} <span>{s.meta.cwd}</span>
            </button>
          ))}
          <button onClick={() => setChoices(null)}>cancel</button>
        </div>
      )}
      <div className="queen-compose">
        {replyTo && (
          <div className="queen-reply">
            replying to @{byKey.get(replyTo.key) ? handleOf(byKey.get(replyTo.key) as SessionState, live, displayName) : replyTo.session}
            {replyTo.stale && <strong> — this card is outdated</strong>}
            <button onClick={() => setReplyTo(null)}>×</button>
          </div>
        )}
        {suggestions.length > 0 && (
          <div className="queen-suggest">
            {suggestions.map((h) => (
              <button key={h.s.key} onClick={() => setText(`@${h.handle} `)}>
                @{h.handle}
              </button>
            ))}
          </div>
        )}
        <textarea
          value={text}
          placeholder="Message a session: @name …, reply to a card, or let Queen route it"
          onChange={(e) => {
            setText(e.target.value);
            setChoices(null);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              void submit();
            }
          }}
          rows={3}
        />
        {status && <div className="queen-status">{status}</div>}
      </div>
    </div>
  );
};
