/**
 * Calls `onMove` for every pointer move until the pointer is released, then `onUp`. Frames stop
 * taking the pointer meanwhile, or a drag over a file pane would lose its moves to the frame.
 */
export const trackPointer = (onMove: (ev: PointerEvent) => void, onUp?: () => void) => {
  document.body.classList.add("tracking-pointer");
  const up = () => {
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", up);
    document.body.classList.remove("tracking-pointer");
    onUp?.();
  };
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", up);
};
