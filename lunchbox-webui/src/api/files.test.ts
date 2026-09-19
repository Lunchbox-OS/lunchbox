// @vitest-environment jsdom
/**
 * The file manager's transport (issue #195).
 *
 * Everything here is the shape of a request rather than what the UI does with
 * the answer — which is exactly the layer that has no other test and the one
 * where a mistake is silent. A precondition header spelt wrongly does not throw
 * anything: it clobbers a file. A `Content-Range` off by one does not throw
 * anything either: it desynchronises an upload that then re-sends four
 * gigabytes.
 *
 * The device's own half of each of these contracts is tested in
 * `crates/lunchbox-http/tests/files.rs`; these are the two ends of the same
 * wire, and both have to be pinned for either to mean anything.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const get = vi.fn();
const put = vi.fn();
const post = vi.fn();
const del = vi.fn();
const sameOrigin = vi.fn(() => true);

vi.mock("./client", () => ({
  apiHttp: {
    get: (...a: unknown[]) => get(...a),
    put: (...a: unknown[]) => put(...a),
    post: (...a: unknown[]) => post(...a),
    delete: (...a: unknown[]) => del(...a),
  },
  apiUrl: (path: string) => `/api/v1${path}`,
  isSameOriginApi: () => sameOrigin(),
}));

const files = await import("./files");

/** One page of a listing, in the shape the route really answers with. */
function page(
  names: string[],
  over: { truncated?: boolean; cursor?: string | null } = {},
) {
  return {
    data: {
      root: "home",
      path: "Books",
      writable: true,
      entries: names.map((name) => ({
        name,
        kind: "file",
        size: 1,
        modified: 0,
        etag: "1-1",
        hidden: false,
        symlink: false,
        unusable: null,
        writable: null,
      })),
      truncated: over.truncated ?? false,
      cursor: over.cursor ?? null,
    },
  };
}

beforeEach(() => {
  sameOrigin.mockReturnValue(true);
});
afterEach(() => vi.clearAllMocks());

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

describe("listing a directory", () => {
  it("joins the pages the device hands out, and asks for no cursor first", async () => {
    get
      .mockResolvedValueOnce(page(["a", "b"], { truncated: true, cursor: "c1" }))
      .mockResolvedValueOnce(page(["c"], { truncated: false }));

    const listing = await files.listDirectory("home", "Books");

    expect(listing.entries.map((e) => e.name)).toEqual(["a", "b", "c"]);
    expect(get).toHaveBeenCalledTimes(2);
    // No `cursor` key at all on the first request — an empty one would be a
    // different question than no question.
    expect(get.mock.calls[0][1]).toEqual({ params: { root: "home", path: "Books" } });
    expect(get.mock.calls[1][1]).toEqual({
      params: { root: "home", path: "Books", cursor: "c1" },
    });
    // The last page said there was no more, so neither does this.
    expect(listing.truncated).toBe(false);
  });

  it("stops at the page budget and says the folder is still truncated", async () => {
    // A ROM set: the device would keep paging forever, and at some point the
    // honest answer is a row saying so rather than a tab that stops responding.
    get.mockResolvedValue(page(["x"], { truncated: true, cursor: "more" }));

    const listing = await files.listDirectory("home", "Roms", 3);

    expect(get).toHaveBeenCalledTimes(3);
    expect(listing.entries).toHaveLength(3);
    expect(listing.truncated).toBe(true);
  });

  it("stops when the device stops handing out a cursor, however truncated it says it is", async () => {
    // Belt and braces: `truncated` without a `cursor` is the device saying
    // there is more but not where, and looping on it would be infinite.
    get.mockResolvedValue(page(["x"], { truncated: true, cursor: null }));
    const listing = await files.listDirectory("home", "Roms");
    expect(get).toHaveBeenCalledTimes(1);
    expect(listing.entries).toHaveLength(1);
  });
});

// ---------------------------------------------------------------------------
// Preconditions
// ---------------------------------------------------------------------------

