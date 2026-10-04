/** Narrows an unknown value to a plain object. */
export const asRecord = (v: unknown): Record<string, unknown> | null =>
  typeof v === "object" && v !== null && !Array.isArray(v) ? (v as Record<string, unknown>) : null;

/** Reads a nested field by path, returning `undefined` when any step is missing. */
export const pick = (v: unknown, ...path: string[]): unknown => {
  let cur: unknown = v;
  for (const k of path) {
    const r = asRecord(cur);
    if (!r) return undefined;
    cur = r[k];
  }
  return cur;
};

/** Reads a string field, or `null`. */
export const str = (v: unknown, ...path: string[]): string | null => {
  const x = pick(v, ...path);
  return typeof x === "string" ? x : null;
};

/** Reads a finite number field, or `null`. */
export const num = (v: unknown, ...path: string[]): number | null => {
  const x = pick(v, ...path);
  return typeof x === "number" && Number.isFinite(x) ? x : null;
};
