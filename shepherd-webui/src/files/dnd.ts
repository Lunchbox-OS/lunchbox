/**
 * Reading a drop from the desktop (issue #195).
 *
 * Native drag events, not `@dnd-kit`, and there is no choice about it: only
 * `DataTransfer` carries the files. (Row-to-row moves are the other half, and
 * those *do* go through dnd-kit, for the keyboard and touch support it brings.)
 *
 * Pure and tested, because the interesting case is the one nobody has on their
 * machine while developing: a dropped *folder*, which needs a recursive walk,
 * a `mkdir` per level and a resumption story, and is refused for now with a
 * sentence rather than silently dropping its contents.
 */

export interface DataTransferItemLike {
  kind: string;
  webkitGetAsEntry?: () => { isDirectory: boolean; name: string } | null;
  getAsFile?: () => File | null;
}

export interface DataTransferLike {
  items?: ArrayLike<DataTransferItemLike> | null;
  files?: ArrayLike<File> | null;
  types?: ReadonlyArray<string>;
}

export interface DropPayload {
  files: File[];
  /** Names of dropped directories, which this does not accept yet. */
  directories: string[];
}

/** Whether a drag is carrying files at all, as opposed to text or a row. */
export function isFileDrag(transfer: DataTransferLike | null | undefined): boolean {
  if (!transfer) return false;
  if (transfer.types && transfer.types.length > 0) {
    return transfer.types.includes("Files");
  }
  return Boolean(transfer.files && transfer.files.length > 0);
}

/**
 * Split a drop into what can be sent and what cannot.
 *
 * `webkitGetAsEntry` is the only way to tell a folder from a file before
 * reading it — a directory arrives in `files` as a zero-byte entry with no
 * type, which would upload as an empty file of the same name and look like it
 * worked.
 */
export function readDrop(transfer: DataTransferLike): DropPayload {
  const files: File[] = [];
  const directories: string[] = [];

  const items = transfer.items;
  if (items && items.length > 0 && typeof items[0]?.webkitGetAsEntry === "function") {
    for (let i = 0; i < items.length; i += 1) {
      const item = items[i];
      if (item.kind !== "file") continue;
      const entry = item.webkitGetAsEntry?.() ?? null;
      if (entry?.isDirectory) {
        directories.push(entry.name);
        continue;
      }
      const file = item.getAsFile?.();
      if (file) files.push(file);
    }
    return { files, directories };
  }

  // No entry API (an older browser, or a synthetic event in a test): take the
  // files as given. A folder dropped here would arrive as a zero-byte file,
  // which is worse than refusing it — but this branch is the fallback, not the
  // path a real browser takes.
  const plain = transfer.files;
  if (plain) {
    for (let i = 0; i < plain.length; i += 1) files.push(plain[i]);
  }
  return { files, directories };
}

/** The sentence for a refused folder drop. */
export function directoriesRefused(directories: string[]): string {
  const names = directories.join(", ");
  return directories.length === 1
    ? `Dropping a folder is not supported yet — open ${names} and drop the files inside it.`
    : `Dropping folders is not supported yet — open them and drop the files inside (${names}).`;
}
