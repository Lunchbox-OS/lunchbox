/**
 * The file manager's wire types (issue #195).
 *
 * Hand-written rather than generated, like the policy document's, because
 * these routes are not RPCs: `#[management_rpc]` carries every async trait
 * method to BLE, and a file transfer there would be a method that exists and
 * cannot work. The Rust side is `crates/shepherd-http/src/files/mod.rs`.
 */

/** Where a root came from, which decides its icon and its grouping. */
export type RootKind = "home" | "external" | "configured";

export interface RootInfo {
  /** Opaque. Every other call takes this and never an absolute path. */
  id: string;
  label: string;
  kind: RootKind;
  /** Display only — the API does not accept it back. */
  path: string;
  writable: boolean;
  total_bytes: number | null;
  free_bytes: number | null;
}

export interface FileLimits {
  /** 0 means no cap. */
  max_upload_bytes: number;
  /** 0 disables the check. */
  free_space_floor_bytes: number;
}

export interface RootsResponse {
  roots: RootInfo[];
  limits: FileLimits;
}

export type EntryKind = "file" | "dir" | "other";

/**
 * Why an entry is listed but cannot be operated on.
 *
 * The distinction matters to the UI: `symlink_escapes` can still be deleted —
 * which is why it is listed rather than hidden — while `name_not_utf8` cannot
 * be addressed at all, so every action is off.
 */
export type UnusableReason =
  | "symlink_escapes"
  | "name_not_utf8"
  | "special_file"
  | "not_browsable"
  | "unreadable";

export interface DirEntryInfo {
  name: string;
  kind: EntryKind;
  size: number | null;
  /** RFC 3339 with the device's offset. */
  modified: string | null;
  /** `"<size>-<mtime_nanos>"`. Opaque; compared only for equality. */
  etag: string | null;
  hidden: boolean;
  symlink: boolean;
  /** Absent when the entry is fine. */
  unusable?: UnusableReason;
  /**
   * How to name this entry when `name` cannot be typed.
   *
   * Only ever present on `name_not_utf8` rows, where `name` is a lossy
   * rendering that addresses nothing. Opaque: pass it back, never build one.
   */
  handle?: string;
  /** Directories only: whether things can be created inside. */
  writable?: boolean;
}

export interface Listing {
  root: string;
  path: string;
  /**
   * Whether the listed directory itself can be written to — which is what
   * delete and rename need, since both are permissions on the parent.
   */
  writable: boolean;
  entries: DirEntryInfo[];
  truncated: boolean;
  cursor: string | null;
  /**
   * Entries the device could see but could not name — absent when there were
   * none, which is nearly always.
   *
   * They cannot be rows, having no name to show, so the count is how the list
   * says it is shorter than the folder instead of quietly being so.
   */
  unreadable?: number;
}
