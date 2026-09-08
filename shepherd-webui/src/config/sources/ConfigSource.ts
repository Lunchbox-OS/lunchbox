/**
 * Where a config document comes from, and where it goes back to.
 *
 * The editor never branches on which implementation it has, which is what lets
 * one component tree serve three homes:
 *
 * - `FileConfigSource` — local files, for the standalone static build.
 * - `DeviceConfigSource` — a shepherd device over `GET`/`PUT /api/v1/config`
 *   (issue #185). It lives in `src/sources/` rather than here, because this
 *   tree may not import `src/api/`: it also builds into the standalone bundle,
 *   which has no daemon to talk to.
 * - `TauriConfigSource` — native file dialogs in a desktop shell.
 */
export interface ConfigDocument {
  /** TOML text. */
  text: string;
  /** Display name for the title bar; null for an unsaved document. */
  name: string | null;
  /** Opaque handle the source may use to save back in place. */
  handle?: unknown;
}

export interface ConfigSource {
  /** Human-readable name for this source, shown in the UI. */
  readonly label: string;
  /** Whether `save` can write back without asking where. */
  canSaveInPlace(doc: ConfigDocument): boolean;
  /** Prompt for and read a document. Resolves null if the user cancels. */
  open(): Promise<ConfigDocument | null>;
  /** Write back to where the document came from. */
  save(doc: ConfigDocument, text: string): Promise<ConfigDocument>;
  /** Write to a new location, prompting for it. */
  saveAs(doc: ConfigDocument, text: string): Promise<ConfigDocument | null>;
}
