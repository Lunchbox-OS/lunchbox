/**
 * The upload queue (issue #195).
 *
 * Lives above the page so a transfer survives a tab switch: a parent who
 * starts a 2 GB video and then goes to look at today's usage should come back
 * to a progress bar rather than to nothing.
 *
 * Two at a time. The bottleneck is one local disk and one daemon, so more
 * parallelism buys nothing and makes every progress bar less honest.
 *
 * ## Surviving a bad link
 *
 * These devices are often repurposed hardware with the wifi chip they came
 * with, so a transfer is expected to be interrupted rather than merely
 * unlucky. Three things follow, and each is load-bearing:
 *
 * - **Anything past one chunk goes up in pieces**, each its own request
 *   against the resumable routes. The unit of retry is 8 MiB, not the file.
 * - **A resumed transfer asks the device where it got to** rather than
 *   starting again — including after the page was closed, because the token
 *   is derived from the file rather than generated.
 * - **A stalled request is treated as a failed one.** A connection that stops
 *   moving without closing is the common wifi failure, and waiting fifteen
 *   minutes for TCP to notice is not something to put in front of a person.
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
import {
  CHUNK_BYTES,
  type Precondition,
  abandonUpload,
  uploadChunk,
  uploadFile,
  uploadOffset,
  uploadToken,
} from "../api/files";
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
  /** Identifies the part file on the device, so a retry resumes it. */
  token: string;
  status: TransferStatus;
  sent: number;
  total: number;
  /** Which attempt is in flight, for a tray that says "retrying". */
  attempt: number;
  /** Where this attempt picked up, when it picked up rather than started. */
  resumedFrom?: number;
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
  /** Try a failed transfer again, resuming whatever the device already holds. */
  retry: (id: string) => void;
  /** Drop a finished or abandoned row from the tray. */
  dismiss: (id: string) => void;
  clearFinished: () => void;
}

const UploadsContext = createContext<Uploads | null>(null);

const CONCURRENCY = 2;

/** Attempts at one chunk before the transfer is handed back to the person. */
const ATTEMPTS = 3;

/** A request that has not moved a byte in this long is not going to. */
const STALL_MS = 30_000;

/** Backoff before attempt 2 and attempt 3. */
const BACKOFF_MS = [1_000, 4_000];

