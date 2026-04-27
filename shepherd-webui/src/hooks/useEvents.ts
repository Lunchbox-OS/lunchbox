import { useEffect, useRef } from "react";
import { sseUrl } from "../api/client";

export type EventCallback = () => void;

/**
 * Subscribes to the shepherd SSE event stream and calls `onEvent` on every
 * event received. The callback is stable — callers should call refetch
 * functions rather than relying on event payloads for state updates.
 */
export function useEvents(onEvent: EventCallback): void {
  const cbRef = useRef(onEvent);
  cbRef.current = onEvent;

  useEffect(() => {
    let es: EventSource | null = null;
    let retryTimer: ReturnType<typeof setTimeout> | null = null;

    function connect() {
      try {
        es = new EventSource(sseUrl());
        es.onmessage = () => cbRef.current();
        es.onerror = () => {
          es?.close();
          es = null;
          retryTimer = setTimeout(connect, 5000);
        };
      } catch {
        retryTimer = setTimeout(connect, 5000);
      }
    }

    connect();

    return () => {
      es?.close();
      if (retryTimer) clearTimeout(retryTimer);
    };
  }, []);
}
