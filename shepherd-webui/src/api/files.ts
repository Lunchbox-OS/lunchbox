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

/**
 * How much of a file goes in one request.
 *
 * The unit of *retry*, which is the point: on a link that drops every few
 * minutes, a failed 8 MiB chunk costs 8 MiB, and the rest of the file stays on
 * the device. Small enough that a bad chip can finish one between drops, large
 * enough that a 4 GiB video is not 4000 round trips.
 */
export const CHUNK_BYTES = 8 * 1024 * 1024;

/** How long one chunk may take before it is abandoned and retried. */
export const CHUNK_TIMEOUT_MS = 120_000;

/**
 * Send one chunk of a resumable upload.
 *
 * `Content-Range: bytes X-Y/Z` says which piece this is; the device appends it
 * to a part file named by `token` and answers `204` with `Upload-Offset` until
 * the last byte lands, when it renames the part into place and answers with
 * the file's new tag.
 *
 * A `409` means this device holds a different amount than the caller thought —
 * the answer to which is [`uploadOffset`], not a retry of the same bytes.
 */
export async function uploadChunk(
  root: string,
  path: string,
  token: string,
  chunk: Blob,
  start: number,
  total: number,
  precondition: Precondition,
  options: { signal?: AbortSignal; onProgress?: (sentInChunk: number) => void } = {},
): Promise<UploadResult | null> {
  const end = start + chunk.size - 1;
  const res = await apiHttp.put<UploadResult | null>("/files/content", chunk, {
    params: { root, path, upload: token },
    headers: {
      "Content-Type": "application/octet-stream",
      "Content-Range": `bytes ${start}-${end}/${total}`,
      ...preconditionHeaders(precondition),
    },
    signal: options.signal,
    timeout: CHUNK_TIMEOUT_MS,
    onUploadProgress: (event) => options.onProgress?.(event.loaded),
  });
  // 204 while there is more to come; the finished body when there is not.
  return res.status === 204 ? null : res.data;
}

/** How much of a resumable upload this device already holds. */
export async function uploadOffset(
  root: string,
  path: string,
  token: string,
): Promise<number> {
  const res = await apiHttp.get<{ offset: number }>("/files/upload", {
    params: { root, path, upload: token },
  });
  return res.data.offset;
}

/** Give up on a resumable upload, and take its bytes with it. */
export async function abandonUpload(
  root: string,
  path: string,
  token: string,
): Promise<void> {
  await apiHttp.delete("/files/upload", { params: { root, path, upload: token } });
}

/**
 * An upload's identity, and therefore the name of the part file it
 * accumulates in.
 *
 * Derived from the file rather than random, so that re-adding the same file
 * after a browser reload resumes what is already on the device instead of
 * starting a second copy of it. The alphabet is what the API accepts.
 */
export function uploadToken(dir: string, file: File): string {
  const seed = `${dir}/${file.name}:${file.size}:${file.lastModified}`;
  // FNV-1a, twice with different offsets: enough to tell two files apart, and
  // nothing here is defending against a collision somebody wants.
  const hash = (offset: number) => {
    let h = offset;
    for (let i = 0; i < seed.length; i += 1) {
      h ^= seed.charCodeAt(i);
      h = Math.imul(h, 0x01000193) >>> 0;
    }
    return h.toString(36).padStart(7, "0");
  };
  return `u-${hash(0x811c9dc5)}${hash(0x2f1e3a77)}`;
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
