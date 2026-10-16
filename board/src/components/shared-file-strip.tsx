import { useEffect, useState } from "react";
import { FileText } from "lucide-react";
import { readFile } from "../api";
import { fileKind, fileView, mimeType } from "../files";
import { openInActiveTab } from "../layout";
import { markdownDocument } from "../markdown";
import { baseName } from "../path";
import type { SessionState, SharedFile } from "../types";

type Preview = { kind: "image"; url: string } | { kind: "doc"; html: string };

/** A small rendering of an image or a Markdown file. */
const Thumbnail = ({ s, file }: { s: SessionState; file: SharedFile }) => {
  const [preview, setPreview] = useState<Preview | null>(null);
  useEffect(() => {
    let created: string | null = null;
    let cancelled = false;
    readFile(s.machine, s.meta.id, file.path)
      .then(async (buf): Promise<Preview> => {
        if (fileKind(file.path) === "markdown") return { kind: "doc", html: await markdownDocument(new TextDecoder().decode(buf)) };
        created = URL.createObjectURL(new Blob([buf], { type: mimeType(file.path) }));
        return { kind: "image", url: created };
      })
      .then((p) => !cancelled && setPreview(p))
      .catch(() => !cancelled && setPreview(null));
    return () => {
      cancelled = true;
      if (created) URL.revokeObjectURL(created);
    };
  }, [s.machine, s.meta.id, file.path, file.shared_at_ms]);
  if (!preview) return <FileText size={16} />;
  return preview.kind === "image" ? <img src={preview.url} alt={baseName(file.path)} /> : <iframe sandbox="" srcDoc={preview.html} tabIndex={-1} title={file.path} />;
};

/** The files a session showed, right of its terminal, newest at the bottom: images and Markdown as thumbnails, others by name. Selecting one opens it beside the session. */
export const SharedFileStrip = ({ s }: { s: SessionState }) => {
  const files = s.meta.files ?? [];
  if (files.length === 0) return null;
  return (
    <div className="shared-file-strip">
      {files.map((file) => (
        <button key={file.path} title={file.path} onClick={() => openInActiveTab(fileView(s.key, file.path), s.key)}>
          {fileKind(file.path) === "image" || fileKind(file.path) === "markdown" ? (
            <Thumbnail s={s} file={file} />
          ) : (
            <>
              <FileText size={14} />
              <span>{baseName(file.path)}</span>
            </>
          )}
        </button>
      ))}
    </div>
  );
};
