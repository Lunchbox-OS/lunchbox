/**
 * The config editor's file picker, backed by this device (issue #186).
 *
 * Out here for the reason `DeviceConfigSource` is: browsing files means
 * `src/api/` and react-query, and `scripts/check-boundary.mjs` forbids both
 * inside `src/config/`, which also builds into the standalone bundle. So the
 * editor holds the `FilePicker` *interface* and this supplies one.
 *
 * A render prop rather than a provider wrapping the editor, because the two
 * halves live on opposite sides of that boundary: the promise the editor
 * awaits is resolved by a dialog that only this file may import.
 *
 * ## Why the picker can be null
 *
 * `service.file_manager.enabled = false` does not answer 403 — the routes are
 * not mounted at all. Asking for the roots once, when the editor opens, is
 * what turns that into "no browse button" rather than a button that opens a
 * dialog with an error in it. It is also the request the dialog needs anyway,
 * so on a device that has a file manager it is not an extra round trip.
 */
import { useCallback, useMemo, useRef, useState } from "react";
import type { FilePicker, PickRequest } from "../config/pick/FilePicker";
import { FilePickerDialog } from "../files/FilePickerDialog";
import { useFileRoots } from "../files/useDirectories";

export function DeviceFilePicker({
  children,
}: {
  children: (picker: FilePicker | null) => React.ReactNode;
}) {
  const roots = useFileRoots();
  const [request, setRequest] = useState<PickRequest | null>(null);
  // The half of the promise the dialog completes. A ref because the pump that
  // settles it runs from an event handler, where a stale closure would drop
  // somebody's answer on the floor.
  const pending = useRef<((value: string | null) => void) | null>(null);

  const picker = useMemo<FilePicker>(
    () => ({
      pick: (next) =>
        new Promise((resolve) => {
          // Two fields asking at once should not leave the first one waiting
          // for an answer that can never arrive.
          pending.current?.(null);
          pending.current = resolve;
          setRequest(next);
        }),
    }),
    [],
  );

  const settle = useCallback((value: string | null) => {
    setRequest(null);
    const resolve = pending.current;
    pending.current = null;
    resolve?.(value);
  }, []);

  return (
    <>
      {children(roots.isSuccess ? picker : null)}
      {request && (
        <FilePickerDialog
          request={request}
          onCancel={() => settle(null)}
          onChoose={settle}
        />
      )}
    </>
  );
}
