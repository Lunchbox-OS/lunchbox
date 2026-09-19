/**
 * What kinds of activity there are, and what a fresh one of each looks like.
 *
 * One list, because there are two places that offer the choice — the "Add an
 * activity" dialog and the type menu in `KindEditor` — and they had drifted:
 * the dialog offered five of the nine and built a media activity with a
 * `library_id` the schema has never had. Records keyed by the generated
 * `RawEntryKind["type"]` rather than arrays, so a kind added to the schema is a
 * compile error here instead of an option nobody can pick.
 */
import type { RawEntryKind } from "./config.generated";

export type KindTag = RawEntryKind["type"];

export const KIND_LABELS: Record<KindTag, string> = {
  process: "Program",
  snap: "Snap",
  steam: "Steam game",
  flatpak: "Flatpak",
  vm: "Virtual machine",
  media: "Media library",
  retroarch: "Emulated game",
  ebook: "Book",
  custom: "Custom",
};

export const KIND_HINTS: Record<KindTag, string> = {
  process: "Runs a command directly.",
  snap: "Launched through snap, with systemd scope-based process management.",
  steam: "Launched through the Steam snap by App ID.",
  flatpak: "Launched through flatpak by application ID.",
  vm: "Handed to a VM driver.",
  media: "Opens a lunchbox-media library.",
  retroarch: "Boots one ROM or disc image through RetroArch.",
  ebook: "Opens one book in a reader locked to reading it.",
  custom: "Passed through to a host adapter that understands the type name.",
};

/** What a kind carries over when an existing activity switches to it. */
interface Carry {
  args?: string[];
  env?: Record<string, string>;
}

/**
 * A kind with its required fields present and empty — what the schema needs to
 * parse, and what the form then fills in. `carry` is for switching an existing
 * activity over: the kinds that take arguments and environment keep them, and
 * the rest have no analogue.
 */
export function blankKind(type: KindTag, carry: Carry = {}): RawEntryKind {
  const runnable = { args: carry.args ?? [], env: carry.env ?? {} };
  switch (type) {
    case "process":
      return { type, command: "", ...runnable };
    case "snap":
      return { type, snap_name: "", ...runnable };
    case "steam":
      return { type, app_id: 0, ...runnable };
    case "flatpak":
      return { type, app_id: "", ...runnable };
    case "vm":
      return { type, driver: "", args: {} };
    case "media":
      return { type, library: "" };
    case "retroarch":
      return { type, content: "", core: "", ...runnable };
    case "ebook":
      return { type, book: "", ...runnable };
    case "custom":
      return { type, type_name: "" };
  }
}
