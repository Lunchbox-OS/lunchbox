/**
 * The upload queue (issue #195).
 *
 * Lives above the page so a transfer survives a tab switch: a parent who
 * starts a 2 GB video and then goes to look at today's usage should come back
 * to a progress bar rather than to nothing.
 *
 * Two at a time. The bottleneck is one local disk and one daemon, so more
 * parallelism buys nothing and makes every progress bar less honest.
 */
import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useRef,
  useState,
} from "react";
import { ApiError } from "../api/client";
import { type Precondition, uploadFile } from "../api/files";
import { describe, useFilesRefresh } from "./useDirectories";
import { joinPath, nodeKey, type NodeKey } from "./tree";
import type { FileLimits, RootInfo } from "./types";

export type TransferStatus =
  | "queued"
  | "sending"
  | "conflict"
  | "done"
  | "error"
  | "cancelled";

export interface Transfer {
  id: string;
  rootId: string;
  /** The directory it is going into. */
  dir: string;
  name: string;
  file: File;
  status: TransferStatus;
  sent: number;
  total: number;
  error?: string;
}

interface StartOptions {
  rootId: string;
  dir: string;
  files: File[];
  /** The place being written to, for the size and free-space checks. */
  root?: RootInfo;
  limits?: FileLimits;
}

export interface Uploads {
  transfers: Transfer[];
  /** Queue files for a directory. Returns what could not even be attempted. */
  start: (options: StartOptions) => string[];
  cancel: (id: string) => void;
  /** Answer a `conflict` by replacing what is there. */
  replace: (id: string) => void;
  /** Drop a finished or abandoned row from the tray. */
  dismiss: (id: string) => void;
  clearFinished: () => void;
}

const UploadsContext = createContext<Uploads | null>(null);

const CONCURRENCY = 2;