describe("preconditions", () => {
  it("spells each of the three the way the route reads them", async () => {
    put.mockResolvedValue({ status: 201, data: { path: "p", size: 1, etag: "1-1" } });

    await files.uploadFile("home", "Books/new.epub", new Blob(["x"]), { kind: "create" });
    expect(put.mock.calls[0][2].headers["If-None-Match"]).toBe("*");

    await files.uploadFile("home", "Books/new.epub", new Blob(["x"]), {
      kind: "replace",
      etag: "16-1789400724326124000",
    });
    expect(put.mock.calls[1][2].headers["If-Match"]).toBe("16-1789400724326124000");

    await files.uploadFile("home", "Books/new.epub", new Blob(["x"]), { kind: "force" });
    expect(put.mock.calls[2][2].headers["If-Match"]).toBe("*");
  });

  it("puts one on a delete too, because the route requires it", async () => {
    // A delete with no precondition is a 428, not a deletion — the device has
    // more writers than this browser and a forgotten tag is how the wrong file
    // goes.
    del.mockResolvedValue({ status: 204 });
    await files.deleteEntry("home", "Books/old.epub", { kind: "replace", etag: "1-1" });
    expect(del.mock.calls[0][1].headers).toEqual({ "If-Match": "1-1" });
  });

  it("asks for a recursive delete only when it means it", async () => {
    del.mockResolvedValue({ status: 204 });
    await files.deleteEntry("home", "Books/album", { kind: "force" });
    expect(del.mock.calls[0][1].params).toEqual({ root: "home", path: "Books/album" });

    await files.deleteEntry("home", "Books/album", { kind: "force" }, true);
    expect(del.mock.calls[1][1].params).toEqual({
      root: "home",
      path: "Books/album",
      recursive: true,
    });
  });
});

// ---------------------------------------------------------------------------
// Chunks
// ---------------------------------------------------------------------------

describe("a resumable upload", () => {
  it("numbers each chunk inclusively, the last one short", async () => {
    put.mockResolvedValue({ status: 204, data: null });
    const total = 20 * 1024 * 1024;

    await files.uploadChunk(
      "home",
      "Books/film.bin",
      "u-abc12345",
      new Blob(["x".repeat(8)]),
      0,
      total,
      { kind: "create" },
    );
    // `bytes 0-7/…`, not `0-8`: the range is inclusive at both ends, and an
    // off-by-one here is an upload that never agrees with the device about
    // where it got to.
    expect(put.mock.calls[0][2].headers["Content-Range"]).toBe(`bytes 0-7/${total}`);
    expect(put.mock.calls[0][2].params).toEqual({
      root: "home",
      path: "Books/film.bin",
      upload: "u-abc12345",
    });

    const start = total - 3;
    await files.uploadChunk(
      "home",
      "Books/film.bin",
      "u-abc12345",
      new Blob(["abc"]),
      start,
      total,
      { kind: "create" },
    );
    expect(put.mock.calls[1][2].headers["Content-Range"]).toBe(
      `bytes ${start}-${total - 1}/${total}`,
    );
  });

  it("answers null while there is more to come and the file when there is not", async () => {
    put.mockResolvedValueOnce({ status: 204, data: null });
    const more = await files.uploadChunk(
      "home", "Books/film.bin", "u-abc12345", new Blob(["x"]), 0, 2, { kind: "create" },
    );
    expect(more).toBeNull();

    put.mockResolvedValueOnce({
      status: 201,
      data: { path: "Books/film.bin", size: 2, etag: "2-1" },
    });
    const done = await files.uploadChunk(
      "home", "Books/film.bin", "u-abc12345", new Blob(["x"]), 1, 2, { kind: "create" },
    );
    expect(done).toEqual({ path: "Books/film.bin", size: 2, etag: "2-1" });
  });

  it("gives a chunk its own timeout, so a stalled one is retried rather than waited on", async () => {
    put.mockResolvedValue({ status: 204, data: null });
    await files.uploadChunk(
      "home", "Books/film.bin", "u-abc12345", new Blob(["x"]), 0, 2, { kind: "create" },
    );
    expect(put.mock.calls[0][2].timeout).toBe(files.CHUNK_TIMEOUT_MS);
  });
});

describe("an upload's token", () => {
  const file = (over: Partial<{ name: string; size: number; lastModified: number }> = {}) =>
    ({
      name: over.name ?? "film.bin",
      size: over.size ?? 4_294_967_296,
      lastModified: over.lastModified ?? 1_789_400_724_326,
    }) as File;

  it("is the same every time, so a reload resumes instead of starting again", () => {
    expect(files.uploadToken("Books", file())).toBe(files.uploadToken("Books", file()));
  });

  it("changes when anything about the file or its destination does", () => {
    const base = files.uploadToken("Books", file());
    expect(files.uploadToken("Films", file())).not.toBe(base);
    expect(files.uploadToken("Books", file({ name: "other.bin" }))).not.toBe(base);
    expect(files.uploadToken("Books", file({ size: 1 }))).not.toBe(base);
    expect(files.uploadToken("Books", file({ lastModified: 1 }))).not.toBe(base);
  });

  it("is spelt in the alphabet the route accepts", () => {
    // `validate_token`: 8 to 64 characters of letters, digits, `-` and `_`. A
    // token outside that is a 400 on every chunk, which would look like a
    // broken device rather than a broken client.
    for (const dir of ["", "Books", "Books/A Folder — with punctuation!"]) {
      const token = files.uploadToken(dir, file({ name: "a b:c?.bin" }));
      expect(token).toMatch(/^[A-Za-z0-9_-]{8,64}$/);
    }
  });
});

