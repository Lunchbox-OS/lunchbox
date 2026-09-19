/**
 * One query per open directory (issue #195).
 *
 * `useQueries` rather than a query per component, because the tree renders as
 * a flat list of rows and the number of open folders changes between renders —
 * which rules out calling a hook per node. It also rules out
 * `useInfiniteQuery`, which has no `useQueries` form; paging therefore happens
 * inside `listDirectory`, which is what the infinite hook would have done
 * anyway.
 */
import { useCallback, useMemo } from "react";
import { useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { ApiError } from "../api/client";
import { DEFAULT_MAX_PAGES, getFileRoots, listDirectory } from "../api/files";
import type { Listing, RootsResponse } from "./types";
import { type DirectoryState, type NodeKey, splitKey } from "./tree";

/** Every query this feature owns, for a wholesale refresh. */
export const FILES_QUERY_KEY = ["files"] as const;

export function useFileRoots() {
  return useQuery<RootsResponse>({
    queryKey: [...FILES_QUERY_KEY, "roots"],
    queryFn: getFileRoots,
  });
}

/**
 * The listings for a set of open directories, keyed the way the walk wants
 * them.
 *
 * `keys` must already be the *reachable* ones — a folder whose parent is shut
 * is still remembered as open, and fetching it would be a request for
 * something nobody can see.
 */
export function useDirectories(
  keys: NodeKey[],
  pageLimits: ReadonlyMap<NodeKey, number>,
): ReadonlyMap<NodeKey, DirectoryState> {
  // Sorted so that opening a second folder does not reorder the first one's
  // position in the results array, which would remount nothing but does make
  // the memo below churn.
  const stable = useMemo(() => [...keys].sort(), [keys]);

  // `combine` rather than a memo over the results array: it is the hook's own
  // answer to "these N queries are really one value", and it is what keeps the
  // map identity stable across the renders where nothing actually moved.
  const combine = useCallback(
    (results: { isPending: boolean; isError: boolean; data?: Listing; error: unknown }[]) => {
      const map = new Map<NodeKey, DirectoryState>();
      stable.forEach((key, i) => {
        const result = results[i];
        if (!result) return;
        map.set(key, {
          status: result.isPending ? "pending" : result.isError ? "error" : "success",
          listing: result.data,
          error: result.error ? describe(result.error) : undefined,
        });
      });
      return map as ReadonlyMap<NodeKey, DirectoryState>;
    },
    [stable],
  );

  return useQueries({
    queries: stable.map((key) => {
      const { rootId, path } = splitKey(key);
      const maxPages = pageLimits.get(key) ?? DEFAULT_MAX_PAGES;
      return {
        queryKey: [...FILES_QUERY_KEY, "dir", rootId, path, maxPages],
        queryFn: () => listDirectory(rootId, path, maxPages),
        // A directory is worth re-reading when the tab comes back, but not on
        // every remount while clicking around the tree.
        staleTime: 10_000,
      };
    }),
    combine,
  });
}

/** Invalidation, in the one place that knows how these keys are shaped. */
export function useFilesRefresh() {
  const queryClient = useQueryClient();
  return useMemo(
    () => ({
      /** Everything: the roots and every open folder. */
      all: () => queryClient.invalidateQueries({ queryKey: FILES_QUERY_KEY }),
      /** One directory, whatever page bound it was fetched with. */
      directory: (key: NodeKey) => {
        const { rootId, path } = splitKey(key);
        return queryClient.invalidateQueries({
          queryKey: [...FILES_QUERY_KEY, "dir", rootId, path],
        });
      },
      /** The roots, whose free space moves whenever anything is written. */
      roots: () =>
        queryClient.invalidateQueries({
          queryKey: [...FILES_QUERY_KEY, "roots"],
        }),
    }),
    [queryClient],
  );
}

/**
 * What went wrong, in the words the daemon used.
 *
 * The `code` is the stable half of an `ApiError` and the message is written
 * for a person, so a failed listing says "that drive is no longer connected"
 * rather than "Request failed with status code 404".
 */
export function describe(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.code === "not_found") {
      return "That folder is no longer on this device.";
    }
    if (error.code === "forbidden") {
      return "This device will not open that folder.";
    }
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}
