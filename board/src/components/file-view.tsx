import { useEffect, useState } from "react";
import { readFile } from "../api";
import { fileKind, mimeType } from "../files";
import { markdownDocument } from "../markdown";

type Props = { machine: string; session: string; path: string; /** Changes when the file is shown again, to reload it. */ version: number };

type Content = { kind: "loading" } | { kind: "error"; message: string } | { kind: "url"; url: string } | { kind: "doc"; html: string; scripts: boolean } | { kind: "text"; text: string };

const decode = (buf: ArrayBuffer): string | null => {
  const text = new TextDecoder().decode(buf);
  return text.includes("\u0000") ? null : text;
};

/** A file a session showed: images and PDFs through blob URLs, HTML and Markdown in sandboxed frames, anything else as text. */
export const FileView = ({ machine, session, path, version }: Props) => {
  const [content, setContent] = useState<Content>({ kind: "loading" });
  useEffect(() => {
    let url: string | null = null;
    let cancelled = false;
    setContent({ kind: "loading" });
    readFile(machine, session, path)
      .then(async (buf) => {
        const kind = fileKind(path);
        let next: Content;
        if (kind === "image" || kind === "pdf") {
          url = URL.createObjectURL(new Blob([buf], { type: mimeType(path) }));
          next = { kind: "url", url };
        } else {
          const text = decode(buf);
          if (text === null) next = { kind: "error", message: "This file is not text and cannot be shown." };
          else if (kind === "html") next = { kind: "doc", html: text, scripts: true };
          else if (kind === "markdown") next = { kind: "doc", html: await markdownDocument(text), scripts: false };
          else next = { kind: "text", text };
        }
        if (!cancelled) setContent(next);
      })
      .catch((e: unknown) => !cancelled && setContent({ kind: "error", message: String(e) }));
    return () => {
      cancelled = true;
      if (url) URL.revokeObjectURL(url);
    };
  }, [machine, session, path, version]);

  switch (content.kind) {
    case "loading":
      return <div className="file-view muted">Loading…</div>;
    case "error":
      return <div className="file-view file-error">{content.message}</div>;
    case "url":
      return fileKind(path) === "pdf" ? (
        <iframe className="file-frame" src={content.url} title={path} />
      ) : (
        <div className="file-view file-image">
          <img src={content.url} alt={path} />
        </div>
      );
    case "doc":
      // Scripts may run in a shared HTML page, but never with the board's origin.
      return <iframe className={`file-frame ${content.scripts ? "page" : ""}`} sandbox={content.scripts ? "allow-scripts" : ""} srcDoc={content.html} title={path} />;
    case "text":
      return <pre className="file-view file-text">{content.text}</pre>;
  }
};