// ---------------------------------------------------------------------------
// Downloads
// ---------------------------------------------------------------------------

describe("saving a file", () => {
  it("is a plain link when the API is this origin, and costs no memory", async () => {
    const clicks: HTMLAnchorElement[] = [];
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (
      this: HTMLAnchorElement,
    ) {
      clicks.push(this);
    });

    await files.downloadFile("home", "Books/Bilbo's Journey.epub", "Bilbo's Journey.epub");

    // The whole point: no request through axios at all, so the browser streams
    // it to disk and the `HttpOnly` session cookie rides along by itself.
    expect(get).not.toHaveBeenCalled();
    expect(clicks).toHaveLength(1);
    expect(clicks[0].download).toBe("Bilbo's Journey.epub");
    expect(clicks[0].getAttribute("href")).toContain("/api/v1/files/content?");
    // And the anchor does not outlive the click.
    expect(document.querySelectorAll("a[download]")).toHaveLength(0);
  });

  it("goes through a blob only when the API is somewhere else", async () => {
    sameOrigin.mockReturnValue(false);
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});
    const createObjectURL = vi.fn(() => "blob:fake");
    const revokeObjectURL = vi.fn();
    Object.assign(URL, { createObjectURL, revokeObjectURL });
    get.mockResolvedValue({ data: new Blob(["bytes"]) });

    vi.useFakeTimers();
    try {
      await files.downloadFile("home", "Books/x.epub", "x.epub");
      // A bearer token is what authenticates a cross-origin call, and an
      // anchor cannot carry one — so the bytes have to come through axios.
      expect(get.mock.calls[0][1].responseType).toBe("blob");
      expect(createObjectURL).toHaveBeenCalledTimes(1);
      // Revoking in the same tick cancels the download in some browsers.
      expect(revokeObjectURL).not.toHaveBeenCalled();
      vi.advanceTimersByTime(1000);
      expect(revokeObjectURL).toHaveBeenCalledWith("blob:fake");
    } finally {
      vi.useRealTimers();
    }
  });

  it("encodes a path the query string would otherwise read as syntax", () => {
    const url = files.fileContentUrl("ext-1A2B-3C4D", "Films/A&B #1 + 2/x?.mkv");
    expect(url).toBe(
      "/api/v1/files/content?root=ext-1A2B-3C4D&path=Films%2FA%26B+%231+%2B+2%2Fx%3F.mkv",
    );
    // Round-trips: what the device parses back out is what was asked for.
    const parsed = new URLSearchParams(url.split("?")[1]);
    expect(parsed.get("path")).toBe("Films/A&B #1 + 2/x?.mkv");
    expect(parsed.get("root")).toBe("ext-1A2B-3C4D");
  });
});

// ---------------------------------------------------------------------------
// The rest
// ---------------------------------------------------------------------------

describe("the routes that take a body", () => {
  it("sends mkdir and move as JSON rather than as query parameters", async () => {
    post.mockResolvedValue({ status: 201 });
    await files.createDirectory("home", "Games/roms/snes");
    expect(post.mock.calls[0]).toEqual([
      "/files/dir",
      { root: "home", path: "Games/roms/snes" },
    ]);

    await files.moveEntry("home", "Books/a.epub", "Books/b.epub");
    expect(post.mock.calls[1]).toEqual([
      "/files/move",
      { root: "home", from: "Books/a.epub", to: "Books/b.epub", overwrite: false },
    ]);

    // Overwrite is opt-in, and the default has to be the safe one: a move that
    // clobbers by default is a lost file nobody asked to lose.
    await files.moveEntry("home", "Books/a.epub", "Books/b.epub", true);
    expect(post.mock.calls[2][1].overwrite).toBe(true);
  });

  it("asks the device where an upload got to, and can give up on one", async () => {
    get.mockResolvedValue({ data: { offset: 8_388_608 } });
    expect(await files.uploadOffset("home", "Books/film.bin", "u-abc12345")).toBe(8_388_608);
    expect(get.mock.calls[0][0]).toBe("/files/upload");

    del.mockResolvedValue({ status: 204 });
    await files.abandonUpload("home", "Books/film.bin", "u-abc12345");
    expect(del.mock.calls[0][0]).toBe("/files/upload");
    expect(del.mock.calls[0][1].params.upload).toBe("u-abc12345");
  });
});
