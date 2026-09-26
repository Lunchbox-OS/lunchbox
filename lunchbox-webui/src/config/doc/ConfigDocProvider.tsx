/**
 * The editor's single source of truth — which lives in Rust, not here.
 *
 * React holds the wasm handle, the latest projection and a version counter;
 * it never keeps its own copy of the config. That is what makes comment
 * preservation structural rather than aspirational: there is no model to
 * re-serialize from, so nothing can be lost in a round-trip.
 */
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import init, { ConfigDoc, versions as wasmVersions } from "../wasm/lunchbox_config";
import type { RawConfig } from "../model/config.generated";
import type { AvailabilityView } from "../model/availability";
import type { Report } from "../model/report";
import type { Versions } from "../model/wasm-types.generated";
import type { Patch } from "./patches";
import type { ConfigDocument, ConfigSource } from "../sources/ConfigSource";

/** How long to wait after the last edit before re-projecting and validating. */
const DEBOUNCE_MS = 120;

/**
 * The readable half of a thrown value.
 *
 * A source raises an `Error` whose message is already written for a person —
 * `DeviceConfigSource` turns a 412 into a sentence about someone else having
 * edited the file — and `${e}` would prefix all of them with "Error:".
 */
function messageOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/**
 * Everything but what changes on every keystroke.
 *
 * The forms draw from `view` and `report`, which are debounced, so an edit
 * alone changes nothing here. That is what lets a keystroke re-render only the
 * field being typed in rather than the whole editor (issue #235); what does
 * change per edit is in `ConfigDocLive`, for the few controls that show it.
 */
interface ConfigDocContextValue {
  /** Null until the wasm module has loaded. */
  ready: boolean;
  loadError: string | null;
  /**
   * Which build this is. Null until the module has loaded, because it comes
   * from the module rather than from the bundle around it — which is the
   * point: the two are deployed together, so a stale one is stale in both
   * halves and says so.
   */
  versions: Versions | null;

  /** `RawConfig` projection. Null before the first successful parse. */
  view: RawConfig | null;
  /** Validation result, refreshed on the same debounce as `view`. */
  report: Report | null;

  /** Metadata about where this document came from. */
  document: ConfigDocument;

  apply: (patch: Patch, coalesceKey?: string) => void;
  endGesture: () => void;
  replaceText: (text: string) => string | null;

  undo: () => void;
  redo: () => void;

  /**
   * Per-day availability spans, computed by the same code the daemon uses.
   * Changes identity when `view` does, so it agrees with the windows drawn
   * from the view beside it.
   */
  availabilityFor: (
    subject: { kind: "entry" | "group"; id: string },
  ) => AvailabilityView | null;

  openFrom: (source: ConfigSource) => Promise<void>;
  /** Resolves true when the document actually reached the source. */
  save: (source: ConfigSource, as?: boolean) => Promise<boolean>;
  startBlank: () => void;
  /** The most recent failure from a patch or a file operation. */
  error: string | null;
  clearError: () => void;
}

/** What changes on every accepted edit. Read it only where it is shown. */
interface ConfigDocLive {
  /** The live document text — exactly what would be saved. */
  text: string;
  /** True when the text differs from what was last opened or saved. */
  dirty: boolean;
  canUndo: boolean;
  canRedo: boolean;
}

const ConfigDocContext = createContext<ConfigDocContextValue | null>(null);
const ConfigDocLiveContext = createContext<ConfigDocLive | null>(null);

export function useConfigDoc(): ConfigDocContextValue {
  const ctx = useContext(ConfigDocContext);
  if (!ctx) throw new Error("useConfigDoc must be used inside ConfigDocProvider");
  return ctx;
}

/**
 * The per-edit half of the document. A component that reads this re-renders
 * on every keystroke anywhere in the editor, so keep such components small.
 */
export function useConfigDocLive(): ConfigDocLive {
  const ctx = useContext(ConfigDocLiveContext);
  if (!ctx) throw new Error("useConfigDocLive must be used inside ConfigDocProvider");
  return ctx;
}

