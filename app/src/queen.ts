import { useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import { sendInput, sessionKey } from "./api";
import { asRecord, str } from "./json";
import type { AgentEvent, SessionState } from "./types";

/** Replies longer than this are summarized by Queen; shorter ones are shown verbatim. */
export const SUMMARY_THRESHOLD = 600;

export type CardKind = "blocking" | "info";

/** unread → answered (via Queen) | answered-elsewhere (direct input) | read (acknowledged). */
export type CardState = "unread" | "answered" | "answered-elsewhere" | "read";

export type Card = {
  id: string;
  key: string;
  machine: string;
  session: string;
  kind: CardKind;
  createdAt: number;
  text: string;
  summary: string | null;
  summarizing: boolean;
  state: CardState;
  /** The session has moved on since this card; replying would answer an outdated context. */
  stale: boolean;
  /** Blocking card whose prompt is gone. */
  resolved: boolean;
};

export type QueenUsage = {
  calls: number;
  input: number;
  output: number;
  cache_read: number;
  cache_creation: number;
  cost_usd: number;
};

export type RouteDecision = { target: string | null; candidates: string[] };

let cards: Card[] = [];
const listeners = new Set<() => void>();
const startedAt = Date.now();
/** Last time Queen sent input to a session, to tell Queen replies from direct input. */
const sentByQueenAt = new Map<string, number>();

const emit = () => {
  cards = [...cards];
  listeners.forEach((l) => l());
};

/** React hook returning all cards, oldest first. */
export const useCards = (): Card[] =>
  useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => cards,
  );

const SUMMARY_CACHE = "cq.summary.";

const cachedSummary = (id: string): string | null => {
  try {
    return localStorage.getItem(SUMMARY_CACHE + id);
  } catch {
    return null;
  }
};

const update = (id: string, patch: Partial<Card>) => {
  const i = cards.findIndex((c) => c.id === id);
  if (i < 0) return;
  cards[i] = { ...cards[i], ...patch };
  emit();
};

const summarize = (card: Card, sessionName: string) => {
  update(card.id, { summarizing: true });
  invoke<string>("queen_summarize", { session: sessionName, text: card.text }).then(
    (summary) => {
      try {
        localStorage.setItem(SUMMARY_CACHE + card.id, summary);
      } catch {
        // Cache is optional.
      }
      update(card.id, { summary, summarizing: false });
    },
    () => update(card.id, { summarizing: false }),
  );
};

const addCard = (card: Omit<Card, "summary" | "summarizing" | "stale" | "resolved">, live: boolean, name: string) => {
  if (cards.some((c) => c.id === card.id)) return;
  const full: Card = { ...card, summary: cachedSummary(card.id), summarizing: false, stale: false, resolved: false };
  // Earlier cards of the same session now describe an outdated state.
  for (const c of cards) if (c.key === card.key && c.kind === "info") c.stale = true;
  cards.push(full);
  cards.sort((a, b) => a.createdAt - b.createdAt);
  emit();
  // Only live replies are summarized, so reloading never spends tokens on history.
  if (live && full.summary === null && full.kind === "info" && full.text.length > SUMMARY_THRESHOLD) summarize(full, name);
};

/** Builds and updates cards from session events. */
export const queenOnEvent = (machine: string, session: string, ev: AgentEvent, name: string): void => {
  const key = sessionKey(machine, session);
  const live = ev.received_at_ms >= startedAt;
  if (ev.source === "hook") {
    const hook = str(ev.payload, "hook_event_name");
    if (hook === "Stop") {
      const text = str(ev.payload, "last_assistant_message") ?? "";
      if (!text.trim()) return;
      addCard(
        { id: `${key}:${ev.received_at_ms}`, key, machine, session, kind: "info", createdAt: ev.received_at_ms, text, state: "unread" },
        live,
        name,
      );
    } else if (hook === "UserPromptSubmit") {
      // Any input reaching the session answers its open cards, whichever route it took.
      const viaQueen = (sentByQueenAt.get(key) ?? 0) > ev.received_at_ms - 10_000;
      let changed = false;
      for (const c of cards) {
        if (c.key !== key || c.createdAt > ev.received_at_ms) continue;
        if (c.state === "unread") {
          c.state = viaQueen ? "answered" : "answered-elsewhere";
          changed = true;
        }
        if (!c.stale) {
          c.stale = true;
          changed = true;
        }
      }
      if (changed) emit();
    }
  } else if (ev.source === "cq") {
    const type = str(ev.payload, "type");
    if (type === "permission_prompt") {
      const text = str(asRecord(ev.payload)?.prompt, "text") ?? "Permission requested";
      addCard(
        { id: `${key}:perm:${ev.received_at_ms}`, key, machine, session, kind: "blocking", createdAt: ev.received_at_ms, text, state: "unread" },
        live,
        name,
      );
    } else if (type === "permission_resolved") {
      let changed = false;
      for (const c of cards) {
        if (c.key === key && c.kind === "blocking" && !c.resolved) {
          c.resolved = true;
          if (c.state === "unread") c.state = "answered-elsewhere";
          changed = true;
        }
      }
      if (changed) emit();
    }
  }
};

/** Marks a card as read without answering it; it never blocks anything. */
export const markRead = (id: string): void => {
  const c = cards.find((x) => x.id === id);
  if (c && c.state === "unread") update(id, { state: "read" });
};

/** Returns a session's @handle: its display name slugified, unique among `all`. */
export const handleOf = (s: SessionState, all: SessionState[], name: (s: SessionState) => string): string => {
  const slug = (x: SessionState) =>
    name(x)
      .toLowerCase()
      .replace(/#\w+$/, "")
      .trim()
      .replace(/[^\p{L}\p{N}]+/gu, "-")
      .replace(/^-|-$/g, "") || x.meta.id.slice(0, 6);
  const base = slug(s);
  const dup = all.filter((o) => o.activity !== "exited" && slug(o) === base).length > 1;
  return dup ? `${base}-${s.meta.id.slice(0, 4)}` : base;
};

const AUTO_ENTER_KEY = "cq.queen.autoEnter";

/** Whether Queen presses Enter after pasting a message. */
export const getAutoEnter = (): boolean => {
  try {
    return localStorage.getItem(AUTO_ENTER_KEY) !== "0";
  } catch {
    return true;
  }
};

/** Persists the auto-Enter preference. */
export const setAutoEnter = (v: boolean): void => {
  try {
    localStorage.setItem(AUTO_ENTER_KEY, v ? "1" : "0");
  } catch {
    // Preference only.
  }
};

/** Pastes a message into a session (bracketed paste) and optionally submits it. */
export const deliver = async (machine: string, session: string, text: string, autoEnter: boolean): Promise<void> => {
  sentByQueenAt.set(sessionKey(machine, session), Date.now());
  await sendInput(machine, session, `\x1b[200~${text}\x1b[201~`);
  if (autoEnter) {
    await new Promise((r) => setTimeout(r, 120));
    await sendInput(machine, session, "\r");
  }
};

/** Asks Queen which session a message is for. */
export const route = (
  message: string,
  candidates: { key: string; name: string; cwd: string; recent: string[] }[],
): Promise<RouteDecision> => invoke("queen_route", { message, candidates });

/** Queen's own token consumption. */
export const getQueenUsage = (): Promise<QueenUsage> => invoke("queen_usage");

/** Recent card texts per session for routing context. */
export const recentTexts = (key: string): string[] =>
  cards
    .filter((c) => c.key === key)
    .slice(-3)
    .map((c) => (c.summary ?? c.text).slice(0, 300));
