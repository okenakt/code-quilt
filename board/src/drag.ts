/** Calls `onMove` for every pointer move until the pointer is released. */
export const trackPointer = (onMove: (ev: PointerEvent) => void) => {
  const up = () => {
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", up);
  };
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", up);
};
