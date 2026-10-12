import { useEffect, useState } from "react";
import { FileText } from "lucide-react";
import { readFile } from "../api";
import { fileKind, fileView, mimeType } from "../files";
import { openInActiveTab } from "../layout";
import { baseName } from "../path";
import type { SessionState, SharedFile } from "../types";

const Thumbnail = ({ s, file }: { s: SessionState; file: SharedFile }) => {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let created: string | null = null;
    let cancelled = false;
    readFile(s.machine, s.meta.id, file.path)
      .then((buf) => {
        if (cancelled) return;
        created = URL.createObjectURL(new Blob([buf], { type: mimeType(file.path) }));
        setUrl(created);
      })
      .catch(() => !cancelled && setUrl(null));
    return () => {
      cancelled = true;
      if (created) URL.revokeObjectURL(created);
    };
  }, [s.machine, s.meta.id, file.path, file.shared_at_ms]);
  return url ? <img src={url} alt={baseName(file.path)} /> : <FileText size={16} />;
};

/** The files a session shared, right of its terminal, newest at the bottom: images as thumbnails, others by name. Selecting one opens it beside the session. */
export const SharedFileStrip = ({ s }: { s: SessionState }) => {
  const files = s.meta.files ?? [];
  if (files.length === 0) return null;
  return (
    <div className="shared-file-strip">
      {files.map((file) => (
        <button key={file.path} title={file.caption ? `${file.caption}\n${file.path}` : file.path} onClick={() => openInActiveTab(fileView(s.key, file.path), s.key)}>
          {fileKind(file.path) === "image" ? (
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
