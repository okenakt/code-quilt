/** The last component of a path (Windows or POSIX). */
export const baseName = (path: string): string => path.split(/[\\/]/).filter(Boolean).pop() ?? path;
