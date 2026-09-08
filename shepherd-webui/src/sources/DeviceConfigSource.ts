/**
 * This device's own config, over the management API (issue #185).
 *
 * The `ConfigSource` the editor was designed around from the start and could
 * not have until #156: writing a config is an arbitrary-command primitive
 * (`kind = { type = "process", command = … }` runs whatever it is given), and
 * before the login existed the API shipped open on an unclaimed device.
 *
 * ## Why this file is not in `src/config/sources/`
 *
 * `scripts/check-boundary.mjs` forbids anything under `src/config/` from
 * importing `src/api/`, because that tree also builds into the standalone
 * static editor, which has no daemon to talk to. So the daemon-coupled source
 * lives out here and imports the *interface* inwards, which is the direction
 * that stays honest in both bundles.
 *
 * ## Concurrency
 *
 * A device's policy has three writers — `sudoedit`,
 * `shepherd install policy`, and this — so every read carries an `ETag` and
 * every write sends it back. A 412 becomes a message naming what happened
 * rather than a generic failure, because the recovery is specific: re-read,
 * and redo the edit against what is actually there.
 */
import { ApiError, getDeviceConfig, putDeviceConfig } from "../api/client";
import type {
  ConfigDocument,
  ConfigSource,
} from "../config/sources/ConfigSource";

/** What the editor shows in its title bar for a device-hosted document. */
const DOCUMENT_NAME = "config.toml (this device)";

export class DeviceConfigSource implements ConfigSource {
  readonly label = "This device";

  /** Always: there is exactly one place a device config can go. */
  canSaveInPlace(): boolean {
    return true;
  }

  async open(): Promise<ConfigDocument | null> {
    const { text, etag } = await getDeviceConfig();
    return { text, name: DOCUMENT_NAME, handle: etag };
  }

  async save(doc: ConfigDocument, text: string): Promise<ConfigDocument> {
    const etag = typeof doc.handle === "string" ? doc.handle : null;
    try {
      const saved = await putDeviceConfig(text, etag);
      return { text: saved.text, name: DOCUMENT_NAME, handle: saved.etag };
    } catch (e) {
      throw new Error(describe(e));
    }
  }

  /**
   * There is no "somewhere else" on a device.
   *
   * The editor only reaches this when a caller asks for it explicitly, and
   * `canSaveInPlace` is always true, so in practice nothing does. Saving in
   * place is the honest answer to "save as" here.
   */
  async saveAs(doc: ConfigDocument, text: string): Promise<ConfigDocument> {
    return this.save(doc, text);
  }
}

/** Turn a failed write into something a person can act on. */
function describe(e: unknown): string {
  if (e instanceof ApiError) {
    switch (e.status) {
      case 412:
        return (
          "The config on the device changed while this was open — someone " +
          "edited it over SSH, or from another browser. Reload to see what " +
          "is there now; this copy has not been saved."
        );
      case 422:
        // The wasm validator agreed to this text and the daemon did not, which
        // means the two disagree — worth showing verbatim rather than
        // paraphrasing.
        return `The device rejected this config: ${e.message}`;
      case 428:
        return "The device asked for a version tag this editor did not send.";
      default:
        return e.message;
    }
  }
  return String(e);
}
