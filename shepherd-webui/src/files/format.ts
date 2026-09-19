/** How the columns read (issue #195). */
import type { DirEntryInfo, RootInfo, UnusableReason } from "./types";
import { extensionOf } from "./tree";

/**
 * Sizes in the units a person reads, not the ones a computer stores.
 *
 * Decimal, because that is what the drive was sold as and what every other
 * file manager on the device says.
 */
export function formatBytes(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined) return "";
  if (bytes < 1000) return `${bytes} B`;
  const units = ["kB", "MB", "GB", "TB"];
  let value = bytes / 1000;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

/** `"12 GB free of 61 GB"`, for a root row's size cell. */
export function formatRootSpace(root: RootInfo): string {
  if (root.free_bytes === null || root.total_bytes === null) return "";
  return `${formatBytes(root.free_bytes)} free of ${formatBytes(root.total_bytes)}`;
}

/**
 * Recent dates as a time, this year's as a day, older as a full date — the
 * shape a file list has had since long before this one.
 */
export function formatModified(iso: string | null | undefined): string {
  if (!iso) return "";
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) return "";
  const now = new Date();
  const sameDay = at.toDateString() === now.toDateString();
  if (sameDay) {
    return at.toLocaleTimeString(undefined, {
      hour: "numeric",
      minute: "2-digit",
    });
  }
  if (at.getFullYear() === now.getFullYear()) {
    return at.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  }
  return at.toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

/** The full timestamp, for the tooltip over the short one. */
export function formatModifiedFull(iso: string | null | undefined): string {
  if (!iso) return "";
  const at = new Date(iso);
  return Number.isNaN(at.getTime()) ? "" : at.toLocaleString();
}

/** Extensions worth naming, because a parent puts these on a device on purpose. */
const KINDS: Record<string, string> = {
  epub: "Book",
  pdf: "PDF",
  cbz: "Comic",
  cbr: "Comic",
  mp4: "Video",
  mkv: "Video",
  mov: "Video",
  webm: "Video",
  mp3: "Audio",
  m4a: "Audio",
  flac: "Audio",
  ogg: "Audio",
  png: "Image",
  jpg: "Image",
  jpeg: "Image",
  gif: "Image",
  webp: "Image",
  svg: "Image",
  toml: "Config",
  json: "Config",
  txt: "Text",
  md: "Text",
  log: "Log",
  zip: "Archive",
  gz: "Archive",
  xz: "Archive",
  gba: "Game ROM",
  gb: "Game ROM",
  gbc: "Game ROM",
  nes: "Game ROM",
  sfc: "Game ROM",
  smc: "Game ROM",
  z64: "Game ROM",
  iso: "Disc image",
  chd: "Disc image",
};

export function kindLabel(entry: DirEntryInfo): string {
  if (entry.kind === "dir") return "Folder";
  if (entry.kind === "other") return "Special file";
  const extension = extensionOf(entry.name);
  if (!extension) return "File";
  return KINDS[extension] ?? `${extension.toUpperCase()} file`;
}

export function rootKindLabel(root: RootInfo): string {
  switch (root.kind) {
    case "home":
      return "This device";
    case "external":
      return "Removable drive";
    case "configured":
      return "Place";
  }
}

/** Why a row is greyed out, in words rather than in a code. */
export function unusableLabel(reason: UnusableReason): string {
  switch (reason) {
    case "symlink_escapes":
      return "This shortcut points outside this place, so it cannot be opened here. It can still be deleted.";
    case "name_not_utf8":
      return "This name is not text this device can address, so nothing can be done to it here.";
    case "special_file":
      return "Not an ordinary file — a socket, a pipe or a device.";
    case "not_browsable":
      return "Managed by shepherd itself, or holding credentials. Not editable here.";
    case "unreadable":
      return "This device can see it but cannot read anything about it. It may still be possible to delete.";
  }
}
