import { baseName } from "../path";

/** A working directory as its last component, with the full path on hover. */
export const DirName = ({ path }: { path: string }) => <span title={path}>{baseName(path)}</span>;
