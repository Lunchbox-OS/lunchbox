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

function clickDownload(url: string, name: string): void {
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.rel = "noopener";
  document.body.append(a);
  a.click();
  a.remove();
}
