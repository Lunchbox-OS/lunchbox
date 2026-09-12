/**
 * The file manager's client (issue #195).
 *
 * Hand-written, like `getDeviceConfig`, because these are not RPCs. It speaks
 * the same axios instance as everything else, so the base URL, the session
 * cookie and the bearer token all work the way they do everywhere.
 */
import { apiHttp, apiUrl, isSameOriginApi } from "./client";
import type { Listing, RootsResponse } from "../files/types";

/**
 * How many pages one directory will fetch before it stops and offers "show
 * more".
 *
 * The API caps a page at 5000 entries and defaults to 1000, so this is 5000
 * rows. A directory holding a ROM set really does have tens of thousands of
 * files in it, and at some point the honest answer is a row saying so rather
 * than a browser tab that stops responding.
 */
export const DEFAULT_MAX_PAGES = 5;

export async function getFileRoots(): Promise<RootsResponse> {
  const res = await apiHttp.get<RootsResponse>("/files/roots");
  return res.data;
}

/**
 * One directory, with its pages already joined.
 *
 * Paging lives here rather than in an `useInfiniteQuery` because the tree
 * needs *one* result per directory to walk: the number of expanded folders
 * changes between renders, so their queries have to go through `useQueries`,
 * which has no infinite form. Looping here is what that hook would have done
 * anyway, and it keeps `truncated` meaning "there is more than this UI will
 * load", which is exactly what the row at the bottom says.
 */
export async function listDirectory(
  root: string,
  path: string,
  maxPages: number = DEFAULT_MAX_PAGES,
): Promise<Listing> {
  let page = await fetchPage(root, path);
  let pages = 1;
  const entries = [...page.entries];
  while (page.truncated && page.cursor && pages < maxPages) {
    page = await fetchPage(root, path, page.cursor);
    pages += 1;
    entries.push(...page.entries);
  }
  return { ...page, entries, truncated: page.truncated };
}

async function fetchPage(
  root: string,
  path: string,
  cursor?: string,
): Promise<Listing> {
  const res = await apiHttp.get<Listing>("/files/list", {
    params: { root, path, ...(cursor ? { cursor } : {}) },
  });
  return res.data;
}

/** The URL a download comes from. Also what a same-origin `<a download>` uses. */
export function fileContentUrl(root: string, path: string): string {
  const query = new URLSearchParams({ root, path }).toString();
  return apiUrl(`/files/content?${query}`);
}

/**
 * Above this, a cross-origin download warns first: that path buffers the whole
 * file in memory, and a ROM is not a thing to buffer.
 */
export const LARGE_DOWNLOAD_BYTES = 100 * 1024 * 1024;

/**
 * Save a file.
 *
 * Two paths, and the difference is not cosmetic. Same-origin, the browser can
 * fetch it itself: the `HttpOnly` session cookie rides along, and the download
 * streams to disk at no memory cost. Pointed at *another* device through
 * `ConnectionSettings`, the credential is a bearer token that a plain anchor
 * cannot carry, so the bytes have to come through axios and land in a blob —
 * which does cost memory, which is why the caller is asked first above
 * `LARGE_DOWNLOAD_BYTES`.
 */
export async function downloadFile(
  root: string,
  path: string,
  name: string,
): Promise<void> {
  const url = fileContentUrl(root, path);
  if (isSameOriginApi()) {
    clickDownload(url, name);
    return;
  }
  const res = await apiHttp.get<Blob>("/files/content", {
    params: { root, path },
    responseType: "blob",
  });
  const objectUrl = URL.createObjectURL(res.data);
  try {
    clickDownload(objectUrl, name);
  } finally {
    // A revoke in the same tick cancels the download in some browsers; one
    // frame later is enough for the click to have been taken.
    setTimeout(() => URL.revokeObjectURL(objectUrl), 1000);
  }
}

// ---------------------------------------------------------------------------
// Writes
//
// Every one of these carries a precondition, because the device has more
// writers than this browser: the activities running at the kiosk's own uid,
// whoever is sitting at it, and `sudoedit`. A forgotten precondition is a 428
// rather than a clobber, which is the API's decision and this module's job to
// honour.
// ---------------------------------------------------------------------------

/** What the caller believes is at the path, in the header the API expects. */
export type Precondition =
  | { kind: "create" }
  | { kind: "replace"; etag: string }
  | { kind: "force" };

function preconditionHeaders(precondition: Precondition): Record<string, string> {
  switch (precondition.kind) {
    case "create":
      return { "If-None-Match": "*" };
    case "replace":
      return { "If-Match": precondition.etag };
    case "force":
      return { "If-Match": "*" };
  }
}

export interface UploadResult {
  path: string;
  size: number;
  etag: string;
}

/**
 * Send one file.
 *
 * A raw body rather than `multipart/form-data`: it streams without a parser,
 * needs no extra axum feature on the other side, and — the reason it is axios
 * and not `fetch` — it reports progress, which `fetch` cannot do for an upload.
 */
export async function uploadFile(
  root: string,
  path: string,
  file: Blob,
  precondition: Precondition,
  options: { signal?: AbortSignal; onProgress?: (sent: number) => void } = {},
): Promise<UploadResult> {
  const res = await apiHttp.put<UploadResult>("/files/content", file, {
    params: { root, path },
    headers: {
      "Content-Type": "application/octet-stream",
      ...preconditionHeaders(precondition),
    },
    signal: options.signal,
    onUploadProgress: (event) => options.onProgress?.(event.loaded),
  });
  return res.data;
}

/** Create a folder, and every folder above it that is missing. */
export async function createDirectory(root: string, path: string): Promise<void> {
  await apiHttp.post("/files/dir", { root, path });
}

/**
 * Rename, or move within one root.
 *
 * Cross-root moves are not expressible — the route takes a single `root` —
 * because `rename(2)` does not cross filesystems. A caller that wants one
 * downloads and re-uploads.
 */
export async function moveEntry(
  root: string,
  from: string,
  to: string,
  overwrite = false,
): Promise<void> {
  await apiHttp.post("/files/move", { root, from, to, overwrite });
}

/** Delete a file, or a folder and optionally everything in it. */
export async function deleteEntry(
  root: string,
  path: string,
  precondition: Precondition,
  recursive = false,
): Promise<void> {
  await apiHttp.delete("/files/entry", {
    params: { root, path, ...(recursive ? { recursive: true } : {}) },
    headers: preconditionHeaders(precondition),
  });
}

function clickDownload(url: string, name: string): void {
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.rel = "noopener";
  document.body.append(a);
  a.click();
  a.remove();
}
