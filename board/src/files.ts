import { baseName } from "./path";

const PREFIX = "file:";

/** A file a session showed, as a pane view. */
export type FileRef = { session: string; path: string };

/** Pane view key of a file shown by the session `session` (a session key). */
export const fileView = (session: string, path: string): string => PREFIX + JSON.stringify([session, path]);

/** The file a pane view shows, or null for a session view. */
export const parseFileView = (view: string): FileRef | null => {
  if (!view.startsWith(PREFIX)) return null;
  const [session, path] = JSON.parse(view.slice(PREFIX.length)) as [string, string];
  return { session, path };
};

/** How the board shows a file. */
export type FileKind = "image" | "pdf" | "html" | "markdown" | "text";

const IMAGE_TYPES: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  svg: "image/svg+xml",
  bmp: "image/bmp",
  ico: "image/x-icon",
};

const extension = (path: string): string => baseName(path).split(".").pop()?.toLowerCase() ?? "";

/** The viewer for a file, by its extension; anything unknown is shown as text. */
export const fileKind = (path: string): FileKind => {
  const ext = extension(path);
  if (ext in IMAGE_TYPES) return "image";
  if (ext === "pdf") return "pdf";
  if (ext === "html" || ext === "htm") return "html";
  if (ext === "md" || ext === "markdown") return "markdown";
  return "text";
};

/** MIME type for files shown through a blob URL. */
export const mimeType = (path: string): string => IMAGE_TYPES[extension(path)] ?? (extension(path) === "pdf" ? "application/pdf" : "application/octet-stream");