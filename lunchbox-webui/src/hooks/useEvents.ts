import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { openEventStream } from "../api/client";

/** How long to wait before reconnecting a stream that ended or failed. */
const RETRY_MS = 5000;

/**
 * Subscribes to the Lunchbox SSE event stream and invalidates all cached
 * queries on every event so TanStack Query refetches fresh data automatically.
 *
 * Reads the stream with `fetch` rather than `EventSource` so the token travels
 * in an `Authorization` header like every other request — see
 * `openEventStream`. Nothing is lost by giving up `EventSource`: this hook
 * always drove its own reconnect, and the server sends no `id:` field, so
 * there is no `Last-Event-ID` resumption to preserve.
 */
export function useEvents(): void {
  const queryClient = useQueryClient();

  useEffect(() => {
    const ac = new AbortController();
    let retryTimer: ReturnType<typeof setTimeout> | null = null;

    // Reads until the server closes the stream; throws if it never opened.
    async function pump(): Promise<void> {
      const res = await openEventStream(ac.signal);
      if (!res.ok || !res.body) {
        throw new Error(`event stream: HTTP ${res.status}`);
      }
      const reader = res.body.pipeThrough(new TextDecoderStream()).getReader();
      let buf = "";
      for (;;) {
        const { value, done } = await reader.read();
        if (done) return;
        // Normalise CRLF so the frame split below only has one case to handle.
        buf += value.replace(/\r\n/g, "\n");
        let end: number;
        while ((end = buf.indexOf("\n\n")) >= 0) {
          const frame = buf.slice(0, end);
          buf = buf.slice(end + 2);
          // One invalidation per frame, not per line: a multi-line `data:`
          // payload is still a single event. Keep-alive comments (":") and
          // fields we don't consume fall through untouched.
          if (frame.split("\n").some((line) => line.startsWith("data:"))) {
            // Everything except the file manager's listings (issue #195).
            // Nothing on the event stream describes the filesystem — the
            // daemon emits no event when a file changes — so invalidating them
            // here would refetch every expanded folder on every volume nudge
            // and session tick, for an answer that cannot have changed because
            // of the thing that was announced.
            queryClient.invalidateQueries({
              predicate: (query) => query.queryKey[0] !== "files",
            });
          }
        }
      }
    }

    function connect() {
      // Retry on both outcomes: a stream that failed to open and one the daemon
      // closed cleanly (a restart) both mean "try again shortly".
      pump()
        .catch(() => {})
        .then(() => {
          if (!ac.signal.aborted) retryTimer = setTimeout(connect, RETRY_MS);
        });
    }

    connect();

    return () => {
      ac.abort();
      if (retryTimer) clearTimeout(retryTimer);
    };
  }, [queryClient]);
}
