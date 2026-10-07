import { Copy, type LucideIcon } from "lucide-react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";

/** A CSS cursor drawn from a lucide icon, white with a dark outline like system cursors. */
const iconCursor = (Icon: LucideIcon, fallback: string): string => {
  const el = document.createElement("div");
  const root = createRoot(el);
  flushSync(() => root.render(<Icon />));
  const paths = el.querySelector("svg")?.innerHTML ?? "";
  root.unmount();
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke-linecap="round" stroke-linejoin="round">` +
    `<g stroke="#000" stroke-width="4">${paths}</g><g stroke="#fff" stroke-width="2">${paths}</g></svg>`;
  return `url("data:image/svg+xml,${encodeURIComponent(svg)}") 10 10, ${fallback}`;
};

/** Defines the cursors used by styles.css as CSS variables (e.g. `--cursor-copy`). */
export const installCursors = () => {
  document.documentElement.style.setProperty("--cursor-copy", iconCursor(Copy, "copy"));
};
