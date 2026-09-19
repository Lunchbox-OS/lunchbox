/**
 * What a row will let somebody do (issue #195).
 *
 * Pure, and consulted *before* a control is drawn rather than after it is
 * clicked — which is the whole reason the API reports `writable` per directory
 * and a reason rather than a bare `unusable`. A menu item that 403s is worse
 * than one that is not there.
 *
 * Two permissions are involved and they are easy to confuse:
 *
 * - **Creating** inside a folder needs write permission on *that folder*.
 * - **Renaming or deleting** an entry needs write permission on its *parent*,
 *   not on the entry. A read-only file in a writable folder can still be
 *   deleted, which is why the row carries `parentWritable`.
 */
import type { DirEntryInfo, UnusableReason } from "./types";
import type { Row } from "./tree";

/**
 * Reasons that still leave an entry deletable.
 *
 * The escaping symlink is the case this whole flag exists for: it is listed
 * rather than hidden precisely so that somebody can get rid of it, and
 * deleting it removes the link and not what it points at. A special file goes
 * the same way, and so does an entry nothing could be learned about: not
 * knowing what something *is* says nothing about whether its name still
 * reaches it, and being unable to tidy it up would be the worse answer.
 *
 * The other two cannot be addressed or reached at all.
 */
const DELETABLE_ANYWAY: UnusableReason[] = [
  "symlink_escapes",
  "special_file",
  "unreadable",
];

function addressable(entry: DirEntryInfo): boolean {
  return entry.unusable === undefined || DELETABLE_ANYWAY.includes(entry.unusable);
}

/** Whether files can be uploaded into this row, and folders created in it. */
export function canWriteInto(row: Row): boolean {
  if (row.kind === "root") return row.root.writable;
  if (row.kind !== "entry") return false;
  if (row.entry.kind !== "dir" || row.entry.unusable !== undefined) return false;
  // `writable` is only absent on a row that is not a directory, which the line
  // above has already excluded; treat a missing one as "no" rather than
  // guessing yes and offering a control that fails.
  return row.entry.writable === true;
}

export function canRename(row: Row): boolean {
  return (
    row.kind === "entry" &&
    row.parentWritable &&
    row.entry.unusable === undefined
  );
}

export function canDelete(row: Row): boolean {
  return row.kind === "entry" && row.parentWritable && addressable(row.entry);
}

export function canDownload(row: Row): boolean {
  return (
    row.kind === "entry" &&
    row.entry.kind === "file" &&
    row.entry.unusable === undefined
  );
}

/** Whether a delete needs the "and everything in it" wording and flag. */
export function isFolder(row: Row): boolean {
  return row.kind === "entry" && row.entry.kind === "dir";
}
