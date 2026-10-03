import { useEffect, useRef } from "react";
import type { IDockviewPanelProps } from "dockview-react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import "@xterm/xterm/css/xterm.css";
import { decodeBytes, registerTerminal, resize, sendInput, sessionKey, setFocus, subscribe, unsubscribe } from "../api";
import { matchShortcut } from "../shortcuts";

export type TerminalParams = { machine: string; session: string };

const THEME = {
  background: "#14161a",
  foreground: "#d8dde6",
  cursor: "#f5c451",
  selectionBackground: "#3a4152",
};

/** A tile showing one session's terminal through the daemon relay. */
export const TerminalPanel = (props: IDockviewPanelProps<TerminalParams>) => {
  const { machine, session } = props.params;
  const hostRef = useRef<HTMLDivElement>(null);
  const panelApi = props.api;

  useEffect(() => {
    const el = hostRef.current;
    if (!el) return;
    const term = new Terminal({
      allowProposedApi: true,
      cursorBlink: true,
      fontFamily: '"Cascadia Mono", "BIZ UDGothic", Consolas, "DejaVu Sans Mono", "Noto Sans Mono CJK JP", monospace',
      fontSize: 13,
      scrollback: 5000,
      theme: THEME,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new Unicode11Addon());
    term.unicode.activeVersion = "11";
    term.open(el);
    fit.fit();

    const send = (data: string) => void sendInput(machine, session, data).catch(() => undefined);
    term.attachCustomKeyEventHandler((e) => {
      if (matchShortcut(e)) return false;
      // Shift+Enter inserts a newline in Claude Code; xterm would otherwise send a plain CR.
      if (e.type === "keydown" && e.key === "Enter" && e.shiftKey && !e.ctrlKey && !e.altKey && !e.isComposing) {
        send("\x1b[13;2u");
        return false;
      }
      return true;
    });
    term.onData(send);
    term.onResize(({ cols, rows }) => void resize(machine, session, cols, rows).catch(() => undefined));
    // Focusing a tile claims the input right so the PTY follows this tile's size, and makes it
    // the desktop's focus (where e.g. voice input goes).
    term.textarea?.addEventListener("focus", () => {
      send("");
      void setFocus(machine, session).catch(() => undefined);
    });

    const off = registerTerminal(sessionKey(machine, session), (msg) => {
      if (msg.kind === "snapshot" && msg.data) {
        term.reset();
        term.write(decodeBytes(msg.data));
      } else if (msg.kind === "output" && msg.data) {
        term.write(decodeBytes(msg.data));
      } else if (msg.kind === "exit") {
        term.write("\r\n\x1b[2m[session exited]\x1b[0m\r\n");
      } else if (msg.kind === "error") {
        // A freshly launched host may not be listening yet; retry a few times.
        if (retries < 5) {
          retries += 1;
          retryTimer = setTimeout(doSubscribe, 1000);
        } else {
          term.write(`\r\n\x1b[31m[${String(msg.json)}]\x1b[0m\r\n`);
        }
      }
    });
    let retries = 0;
    let retryTimer: ReturnType<typeof setTimeout> | undefined;
    const doSubscribe = () =>
      void subscribe(machine, session, term.cols, term.rows).catch((e: unknown) =>
        term.write(`\x1b[31m${String(e)}\x1b[0m\r\n`),
      );
    doSubscribe();

    const ro = new ResizeObserver(() => {
      if (el.clientWidth > 0 && el.clientHeight > 0) fit.fit();
    });
    ro.observe(el);
    const activeSub = panelApi.onDidActiveChange((e) => {
      if (e.isActive) term.focus();
    });
    if (panelApi.isActive) term.focus();

    return () => {
      clearTimeout(retryTimer);
      activeSub.dispose();
      ro.disconnect();
      off();
      void unsubscribe(machine, session).catch(() => undefined);
      term.dispose();
    };
  }, [machine, session, panelApi]);

  return <div className="terminal-host" ref={hostRef} />;
};
