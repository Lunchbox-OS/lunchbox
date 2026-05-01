import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { sseUrl } from "../api/client";

/**
 * Subscribes to the shepherd SSE event stream and invalidates all cached
 * queries on every event so TanStack Query refetches fresh data automatically.
 */
export function useEvents(): void {
  const queryClient = useQueryClient();

  useEffect(() => {
    let es: EventSource | null = null;
    let retryTimer: ReturnType<typeof setTimeout> | null = null;

    function connect() {
      try {
        es = new EventSource(sseUrl());
        es.onmessage = () => queryClient.invalidateQueries();
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
  }, [queryClient]);
}
