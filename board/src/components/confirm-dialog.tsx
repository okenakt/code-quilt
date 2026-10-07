import { useEffect, useRef, useSyncExternalStore } from "react";

type Request = { title: string; message: string; confirmLabel: string; resolve: (ok: boolean) => void };

let current: Request | null = null;
const listeners = new Set<() => void>();
const set = (next: Request | null) => {
  current = next;
  listeners.forEach((l) => l());
};

/** Asks the user to confirm in a centered modal; resolves to whether they confirmed. */
export const askConfirm = (title: string, message: string, confirmLabel: string): Promise<boolean> =>
  new Promise((resolve) => {
    current?.resolve(false);
    set({ title, message, confirmLabel, resolve });
  });

/** The modal for `askConfirm`; mount once. */
export const ConfirmDialog = () => {
  const req = useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => current,
  );
  const okRef = useRef<HTMLButtonElement>(null);
  useEffect(() => okRef.current?.focus(), [req]);
  if (!req) return null;
  const answer = (ok: boolean) => {
    set(null);
    req.resolve(ok);
  };
  return (
    <div className="modal-backdrop" onClick={() => answer(false)}>
      <div
        className="modal confirm"
        role="alertdialog"
        aria-label={req.title}
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => e.key === "Escape" && answer(false)}
      >
        <header className="modal-title">{req.title}</header>
        <p className="confirm-message">{req.message}</p>
        <div className="modal-actions">
          <button onClick={() => answer(false)}>Cancel</button>
          <button ref={okRef} className="danger" onClick={() => answer(true)}>
            {req.confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
};
