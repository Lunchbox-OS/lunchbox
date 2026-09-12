/**
 * The writes that are not uploads (issue #195): new folder, rename, delete.
 *
 * Each one carries the precondition the API demands and invalidates exactly
 * what it changed — never the whole tree, because an open folder somewhere
 * else has not moved and refetching it would collapse nothing but would cost a
 * listing.
 *
 * No optimistic updates. Every one of these is a round trip to a disk on the
 * same machine, so the honest render is a brief spinner rather than a guess
 * that has to be taken back when a precondition fails.
 */
import { useMemo } from "react";
import { useMutation } from "@tanstack/react-query";
import { ApiError } from "../api/client";
import { createDirectory, deleteEntry, moveEntry } from "../api/files";
import { describe, useFilesRefresh } from "./useDirectories";
import { joinPath, nodeKey, parentPath, type NodeKey } from "./tree";

export interface NewFolder {
  rootId: string;
  /** The directory it goes in. */
  parent: string;
  name: string;
}

export interface Rename {
  rootId: string;
  path: string;
  name: string;
}

export interface Move {
  rootId: string;
  /** What is being moved. */
  from: string;
  /** The folder it is going into; `""` for the top of the place. */
  toDir: string;
  /** Replace what is already there. Only ever true after somebody was asked. */
  overwrite?: boolean;
}

export interface Delete {
  rootId: string;
  path: string;
  /** The version the row was drawn with; `null` for a folder, which has none. */
  etag: string | null;
  recursive: boolean;
}

export function useFileActions(forgetSubtree: (key: NodeKey) => void) {
  const refresh = useFilesRefresh();

  const newFolder = useMutation({
    mutationFn: ({ rootId, parent, name }: NewFolder) =>
      createDirectory(rootId, joinPath(parent, name)),
    onSuccess: (_data, { rootId, parent }) => {
      void refresh.directory(nodeKey(rootId, parent));
    },
  });

  const rename = useMutation({
    mutationFn: ({ rootId, path, name }: Rename) =>
      moveEntry(rootId, path, joinPath(parentPath(path), name)),
    onSuccess: (_data, { rootId, path }) => {
      void refresh.directory(nodeKey(rootId, parentPath(path)));
      // The old name's subtree is gone as a key even though the files are the
      // same ones; without this, a folder later given the old name arrives
      // pre-expanded with a listing that was never about it.
      forgetSubtree(nodeKey(rootId, path));
    },
  });

  const move = useMutation({
    mutationFn: ({ rootId, from, toDir, overwrite }: Move) =>
      moveEntry(rootId, from, joinPath(toDir, basename(from)), overwrite ?? false),
    onSuccess: (_data, { rootId, from, toDir }) => {
      // Both ends: the folder it left and the folder it arrived in.
      void refresh.directory(nodeKey(rootId, parentPath(from)));
      void refresh.directory(nodeKey(rootId, toDir));
      forgetSubtree(nodeKey(rootId, from));
    },
  });

  const remove = useMutation({
    mutationFn: ({ rootId, path, etag, recursive }: Delete) =>
      deleteEntry(
        rootId,
        path,
        etag ? { kind: "replace", etag } : { kind: "force" },
        recursive,
      ),
    onSuccess: (_data, { rootId, path }) => {
      void refresh.directory(nodeKey(rootId, parentPath(path)));
      void refresh.roots();
      forgetSubtree(nodeKey(rootId, path));
    },
  });

  return useMemo(
    () => ({ newFolder, rename, move, remove }),
    [newFolder, rename, move, remove],
  );
}

/** The last component of a path — what the thing is called. */
export function basename(path: string): string {
  const at = path.lastIndexOf("/");
  return at < 0 ? path : path.slice(at + 1);
}

/**
 * What to say when one of them fails.
 *
 * The `code` is the stable half of an `ApiError`, and each of these has a
 * specific recovery that a generic message would hide.
 */
export function describeWriteFailure(error: unknown, what: string): string {
  if (error instanceof ApiError) {
    switch (error.code) {
      case "conflict":
        return `Something called ${what} is already there.`;
      case "precondition_failed":
        return `${what} changed on the device since this list was read. Refresh and try again.`;
      case "forbidden":
        return `This device will not let ${what} be changed.`;
      case "not_found":
        return `${what} is no longer on this device.`;
      default:
        return error.message;
    }
  }
  return describe(error);
}