export function ConfigDocProvider({ children }: { children: ReactNode }) {
  const docRef = useRef<ConfigDoc | null>(null);
  const [ready, setReady] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);

  // Bumped on every accepted mutation. Everything downstream keys off it
  // rather than off the document, which React cannot see into.
  const [version, setVersion] = useState(0);

  const [view, setView] = useState<RawConfig | null>(null);
  const [report, setReport] = useState<Report | null>(null);
  const [text, setText] = useState("");
  const [versions, setVersions] = useState<Versions | null>(null);
  const [document, setDocument] = useState<ConfigDocument>({ text: "", name: null });
  const [error, setError] = useState<string | null>(null);

  // Every accepted mutation goes through here. The text is read in the same
  // update as the version bump, not in an effect keyed on it: an effect that
  // sets state costs a second render of the whole editor after every
  // keystroke, and leaves an update queued behind each one that fast typing
  // keeps starving (issue #235).
  const changed = useCallback((doc: ConfigDoc) => {
    setVersion((v) => v + 1);
    setText(doc.text());
  }, []);

  // Load the wasm module, then start from a blank document so the editor is
  // usable before anyone opens a file.
  useEffect(() => {
    let cancelled = false;
    init()
      .then(() => {
        if (cancelled) return;
        docRef.current = ConfigDoc.blank();
        setVersions(JSON.parse(wasmVersions()) as Versions);
        setReady(true);
        changed(docRef.current);
      })
      .catch((e: unknown) => {
        if (!cancelled) setLoadError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [changed]);

  // Text is kept current with every edit (see `changed`) so the raw pane and
  // the save button never lag behind one; the projection and validation are
  // debounced, since they are the expensive half and only feed rendering.
  useEffect(() => {
    const doc = docRef.current;
    if (!doc) return;
    const timer = setTimeout(() => {
      try {
        setView(JSON.parse(doc.view()) as RawConfig);
      } catch {
        // The document no longer fits the schema's shape. Keep the last good
        // projection on screen; the report below says what is wrong.
      }
      setReport(JSON.parse(doc.validate()) as Report);
    }, DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [version]);

  const apply = useCallback((patch: Patch, coalesceKey?: string) => {
    const doc = docRef.current;
    if (!doc) return;
    try {
      if (doc.apply(JSON.stringify(patch), coalesceKey ?? null)) changed(doc);
    } catch (e) {
      setError(String(e));
    }
  }, [changed]);

  const endGesture = useCallback(() => {
    docRef.current?.endGesture();
  }, []);

  const replaceText = useCallback((next: string): string | null => {
    const doc = docRef.current;
    if (!doc) return null;
    try {
      if (doc.replaceText(next)) changed(doc);
      return null;
    } catch (e) {
      // Returned rather than thrown: the raw pane shows this inline while the
      // user is mid-edit, which is not an error state worth a dialog.
      return String(e);
    }
  }, [changed]);

  const undo = useCallback(() => {
    const doc = docRef.current;
    if (doc?.undo()) changed(doc);
  }, [changed]);

  const redo = useCallback(() => {
    const doc = docRef.current;
    if (doc?.redo()) changed(doc);
  }, [changed]);

  const availabilityFor = useCallback(
    (subject: { kind: "entry" | "group"; id: string }): AvailabilityView | null => {
      const doc = docRef.current;
      if (!doc) return null;
      try {
        const json =
          subject.kind === "entry"
            ? doc.availabilityForEntry(subject.id)
            : doc.availabilityForGroup(subject.id);
        return JSON.parse(json) as AvailabilityView;
      } catch {
        return null;
      }
    },
    // Reads the document through the ref, so nothing here names `view`; it is
    // what the identity should follow, all the same.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [view],
  );

  const openFrom = useCallback(async (source: ConfigSource) => {
    try {
      const opened = await source.open();
      if (!opened) return;
      docRef.current = ConfigDoc.open(opened.text);
      setDocument(opened);
      changed(docRef.current);
      setError(null);
    } catch (e) {
      setError(`Could not open that config: ${messageOf(e)}`);
    }
  }, [changed]);

  const save = useCallback(
    async (source: ConfigSource, as = false): Promise<boolean> => {
      const doc = docRef.current;
      if (!doc) return false;
      const current = doc.text();
      try {
        const saved =
          as || !source.canSaveInPlace(document)
            ? await source.saveAs(document, current)
            : await source.save(document, current);
        // Null means the user dismissed a picker, which is neither a success
        // nor an error — the caller must not tell them it was saved.
        if (!saved) return false;
        setDocument(saved);
        return true;
      } catch (e) {
        setError(`Could not save: ${messageOf(e)}`);
        return false;
      }
    },
    [document],
  );

  const startBlank = useCallback(() => {
    docRef.current = ConfigDoc.blank();
    setDocument({ text: "", name: null });
    changed(docRef.current);
  }, [changed]);

  const value = useMemo<ConfigDocContextValue>(
    () => ({
      ready,
      loadError,
      versions,
      view,
      report,
      document,
      apply,
      endGesture,
      replaceText,
      undo,
      redo,
      availabilityFor,
      openFrom,
      save,
      startBlank,
      error,
      clearError: () => setError(null),
    }),
    [
      ready, loadError, versions, view, report, document, error,
      apply, endGesture, replaceText, undo, redo, availabilityFor, openFrom, save, startBlank,
    ],
  );

  const live = useMemo<ConfigDocLive>(
    () => ({
      text,
      dirty: text !== document.text,
      canUndo: docRef.current?.canUndo() ?? false,
      canRedo: docRef.current?.canRedo() ?? false,
    }),
    // `version` is in the deps because canUndo/canRedo read through the ref,
    // which React cannot observe on its own.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [text, document, version],
  );

  return (
    <ConfigDocContext.Provider value={value}>
      <ConfigDocLiveContext.Provider value={live}>{children}</ConfigDocLiveContext.Provider>
    </ConfigDocContext.Provider>
  );
}
