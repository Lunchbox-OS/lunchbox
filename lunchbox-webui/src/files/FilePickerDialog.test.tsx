// @vitest-environment jsdom
/**
 * The picker, in a DOM (issue #186).
 *
 * The arithmetic — what a row becomes, what may be picked, where a value
 * points — is `pick.test.ts`. What is worth mounting for is the behaviour
 * that only exists across one: that the dialog *opens where the field already
 * points* rather than at the roots, that a refusal reaches the button, and
 * that a device serving no file routes says so instead of showing an empty
 * tree.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { DirEntryInfo, Listing, RootsResponse } from "./types";
import type { PickRequest } from "../config/pick/FilePicker";

const getFileRoots = vi.fn();
const listDirectory = vi.fn();
const uploadFile = vi.fn();

vi.mock("../api/files", async () => {
  const actual = await vi.importActual<typeof import("../api/files")>("../api/files");
  return {
    ...actual,
    getFileRoots: () => getFileRoots(),
    listDirectory: (root: string, path: string, maxPages?: number) =>
      listDirectory(root, path, maxPages),
    uploadFile: (...args: unknown[]) => uploadFile(...args),
  };
});

const { FilePickerDialog } = await import("./FilePickerDialog");
const { UploadsProvider } = await import("./useUploads");
const { ApiError } = await import("../api/client");

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

const ROOTS: RootsResponse = {
  roots: [
    {
      id: "home",
      label: "Home",
      kind: "home",
      path: "/home/kiosk",
      writable: true,
      total_bytes: null,
      free_bytes: null,
    },
    {
      id: "ext-9f2a1c04",
      label: "KINGSTON",
      kind: "external",
      path: "/media/kiosk/KINGSTON",
      writable: true,
      total_bytes: null,
      free_bytes: null,
    },
  ],
  limits: { max_upload_bytes: 0, free_space_floor_bytes: 0 },
};

function file(name: string, over: Partial<DirEntryInfo> = {}): DirEntryInfo {
  return {
    name,
    kind: "file",
    size: 1863410,
    modified: "2026-08-30T09:12:44-04:00",
    etag: "1863410-1756557164123456789",
    hidden: name.startsWith("."),
    symlink: false,
    ...over,
  };
}

const folder = (name: string, over: Partial<DirEntryInfo> = {}) =>
  file(name, { kind: "dir", size: null, etag: null, writable: true, ...over });

const listing = (root: string, path: string, entries: DirEntryInfo[]): Listing => ({
  root,
  path,
  writable: true,
  entries,
  truncated: false,
  cursor: null,
});

/** The home directory used throughout: Books/the-hobbit.epub, and a dotfile. */
function library() {
  listDirectory.mockImplementation(async (root: string, path: string) => {
    if (root !== "home") return listing(root, path, []);
    if (path === "") {
      return listing(root, "", [folder("Books"), folder(".config"), file("notes.txt")]);
    }
    if (path === "Books") {
      return listing(root, path, [
        file("the-hobbit.epub"),
        file("��.epub", { unusable: "name_not_utf8", handle: "op-7" }),
      ]);
    }
    if (path === ".config") return listing(root, path, [folder("lunchbox")]);
    if (path === ".config/lunchbox") return listing(root, path, [file("movies.toml")]);
    return listing(root, path, []);
  });
}

function show(request: PickRequest, onChoose = vi.fn(), onCancel = vi.fn()) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <UploadsProvider>
        <FilePickerDialog request={request} onChoose={onChoose} onCancel={onCancel} />
      </UploadsProvider>
    </QueryClientProvider>,
  );
  return { onChoose, onCancel };
}

const useThis = () => screen.getByRole("button", { name: "Use this" }) as HTMLButtonElement;

describe("opening where the field points", () => {
  it("expands down to the file and offers it back unchanged", async () => {
    getFileRoots.mockResolvedValue(ROOTS);
    library();
    const { onChoose } = show({
      kind: "file",
      what: "a book",
      start: "~/Books/the-hobbit.epub",
    });

    expect(await screen.findByText("the-hobbit.epub")).toBeTruthy();
    await waitFor(() => expect(useThis().disabled).toBe(false));
    await userEvent.click(useThis());
    expect(onChoose).toHaveBeenCalledWith("~/Books/the-hobbit.epub");
  });

  // ~/.config/lunchbox/movies.toml is the stock media library, and a dialog
  // that opened without showing what the field says would be a puzzle.
  it("turns hidden files on when the path goes through one", async () => {
    getFileRoots.mockResolvedValue(ROOTS);
    library();
    show({
      kind: "file",
      what: "a library file",
      start: "~/.config/lunchbox/movies.toml",
    });

    expect(await screen.findByText("movies.toml")).toBeTruthy();
    // The dialog has exactly one switch.
    expect((screen.getByRole("switch") as HTMLInputElement).checked).toBe(true);
  });

  it("opens the home directory when the field points nowhere here", async () => {
    getFileRoots.mockResolvedValue(ROOTS);
    library();
    show({ kind: "file", what: "a library file", start: "https://youtube.com/playlist?list=PL" });

    expect(await screen.findByText("notes.txt")).toBeTruthy();
    // And nothing was selected, so there is nothing to confirm yet.
    expect(useThis().disabled).toBe(true);
  });
});