export function UploadsProvider({ children }: { children: React.ReactNode }) {
  const [transfers, setTransfers] = useState<Transfer[]>([]);
  const refresh = useFilesRefresh();
  // Kept in refs rather than state: the pump reads them between awaits, where
  // a stale closure would either stall the queue or start a cancelled upload.
  const queue = useRef<Map<string, { transfer: Transfer; force: boolean }>>(new Map());
  const aborts = useRef<Map<string, AbortController>>(new Map());
  const cancelled = useRef<Set<string>>(new Set());
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
      update(id, { status: "sending" });
      void send(id, item.transfer, item.force);
    }

    /**
     * One request, with a watchdog on it.
     *
     * The watchdog is the difference between a bad link costing a retry and a
     * bad link costing fifteen minutes of a frozen progress bar: a connection
     * that stops moving without closing is the common wifi failure, and
     * nothing below the application notices it quickly.
     */
    async function attempt<T>(
      id: string,
      run: (signal: AbortSignal, onProgress: (sent: number) => void) => Promise<T>,
      onProgress: (sent: number) => void,
    ): Promise<T> {
      const controller = new AbortController();
      aborts.current.set(id, controller);
      let lastMoved = Date.now();
      const watchdog = setInterval(() => {
        if (Date.now() - lastMoved > STALL_MS) controller.abort();
      }, 5_000);
      try {
        return await run(controller.signal, (sent) => {
          lastMoved = Date.now();
          onProgress(sent);
        });
      } finally {
        clearInterval(watchdog);
        aborts.current.delete(id);
      }
    }

    async function send(id: string, transfer: Transfer, force: boolean) {
      // First attempt creates rather than replaces: an upload that silently
      // overwrote a book somebody else put there would be the one thing this
      // feature must not do. A `force` retry is somebody answering the
      // conflict.
      const precondition: Precondition = force ? { kind: "force" } : { kind: "create" };
      const path = joinPath(transfer.dir, transfer.name);
      try {
        if (transfer.total <= CHUNK_BYTES) {
          await withRetries(id, transfer, (attemptNo) =>
            attempt(
              id,
              (signal, onProgress) =>
                uploadFile(transfer.rootId, path, transfer.file, precondition, {
                  signal,
                  onProgress,
                }),
              (sent) => update(id, { sent, attempt: attemptNo }),
            ),
          );
        } else {
          await sendInChunks(id, transfer, path, precondition);
        }
        update(id, { status: "done", sent: transfer.total, error: undefined });
        void refresh.directory(nodeKey(transfer.rootId, transfer.dir));
        // The root's free space moved, and it is on screen.
        void refresh.roots();
      } catch (error) {
        if (cancelled.current.has(id)) {
          update(id, { status: "cancelled" });
        } else if (error instanceof ApiError && error.code === "precondition_failed") {
          // Something is already there. Not an error yet — a question.
          update(id, { status: "conflict", error: "Something is already there." });
        } else {
          update(id, { status: "error", error: describe(error) });
        }
      } finally {
        cancelled.current.delete(id);
        queue.current.delete(id);
        running.current -= 1;
        pump();
      }
    }

    /**
     * A file that does not fit in one request, sent as pieces that append to
     * the same part file on the device.
     *
     * It opens by *asking* where to carry on from, which is what makes a
     * retry — or a fresh page with the same file dropped on it — resume
     * instead of start over. Every recovery asks again rather than assuming,
     * because a connection that died mid-chunk left the device holding part
     * of one: re-sending from the chunk boundary would send bytes it already
     * has, which on a bad link is exactly the waste this is here to avoid.
     */
    async function sendInChunks(
      id: string,
      transfer: Transfer,
      path: string,
      precondition: Precondition,
    ) {
      const resync = async () => {
        let at = await uploadOffset(transfer.rootId, path, transfer.token);
        if (at > transfer.total) {
          // The device holds something longer than this file: a different
          // file under the same token. Start again rather than append
          // nonsense to it.
          await abandonUpload(transfer.rootId, path, transfer.token);
          at = 0;
        }
        return at;
      };

      let offset = await resync();
      update(id, { sent: offset, resumedFrom: offset > 0 ? offset : undefined });

      // Counted consecutively and reset by every success, so a long file over
      // a bad link keeps going: three failures in a row is a link that is
      // down, three failures across four gigabytes is a Tuesday.
      let failures = 0;
      while (offset < transfer.total) {
        if (cancelled.current.has(id)) throw new Error("cancelled");
        const end = Math.min(offset + CHUNK_BYTES, transfer.total);
        const base = offset;
        try {
          await attempt(
            id,
            (signal, onProgress) =>
              uploadChunk(
                transfer.rootId,
                path,
                transfer.token,
                transfer.file.slice(base, end),
                base,
                transfer.total,
                precondition,
                { signal, onProgress },
              ),
            (sentInChunk) => update(id, { sent: base + sentInChunk }),
          );
          offset = end;
          failures = 0;
          update(id, { sent: offset, attempt: 1 });
        } catch (error) {
          if (cancelled.current.has(id)) throw error;
          // A `conflict` is the device saying it holds a different amount than
          // this loop thought — an answer, not a failure, and the same
          // recovery either way.
          const recoverable =
            isTransient(error) ||
            (error instanceof ApiError && error.code === "conflict");
          failures += 1;
          if (!recoverable || failures >= ATTEMPTS) throw error;
          update(id, { attempt: failures + 1 });
          await delay(BACKOFF_MS[Math.min(failures - 1, BACKOFF_MS.length - 1)]);
          offset = await resync();
          update(id, { sent: offset });
        }
      }
    }

    /** Retry the transient failures, and only those. */
    async function withRetries<T>(
      id: string,
      transfer: Transfer,
      run: (attemptNo: number) => Promise<T>,
    ): Promise<T> {
      let lastError: unknown;
      for (let i = 0; i < ATTEMPTS; i += 1) {
        if (cancelled.current.has(id)) throw new Error("cancelled");
        try {
          return await run(i + 1);
        } catch (error) {
          lastError = error;
          if (cancelled.current.has(id) || !isTransient(error) || i === ATTEMPTS - 1) {
            throw error;
          }
          update(id, { attempt: i + 2, error: `Retrying ${transfer.name}…` });
          await delay(BACKOFF_MS[Math.min(i, BACKOFF_MS.length - 1)]);
        }
      }
      throw lastError;
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
          token: uploadToken(dir, file),
          status: "queued",
          sent: 0,
          total: file.size,
          attempt: 1,
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

  const cancel = useCallback(
    (id: string) => {
      cancelled.current.add(id);
      aborts.current.get(id)?.abort();
      // A transfer still waiting its turn never reaches the sender, so it has
      // to be taken off the queue here.
      const waiting = queue.current.get(id);
      queue.current.delete(id);
      update(id, { status: "cancelled" });
      // And the bytes the device is holding go too. The sweep would collect
      // them a day later; a cancel that leaves gigabytes on a small disk until
      // tomorrow is not a cancel.
      setTransfers((list) => {
        const transfer = waiting?.transfer ?? list.find((t) => t.id === id);
        if (transfer && transfer.total > CHUNK_BYTES) {
          void abandonUpload(
            transfer.rootId,
            joinPath(transfer.dir, transfer.name),
            transfer.token,
          ).catch(() => {});
        }
        return list;
      });
    },
    [update],
  );

  /** Put a transfer back on the queue. `force` answers a conflict. */
  const requeue = useCallback(
    (id: string, force: boolean) => {
      cancelled.current.delete(id);
      setTransfers((list) => {
        const transfer = list.find((t) => t.id === id);
        if (transfer) {
          queue.current.set(id, {
            transfer: { ...transfer, status: "queued" },
            force,
          });
        }
        return list.map((t) =>
          t.id === id
            ? { ...t, status: "queued", attempt: 1, error: undefined }
            : t,
        );
      });
      pump();
    },
    [pump],
  );

  const replace = useCallback((id: string) => requeue(id, true), [requeue]);
  const retry = useCallback((id: string) => requeue(id, false), [requeue]);

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
    () => ({ transfers, start, cancel, replace, retry, dismiss, clearFinished }),
    [transfers, start, cancel, replace, retry, dismiss, clearFinished],
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

/**
 * Whether trying again could plausibly work.
 *
 * A dropped connection, a timeout, a stall the watchdog cut off, a device that
 * answered 5xx: yes. Anything the device *decided* — too large, no room,
 * forbidden, a precondition that did not hold — is an answer, and re-sending
 * gigabytes to hear it again helps nobody.
 */
export function isTransient(error: unknown): boolean {
  // Not every 5xx: `507 Insufficient Storage` is the device saying the disk is
  // full, which re-sending will not change. These four are the ones that mean
  // "something went wrong in the middle, ask again".
  const TRY_AGAIN = [500, 502, 503, 504];
  if (error instanceof ApiError) return TRY_AGAIN.includes(error.status);
  if (error instanceof Error && error.message === "cancelled") return false;
  // No response at all: the network, or the watchdog's abort.
  return true;
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
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
