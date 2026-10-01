import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AgentEvent, MachineInfo, SessionData, SessionMeta } from "./types";

/** Builds the store key of a session. */
export const sessionKey = (machine: string, session: string): string => `${machine}/${session}`;

/** Returns all machines and their connection state. */
export const getMachines = (): Promise<MachineInfo[]> => invoke("machines");

export type Replay = {
  sessions: { machine: string; sessions: SessionMeta[] }[];
  events: { machine: string; session: string; event: AgentEvent }[];
};

/** Returns sessions and events the backend received before this page loaded. */
export const getReplay = (): Promise<Replay> => invoke("replay");

/** Asks a machine's daemon to resend its session list. */
export const refreshSessions = (machine: string): Promise<void> => invoke("list_sessions", { machine });

/** Opens a terminal relay for a session. */
export const subscribe = (machine: string, session: string, cols: number, rows: number): Promise<void> =>
  invoke("subscribe", { machine, session, cols, rows });

/** Closes a terminal relay. */
export const unsubscribe = (machine: string, session: string): Promise<void> =>
  invoke("unsubscribe", { machine, session });

/** Sends terminal input (UTF-8 text, including escape sequences). */
export const sendInput = (machine: string, session: string, data: string): Promise<void> =>
  invoke("send_input", { machine, session, data });

/** Reports a terminal tile size. */
export const resize = (machine: string, session: string, cols: number, rows: number): Promise<void> =>
  invoke("resize", { machine, session, cols, rows });

/** Answers a permission prompt with a 1-based option number. */
export const approve = (machine: string, session: string, option: number): Promise<void> =>
  invoke("approve", { machine, session, option });

export type DirList = { path: string; parent: string | null; entries: string[]; roots: string[] };

/** Lists subdirectories on a machine (an empty path lists the allowed roots). */
export const listDir = (machine: string, path: string): Promise<DirList> => invoke("list_dir", { machine, path });

/** Starts a session on a machine without a local terminal; resolves to its id. */
export const launchSession = (machine: string, cwd: string, args: string[], resume: string | null): Promise<string> =>
  invoke("launch", { machine, cwd, args, resume });

type TerminalSink = (msg: SessionData) => void;

const terminalSinks = new Map<string, TerminalSink>();

/** Registers the terminal tile that receives a session's output; returns an unregister function. */
export const registerTerminal = (key: string, sink: TerminalSink): (() => void) => {
  terminalSinks.set(key, sink);
  return () => {
    if (terminalSinks.get(key) === sink) terminalSinks.delete(key);
  };
};

type Handlers = {
  machines: (m: MachineInfo[]) => void;
  sessions: (machine: string, sessions: SessionMeta[]) => void;
  event: (machine: string, session: string, ev: AgentEvent) => void;
  data: (msg: SessionData) => void;
};

/** Subscribes to all backend events once; terminal data is routed to registered tiles. */
export const connectBackend = async (h: Handlers): Promise<() => void> => {
  const offs = await Promise.all([
    listen<MachineInfo[]>("machines", (e) => h.machines(e.payload)),
    listen<{ machine: string; sessions: SessionMeta[] }>("sessions", (e) =>
      h.sessions(e.payload.machine, e.payload.sessions),
    ),
    listen<{ machine: string; session: string; event: AgentEvent }>("session-event", (e) =>
      h.event(e.payload.machine, e.payload.session, e.payload.event),
    ),
    listen<SessionData>("session-data", (e) => {
      const msg = e.payload;
      terminalSinks.get(sessionKey(msg.machine, msg.session))?.(msg);
      h.data(msg);
    }),
  ]);
  return () => offs.forEach((off) => off());
};

/** Decodes base64 terminal bytes. */
export const decodeBytes = (b64: string): Uint8Array => {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
};