describe("what it will not accept", () => {
  it("says a folder is a folder, and keeps the button off", async () => {
    getFileRoots.mockResolvedValue(ROOTS);
    library();
    show({ kind: "file", what: "a book", start: "~/" });

    await userEvent.click(await screen.findByText("Books"));
    expect(screen.getByText("That is a folder.")).toBeTruthy();
    expect(useThis().disabled).toBe(true);
  });

  // TOML is UTF-8: this file is listed, and no policy can name it.
  it("refuses a name that is not text", async () => {
    getFileRoots.mockResolvedValue(ROOTS);
    library();
    show({ kind: "file", what: "a book", start: "~/Books/the-hobbit.epub" });

    await userEvent.click(await screen.findByText("��.epub"));
    expect(
      screen.getByText("That name is not text, so no config can refer to it."),
    ).toBeTruthy();
    expect(useThis().disabled).toBe(true);
  });
});

describe("a device that is not offering its files", () => {
  it("says so, rather than showing an empty tree", async () => {
    getFileRoots.mockRejectedValue(new ApiError(404, "not_found", "Not found"));
    show({ kind: "file", what: "a book" });

    expect(await screen.findByText(/no longer on this device|Not found/)).toBeTruthy();
    expect(screen.queryByRole("tree")).toBeNull();
  });
});

/**
 * Uploading from the dialog (issue #186).
 *
 * The queue is the Files tab's — the editor renders inside the same
 * `UploadsProvider` — so what is worth checking here is the wiring either
 * side of it: that the file goes where the selection says, and that the
 * *arrival* rather than the *sending* is what the field is allowed to point
 * at. The device assembles an upload under a part file and publishes it with
 * one rename, so a path written mid-transfer names nothing.
 */
describe("putting the file there first", () => {
  const upload = () => screen.getByRole("button", { name: "Upload" }) as HTMLButtonElement;

  it("has nowhere to put anything until a folder is chosen", async () => {
    getFileRoots.mockResolvedValue(ROOTS);
    library();
    show({ kind: "file", what: "a book" });

    expect(await screen.findByText("notes.txt")).toBeTruthy();
    expect(upload().disabled).toBe(true);

    await userEvent.click(screen.getByText("Books"));
    expect(upload().disabled).toBe(false);
  });

  // Somebody who has just been told "that book is not there" has the missing
  // file selected, not the folder holding it.
  it("aims at the folder holding the selected file", async () => {
    getFileRoots.mockResolvedValue(ROOTS);
    library();
    uploadFile.mockResolvedValue({ path: "Books/redwall.epub", size: 4, etag: "4-1" });
    show({ kind: "file", what: "a book", start: "~/Books/the-hobbit.epub" });

    expect(await screen.findByText("the-hobbit.epub")).toBeTruthy();
    await userEvent.upload(
      document.querySelector("input[type=file]") as HTMLInputElement,
      new File(["abcd"], "redwall.epub"),
    );

    await waitFor(() => expect(uploadFile).toHaveBeenCalled());
    const [root, path] = uploadFile.mock.calls[0];
    expect([root, path]).toEqual(["home", "Books/redwall.epub"]);
  });

  it("offers the uploaded file once the device actually has it", async () => {
    getFileRoots.mockResolvedValue(ROOTS);
    library();
    let finish: (value: unknown) => void = () => {};
    uploadFile.mockReturnValue(new Promise((resolve) => (finish = resolve)));
    const { onChoose } = show({
      kind: "file",
      what: "a book",
      start: "~/Books/the-hobbit.epub",
    });

    expect(await screen.findByText("the-hobbit.epub")).toBeTruthy();
    await userEvent.upload(
      document.querySelector("input[type=file]") as HTMLInputElement,
      new File(["abcd"], "redwall.epub"),
    );
    await waitFor(() => expect(uploadFile).toHaveBeenCalled());

    // Mid-transfer there is nothing on the device under that name, so the
    // selection has not moved off the file the field already named.
    await userEvent.click(useThis());
    expect(onChoose).toHaveBeenCalledWith("~/Books/the-hobbit.epub");

    // It lands, the folder is re-read, and the new file is what is selected.
    listDirectory.mockImplementation(async (root: string, path: string) =>
      path === "Books"
        ? listing(root, path, [file("the-hobbit.epub"), file("redwall.epub")])
        : listing(root, path, []),
    );
    finish({ path: "Books/redwall.epub", size: 4, etag: "4-1" });

    await waitFor(() => expect(uploadFile).toHaveBeenCalled());
    expect(await screen.findByText("redwall.epub")).toBeTruthy();
    await waitFor(() => expect(useThis().disabled).toBe(false));
    await userEvent.click(useThis());
    expect(onChoose).toHaveBeenLastCalledWith("~/Books/redwall.epub");
  });
});
