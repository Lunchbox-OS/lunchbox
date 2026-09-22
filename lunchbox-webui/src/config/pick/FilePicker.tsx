/**
 * Choosing a path that exists on the device (issue #186).
 *
 * The editor's second seam, and the same shape as `ConfigSource` for the same
 * reason: browsing a device's files means `src/api/`, react-query and axios,
 * and `scripts/check-boundary.mjs` forbids all three in here because this tree
 * also builds into the standalone bundle. So what lives here is the
 * *interface*, and the implementation is out in `src/sources/`, importing it
 * inwards.
 *
 * ## Why standalone deliberately gets nothing
 *
 * The File System Access API is right there — `FileConfigSource` already uses
 * it to open a config. Pointed at `content` or `book` it would pick a path on
 * the *browsing* computer, which names nothing on the device the policy is
 * for. A wrong path that arrived through a file dialog looks far more
 * authoritative than one somebody typed, so no picker is the honest answer and
 * the field stays the text field it has always been.
 *
 * Which is also the rule for every consumer: a picker is an *adornment*.
 * `library` also accepts a YouTube playlist URL and `icon` also accepts a theme
 * name, and typing has to keep working everywhere regardless.
 */
import { createContext, useContext } from "react";

/** What counts as an answer. */
export type PathKind =
  | "file"
  | "directory"
  /**
   * Either. RetroArch's `content` is the case: a few cores load a directory
   * rather than a file, which is why the device's own check is `exists` rather
   * than `is_file` (`crates/lunchbox-host-linux/src/retroarch.rs`).
   */
  | "either";

export interface PickRequest {
  kind: PathKind;
  /** What is being chosen, for the dialog's title: "Choose a ROM…". */
  what: string;
  /**
   * Where the field points now, so the dialog can open somewhere useful
   * rather than at the roots. Absent, empty, or a value that names nothing on
   * this device — a URL, a theme name, a path on another machine — is not an
   * error; it just means starting from the top.
   */
  start?: string;
  /**
   * Start with dotfiles shown. True where the thing being chosen normally
   * lives in one: `~/.config/retroarch/cores`, `~/.config/lunchbox/movies.toml`.
   */
  showHidden?: boolean;
}

export interface FilePicker {
  /**
   * Ask for a path.
   *
   * Resolves to the string to write into the config — already in the spelling
   * the schema wants, `~/` or absolute — or null if the person cancelled.
   */
  pick(request: PickRequest): Promise<string | null>;
}

const FilePickerContext = createContext<FilePicker | null>(null);

export function FilePickerProvider({
  picker,
  children,
}: {
  picker: FilePicker | null;
  children: React.ReactNode;
}) {
  return (
    <FilePickerContext.Provider value={picker}>
      {children}
    </FilePickerContext.Provider>
  );
}

/**
 * The picker, or null where there is none.
 *
 * Null is the ordinary case, not an error: the standalone bundle has no
 * device, and an embedded one whose `service.file_manager.enabled` is false
 * has no file routes either. Every caller has to render without it.
 */
export function useFilePicker(): FilePicker | null {
  return useContext(FilePickerContext);
}
