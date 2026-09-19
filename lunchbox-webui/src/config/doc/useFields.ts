/**
 * Field-level patching for a table in the document.
 *
 * Every form control in the editor goes through this rather than building
 * paths by hand, so the "write null means remove the key" rule is applied in
 * one place. Removing rather than writing `null` matters: TOML has no null, and
 * an absent key is what "inherit the default" means throughout the schema.
 */
import { useCallback, useMemo } from "react";
import { useConfigDoc } from "./ConfigDocProvider";
import { dragKey, insert, set, unset, type Json } from "./patches";

export interface Fields {
  /** Write a value, or remove the key when the value is null/undefined/"". */
  setField: (name: string, value: Json | undefined) => void;
  /** Write a value continuously during a gesture; one undo step for the lot. */
  dragField: (name: string, value: Json) => void;
  /** End the current drag gesture. */
  commit: () => void;
  /** Remove a key outright. */
  unsetField: (name: string) => void;
  /** Replace a whole sub-table. */
  setTable: (name: string, value: Json) => void;
  /** Append to an array under this table. */
  push: (name: string, value: Json) => void;
  /** Path of a field under this table, for callers that need it. */
  pathOf: (name: string) => string;
}

export function useFields(basePath: string): Fields {
  const { apply, endGesture } = useConfigDoc();

  const pathOf = useCallback(
    (name: string) => (name ? `${basePath}.${name}` : basePath),
    [basePath],
  );

  return useMemo<Fields>(
    () => ({
      pathOf,
      setField: (name, value) => {
        const path = pathOf(name);
        if (value === undefined || value === null || value === "") apply(unset(path));
        else apply(set(path, value));
      },
      dragField: (name, value) => {
        const path = pathOf(name);
        apply(set(path, value), dragKey(path));
      },
      commit: endGesture,
      unsetField: (name) => apply(unset(pathOf(name))),
      setTable: (name, value) => apply(set(pathOf(name), value)),
      push: (name, value) => apply(insert(pathOf(name), value)),
    }),
    [apply, endGesture, pathOf],
  );
}
