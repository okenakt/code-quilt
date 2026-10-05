import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import "@xterm/xterm/css/xterm.css";
import { decodeBytes, registerTerminal, resize, sendInput, sessionKey, setFocus, subscribe, unsubscribe } from "../api";
import { isAppChord, terminalFont, useCommandState } from "../commands";

const THEME = {
  background: "#14161a",
  foreground: "#d8dde6",
  cursor: "#f5c451",
  selectionBackground: "#3a4152",
};

type Props = {
  machine: string;
  session: string;
  focused: boolean;
  /** The terminal got the keyboard focus. */
  onFocus: () => void;
  /** The session no longer exists or has ended. */
  onGone: () => void;
};

/** One session's terminal through the daemon relay. */
export const TerminalView = ({ machine, session, focused, onFocus, onGone }: Props) => {
  const hostRef = useRef<HTMLDivElement>(null);
  const callbacks = useRef({ onFocus, onGone });
  callbacks.current = { onFocus, onGone };
  const termRef = useRef<{ term: Terminal; fit: FitAddon } | null>(null);
  const font = terminalFont(useCommandState().config);
  const fontRef = useRef(font);
  fontRef.current = font;

  useEffect(() => {
    const el = hostRef.current;
    if (!el) return;
    const term = new Terminal({
      allowProposedApi: true,
      cursorBlink: true,
      fontFamily: fontRef.current.family,
      fontSize: fontRef.current.size,
      scrollback: 5000,
      theme: THEME,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new Unicode11Addon());
    term.unicode.activeVersion = "11";
    term.open(el);
    fit.fit();
    termRef.current = { term, fit };

    const send = (data: string) => void sendInput(machine, session, data).catch(() => undefined);
    term.attachCustomKeyEventHandler((e) => {
      if (isAppChord(e)) return false;
      // Shift+Enter inserts a newline in Claude Code. xterm would otherwise send a plain CR, also
      // from the keypress that follows the keydown, so both are kept from it.
      if (e.key === "Enter" && e.shiftKey && !e.ctrlKey && !e.altKey && !e.isComposing) {
        if (e.type === "keydown") send("\x1b[13;2u");
        return false;
      }
      return true;
    });
    term.onData(send);
    term.onResize(({ cols, rows }) => void resize(machine, session, cols, rows).catch(() => undefined));
    // Focusing a terminal claims the input right so the PTY follows this pane's size, and makes
    // it the desktop's focus (where e.g. voice input goes).
    term.textarea?.addEventListener("focus", () => {
      send("");
      void setFocus(machine, session).catch(() => undefined);
      callbacks.current.onFocus();
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
        // A pane restored from the saved layout may point at a session that has ended.
        if (msg.json === "unknown session" || msg.json === "session exited") callbacks.current.onGone();
        else term.write(`\r\n\x1b[31m[${String(msg.json)}]\x1b[0m\r\n`);
      }
    });
    void subscribe(machine, session, term.cols, term.rows).catch((e: unknown) =>
      term.write(`\x1b[31m${String(e)}\x1b[0m\r\n`),
    );

    const ro = new ResizeObserver(() => {
      if (el.clientWidth > 0 && el.clientHeight > 0) fit.fit();
    });
    ro.observe(el);
    return () => {
      ro.disconnect();
      off();
      void unsubscribe(machine, session).catch(() => undefined);
      termRef.current = null;
      term.dispose();
    };
  }, [machine, session]);

  useEffect(() => {
    if (focused) termRef.current?.term.focus();
  }, [focused]);

  // Apply font changes from ui.toml without recreating the terminal.
  useEffect(() => {
    const t = termRef.current;
    if (!t) return;
    if (t.term.options.fontFamily === font.family && t.term.options.fontSize === font.size) return;
    t.term.options.fontFamily = font.family;
    t.term.options.fontSize = font.size;
    t.fit.fit();
  }, [font.family, font.size]);

  return <div className="terminal-host" ref={hostRef} />;
};