export function UploadsProvider({ children }: { children: React.ReactNode }) {
  const [transfers, setTransfers] = useState<Transfer[]>([]);
  const refresh = useFilesRefresh();
  // Kept in refs rather than state: the pump reads them between awaits, where
  // a stale closure would either stall the queue or start a cancelled upload.
  const queue = useRef<Map<string, { transfer: Transfer; force: boolean }>>(new Map());
  const aborts = useRef<Map<string, AbortController>>(new Map());
  const running = useRef(0);

  const update = useCallback((id: string, patch: Partial<Transfer>) => {
    setTransfers((list) =>
      list.map((t) => (t.id === id ? { ...t, ...patch } : t)),
    );
  }, []);

  const pump = useCallback(() => {
    while (running.current < CONCURRENCY) {
      const next = [...queue.current.entries()].find(
        ([, item]) => item.transfer.status === "queued",
      );
      if (!next) return;
      const [id, item] = next;
      queue.current.set(id, { ...item, transfer: { ...item.transfer, status: "sending" } });
      running.current += 1;
      update(id, { status: "sending", sent: 0 });
      void send(id, item.transfer, item.force);
    }

    async function send(id: string, transfer: Transfer, force: boolean) {
      const controller = new AbortController();
      aborts.current.set(id, controller);
      // First attempt creates rather than replaces: an upload that silently
      // overwrote a book somebody else put there would be the one thing this
      // feature must not do. A `force` retry is somebody answering the
      // conflict.
      const precondition: Precondition = force ? { kind: "force" } : { kind: "create" };
      try {
        await uploadFile(
          transfer.rootId,
          joinPath(transfer.dir, transfer.name),
          transfer.file,
          precondition,
          {
            signal: controller.signal,
            onProgress: (sent) => update(id, { sent }),
          },
        );
        update(id, { status: "done", sent: transfer.total });
        void refresh.directory(nodeKey(transfer.rootId, transfer.dir));
        // The root's free space moved, and it is on screen.
        void refresh.roots();
      } catch (error) {
        if (controller.signal.aborted) {
          update(id, { status: "cancelled" });
        } else if (error instanceof ApiError && error.code === "precondition_failed") {
          // Something is already there. Not an error yet — a question.
          update(id, { status: "conflict", error: "Something is already there." });
        } else {
          update(id, { status: "error", error: describe(error) });
        }
      } finally {
        aborts.current.delete(id);
        queue.current.delete(id);
        running.current -= 1;
        pump();
      }
    }
  }, [refresh, update]);

  const start = useCallback(
    ({ rootId, dir, files, root, limits }: StartOptions): string[] => {
      const refused: string[] = [];
      const accepted: Transfer[] = [];
      // Checked here rather than by the device, so a 12 GB file against an
      // 8 GiB cap fails instantly instead of after twenty minutes of upload.
      let free = root?.free_bytes ?? null;
      for (const file of files) {
        if (limits && limits.max_upload_bytes > 0 && file.size > limits.max_upload_bytes) {
          refused.push(
            `${file.name} is larger than this device accepts (${format(limits.max_upload_bytes)}).`,
          );
          continue;
        }
        if (
          limits &&
          limits.free_space_floor_bytes > 0 &&
          free !== null &&
          free - file.size < limits.free_space_floor_bytes
        ) {
          refused.push(`${file.name} would not leave enough room on this device.`);
          continue;
        }
        if (free !== null) free -= file.size;
        accepted.push({
          id: `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
          rootId,
          dir,
          name: file.name,
          file,
          status: "queued",
          sent: 0,
          total: file.size,
        });
      }
      if (accepted.length > 0) {
        setTransfers((list) => [...list, ...accepted]);
        for (const transfer of accepted) {
          queue.current.set(transfer.id, { transfer, force: false });
        }
        pump();
      }
      return refused;
    },
    [pump],
  );

  const cancel = useCallback((id: string) => {
    const controller = aborts.current.get(id);
    if (controller) controller.abort();
    // A transfer still waiting its turn never reaches the sender, so it has to
    // be taken off the queue here.
    queue.current.delete(id);
    update(id, { status: "cancelled" });
  }, [update]);

  const replace = useCallback(
    (id: string) => {
      setTransfers((list) => {
        const transfer = list.find((t) => t.id === id);
        if (transfer) {
          queue.current.set(id, {
            transfer: { ...transfer, status: "queued", sent: 0 },
            force: true,
          });
        }
        return list.map((t) =>
          t.id === id ? { ...t, status: "queued", sent: 0, error: undefined } : t,
        );
      });
      pump();
    },
    [pump],
  );

  const dismiss = useCallback((id: string) => {
    queue.current.delete(id);
    setTransfers((list) => list.filter((t) => t.id !== id));
  }, []);

  const clearFinished = useCallback(() => {
    setTransfers((list) =>
      list.filter((t) => t.status === "queued" || t.status === "sending" || t.status === "conflict"),
    );
  }, []);

  const value = useMemo(
    () => ({ transfers, start, cancel, replace, dismiss, clearFinished }),
    [transfers, start, cancel, replace, dismiss, clearFinished],
  );

  return <UploadsContext.Provider value={value}>{children}</UploadsContext.Provider>;
}

export function useUploads(): Uploads {
  const uploads = useContext(UploadsContext);
  if (!uploads) {
    throw new Error("useUploads outside an UploadsProvider");
  }
  return uploads;
}

/** Local, because `format.ts` is about rows and this is about a message. */
function format(bytes: number): string {
  const gib = bytes / (1024 * 1024 * 1024);
  return gib >= 1 ? `${gib.toFixed(1)} GiB` : `${Math.round(bytes / (1024 * 1024))} MiB`;
}

/** Which directories currently have something arriving, for a row spinner. */
export function busyDirectories(transfers: Transfer[]): Set<NodeKey> {
  const busy = new Set<NodeKey>();
  for (const transfer of transfers) {
    if (transfer.status === "sending" || transfer.status === "queued") {
      busy.add(nodeKey(transfer.rootId, transfer.dir));
    }
  }
  return busy;
}
