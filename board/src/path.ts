/** The last component of a directory path (Windows or POSIX). */
export const baseName = (path: string): string => path.split(/[\\/]/).filter(Boolean).pop() ?? path;
