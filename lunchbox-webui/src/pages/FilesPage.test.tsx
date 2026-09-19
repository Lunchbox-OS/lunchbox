// @vitest-environment jsdom
/**
 * The file tree, in a DOM (issue #195).
 *
 * The pure half — the walk, the sort, the pruning — is tested in
 * `src/files/tree.test.ts`. What is worth mounting for is the behaviour that
 * only exists across a mount: that opening a folder *fetches* it, that closing
 * one stops asking, and that a row nobody can act on still renders instead of
 * vanishing.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { DirEntryInfo, Listing, RootsResponse } from "../files/types";

const getFileRoots = vi.fn();
const listDirectory = vi.fn();
const downloadFile = vi.fn();
const uploadFile = vi.fn();
const createDirectory = vi.fn();
const moveEntry = vi.fn();
const deleteEntry = vi.fn();

vi.mock("../api/files", async () => {
  const actual =
    await vi.importActual<typeof import("../api/files")>("../api/files");
  return {
    ...actual,
    getFileRoots: () => getFileRoots(),
    listDirectory: (root: string, path: string, maxPages?: number) =>
      listDirectory(root, path, maxPages),
    downloadFile: (root: string, path: string, name: string) =>
      downloadFile(root, path, name),
    uploadFile: (...args: unknown[]) => uploadFile(...args),
    createDirectory: (...args: unknown[]) => createDirectory(...args),
    moveEntry: (...args: unknown[]) => moveEntry(...args),
    deleteEntry: (...args: unknown[]) => deleteEntry(...args),
  };
});

const { FilesPage } = await import("./FilesPage");
const { UploadsProvider } = await import("../files/useUploads");
const { TransferTray } = await import("../files/TransferTray");
const { ApiError } = await import("../api/client");

/**
 * jsdom has no `matchMedia`, and MUI's `useMediaQuery` answers `false` to
 * everything without one — which would put every test in the phone layout,
 * where there are no column headers to click. Answer as a laptop unless a test
 * asks otherwise.
 */
function viewport(kind: "desktop" | "phone") {
  window.matchMedia = ((query: string) => ({
    matches: kind === "desktop" ? query.includes("min-width") : false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}

beforeEach(() => viewport("desktop"));

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
      total_bytes: 100_000_000_000,
      free_bytes: 42_000_000_000,
    },
    {
      id: "ext-9f2a1c04",
      label: "KINGSTON",
      kind: "external",
      path: "/media/kiosk/KINGSTON",
      writable: true,
      total_bytes: 61_000_000_000,
      free_bytes: 60_000_000_000,
    },
  ],
  limits: { max_upload_bytes: 8589934592, free_space_floor_bytes: 2147483648 },
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

function folder(name: string, over: Partial<DirEntryInfo> = {}): DirEntryInfo {
  return file(name, { kind: "dir", size: null, etag: null, writable: true, ...over });
}

function listing(root: string, path: string, entries: DirEntryInfo[]): Listing {
  return { root, path, writable: true, entries, truncated: false, cursor: null };
}

async function renderPage(roots: RootsResponse = ROOTS) {
  getFileRoots.mockResolvedValue(roots);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  render(
    <QueryClientProvider client={client}>
      <UploadsProvider>
        <FilesPage />
        <TransferTray />
      </UploadsProvider>
    </QueryClientProvider>,
  );
  expect(await screen.findByText("Files")).toBeTruthy();
}

/** A dropped payload, in the shape the browser hands the row. */
function dropOf(...files: File[]) {
  return { files, types: ["Files"], items: [] };
}

/** Open Home and wait for its listing. */
async function openHome() {
  await userEvent.click(screen.getByLabelText("Expand Home"));
  // A root row carries a ⋮ too — it is a folder you can upload into — so this
  // waits for more than one rather than for exactly one.
  await waitFor(() =>
    expect(screen.getAllByRole("button", { name: /Actions for/ }).length).toBeGreaterThan(1),
  );
}

describe("the file tree", () => {
  it("opens on the places, and asks the device for nothing else", async () => {
    await renderPage();
    expect(screen.getByText("Home")).toBeTruthy();
    expect(screen.getByText("KINGSTON")).toBeTruthy();
    // Nothing is expanded, so nothing has been listed. A tree that read every
    // root at startup would spin up a removable drive nobody opened.
    expect(listDirectory).not.toHaveBeenCalled();
  });

  it("lists a place when it is opened, and nests what is inside", async () => {
    await renderPage();
    listDirectory.mockImplementation(async (root: string, path: string) => {
      if (path === "") return listing(root, "", [folder("Books"), file("notes.txt")]);
      return listing(root, path, [file("hobbit.epub")]);
    });

    await userEvent.click(screen.getByLabelText("Expand Home"));
    expect(await screen.findByText("Books")).toBeTruthy();
    expect(screen.getByText("notes.txt")).toBeTruthy();
    expect(listDirectory).toHaveBeenCalledWith("home", "", expect.anything());

    await userEvent.click(screen.getByLabelText("Expand Books"));
    expect(await screen.findByText("hobbit.epub")).toBeTruthy();
    expect(listDirectory).toHaveBeenCalledWith("home", "Books", expect.anything());
    // The drive was never touched.
    expect(
      listDirectory.mock.calls.some(([root]) => root === "ext-9f2a1c04"),
    ).toBe(false);
  });

  it("stops showing a folder's contents when it is closed", async () => {
    await renderPage();
    listDirectory.mockResolvedValue(listing("home", "", [file("notes.txt")]));

    await userEvent.click(screen.getByLabelText("Expand Home"));
    expect(await screen.findByText("notes.txt")).toBeTruthy();
    await userEvent.click(screen.getByLabelText("Collapse Home"));
    await waitFor(() => expect(screen.queryByText("notes.txt")).toBeNull());
  });

  it("hides dotfiles until asked, and then shows them", async () => {
    await renderPage();
    listDirectory.mockResolvedValue(
      listing("home", "", [file("notes.txt"), file(".pam_environment")]),
    );

    await userEvent.click(screen.getByLabelText("Expand Home"));
    expect(await screen.findByText("notes.txt")).toBeTruthy();
    expect(screen.queryByText(".pam_environment")).toBeNull();

    await userEvent.click(screen.getByRole("switch", { name: /show hidden/i }));
    expect(await screen.findByText(".pam_environment")).toBeTruthy();
  });

  it("shows a link that points out of its place, and will not open it", async () => {
    await renderPage();
    listDirectory.mockResolvedValue(
      listing("home", "", [
        folder("escape", { symlink: true, unusable: "symlink_escapes", writable: undefined }),
      ]),
    );

    await userEvent.click(screen.getByLabelText("Expand Home"));
    // Listed rather than hidden: it is on the disk, and seeing it is how
    // somebody gets rid of it.
    expect(await screen.findByText("escape")).toBeTruthy();
    expect(screen.queryByLabelText("Expand escape")).toBeNull();
  });

  it("says what went wrong without closing the folder", async () => {
    await renderPage();
    listDirectory.mockRejectedValue(new Error("the drive went away"));

    await userEvent.click(screen.getByLabelText("Expand Home"));
    expect(await screen.findByText(/the drive went away/)).toBeTruthy();
    // Still open, so a retry is one click rather than a re-navigation.
    expect(screen.getByLabelText("Collapse Home")).toBeTruthy();
  });

  it("downloads a file from its ⋮ menu", async () => {
    await renderPage();
    listDirectory.mockResolvedValue(listing("home", "", [file("hobbit.epub")]));
    downloadFile.mockResolvedValue(undefined);

    await userEvent.click(screen.getByLabelText("Expand Home"));
    await userEvent.click(await screen.findByLabelText("Actions for hobbit.epub"));
    await userEvent.click(screen.getByText("Download"));

    expect(downloadFile).toHaveBeenCalledWith("home", "hobbit.epub", "hobbit.epub");
  });

  it("folds the columns into the name on a phone", async () => {
    viewport("phone");
    await renderPage();
    listDirectory.mockResolvedValue(listing("home", "", [file("hobbit.epub")]));

    await userEvent.click(screen.getByLabelText("Expand Home"));
    expect(await screen.findByText("hobbit.epub")).toBeTruthy();
    // No sortable headers to tap at this width; the size and date ride under
    // the name instead.
    expect(screen.queryByText("Kind")).toBeNull();
    // Locale decides whether the date reads "Aug 30" or "30 Aug"; what this
    // asserts is that both facts ride under the name rather than in columns.
    expect(screen.getByText(/1\.9 MB · .*Aug/)).toBeTruthy();
  });

  it("sorts by a column when its header is clicked", async () => {
    await renderPage();
    listDirectory.mockResolvedValue(
      listing("home", "", [
        file("small.txt", { size: 10 }),
        file("large.bin", { size: 9_000_000 }),
      ]),
    );

    await userEvent.click(screen.getByLabelText("Expand Home"));
    expect(await screen.findByText("large.bin")).toBeTruthy();

    const names = () =>
      screen
        .getAllByRole("row")
        .map((row) => row.textContent ?? "")
        .filter((text) => text.includes(".txt") || text.includes(".bin"));

    // Ascending by name to start with.
    expect(names()[0]).toContain("large.bin");
    await userEvent.click(screen.getByText("Size"));
    expect(names()[0]).toContain("small.txt");
    await userEvent.click(screen.getByText("Size"));
    expect(names()[0]).toContain("large.bin");
  });

  // -------------------------------------------------------------------------
  // Writes
  // -------------------------------------------------------------------------

  it("uploads a file dropped onto a folder, and will not clobber with it", async () => {
    await renderPage();
    listDirectory.mockResolvedValue(listing("home", "", [folder("Books")]));
    uploadFile.mockResolvedValue({ path: "Books/new.epub", size: 4, etag: "4-1" });

    await openHome();
    const row = screen.getByText("Books").closest("tr")!;
    fireEvent.drop(row, {
      dataTransfer: dropOf(new File(["book"], "new.epub")),
    });

    await waitFor(() => expect(uploadFile).toHaveBeenCalled());
    const [root, path, , precondition] = uploadFile.mock.calls[0];
    expect([root, path]).toEqual(["home", "Books/new.epub"]);
    // Create, not replace: an upload that silently overwrote a book somebody
    // else put there is the one thing this must not do.
    expect(precondition).toEqual({ kind: "create" });
  });

  it("refuses a dropped folder with a sentence rather than an empty file", async () => {
    await renderPage();
    listDirectory.mockResolvedValue(listing("home", "", [folder("Books")]));

    await openHome();
    const row = screen.getByText("Books").closest("tr")!;
    fireEvent.drop(row, {
      dataTransfer: {
        types: ["Files"],
        files: [],
        items: [
          {
            kind: "file",
            webkitGetAsEntry: () => ({ isDirectory: true, name: "covers" }),
            getAsFile: () => null,
          },
        ],
      },
    });

    expect(await screen.findByText(/Dropping a folder is not supported/)).toBeTruthy();
    expect(uploadFile).not.toHaveBeenCalled();
  });

  it("asks before replacing, and only then sends with force", async () => {
    await renderPage();
    listDirectory.mockResolvedValue(listing("home", "", [folder("Books")]));
    uploadFile.mockRejectedValueOnce(
      new ApiError(412, "precondition_failed", "Something is already there"),
    );

    await openHome();
    fireEvent.drop(screen.getByText("Books").closest("tr")!, {
      dataTransfer: dropOf(new File(["book"], "hobbit.epub")),
    });

    // The tray asks rather than failing: the file is there, the question is
    // whether this one should win.
    const replace = await screen.findByRole("button", { name: "Replace" });
    uploadFile.mockResolvedValueOnce({ path: "x", size: 4, etag: "4-1" });
    await userEvent.click(replace);

    await waitFor(() => expect(uploadFile).toHaveBeenCalledTimes(2));
    expect(uploadFile.mock.calls[1][3]).toEqual({ kind: "force" });
  });

  it("refuses a file bigger than the device accepts, before sending a byte", async () => {
    await renderPage({
      ...ROOTS,
      limits: { max_upload_bytes: 8, free_space_floor_bytes: 0 },
    });
    listDirectory.mockResolvedValue(listing("home", "", [folder("Books")]));

    await openHome();
    fireEvent.drop(screen.getByText("Books").closest("tr")!, {
      dataTransfer: dropOf(new File(["far too many bytes"], "big.bin")),
    });

    expect(await screen.findByText(/larger than this device accepts/)).toBeTruthy();
    expect(uploadFile).not.toHaveBeenCalled();
  });

  it("creates a folder where the ⋮ menu was opened", async () => {
    await renderPage();
    listDirectory.mockResolvedValue(listing("home", "", [folder("Books")]));
    createDirectory.mockResolvedValue(undefined);

    await openHome();
    await userEvent.click(screen.getByLabelText("Actions for Books"));
    await userEvent.click(screen.getByText("New folder…"));
    await userEvent.type(screen.getByLabelText("Name"), "covers");
    await userEvent.click(screen.getByRole("button", { name: "Create" }));

    await waitFor(() => expect(createDirectory).toHaveBeenCalledWith("home", "Books/covers"));
  });

  it("renames in place, keeping the file in its folder", async () => {
    await renderPage();
    listDirectory.mockResolvedValue(listing("home", "Books", [file("hobbit.epub")]));
    moveEntry.mockResolvedValue(undefined);

    await openHome();
    await userEvent.click(screen.getByLabelText("Actions for hobbit.epub"));
    await userEvent.click(screen.getByText("Rename"));

    const field = await screen.findByLabelText("New name for hobbit.epub");
    await userEvent.clear(field);
    await userEvent.type(field, "the-hobbit.epub{Enter}");

    await waitFor(() =>
      expect(moveEntry).toHaveBeenCalledWith(
        "home",
        "hobbit.epub",
        "the-hobbit.epub",
        false,
        // No handle: an ordinary name says which file it means.
        undefined,
      ),
    );
  });

  it("deletes a file against the version the row was drawn with", async () => {
    await renderPage();
    listDirectory.mockResolvedValue(listing("home", "", [file("hobbit.epub")]));
    deleteEntry.mockResolvedValue(undefined);

    await openHome();
    await userEvent.click(screen.getByLabelText("Actions for hobbit.epub"));
    await userEvent.click(screen.getByText("Delete…"));
    await userEvent.click(screen.getByRole("button", { name: "Delete" }));

    await waitFor(() =>
      expect(deleteEntry).toHaveBeenCalledWith(
        "home",
        "hobbit.epub",
        { kind: "replace", etag: "1863410-1756557164123456789" },
        false,
        // No handle: an ordinary name addresses its own file, and `path` is
        // the entry rather than the folder it sits in.
        undefined,
      ),
    );
  });

  it("deletes a file whose name is not text, by the handle it was given", async () => {
    await renderPage();
    // What a FAT drive mounted with the wrong charset hands back: the name is
    // a lossy rendering that addresses nothing, so every other action is off
    // and this one goes by the bytes instead.
    const broken = file("caf\uFFFD.mp3", {
      unusable: "name_not_utf8",
      handle: "636166e92e6d7033",
    });
    listDirectory.mockImplementation(async (root: string, path: string) =>
      path === ""
        ? listing(root, "", [folder("Books")])
        : listing(root, path, [broken]),
    );
    deleteEntry.mockResolvedValue(undefined);

    await openHome();
    await userEvent.click(screen.getByLabelText("Expand Books"));
    expect(await screen.findByText("caf\uFFFD.mp3")).toBeTruthy();

    await userEvent.click(screen.getByLabelText("Actions for caf\uFFFD.mp3"));
    await userEvent.click(screen.getByText("Delete…"));
    await userEvent.click(screen.getByRole("button", { name: "Delete" }));

    await waitFor(() =>
      expect(deleteEntry).toHaveBeenCalledWith(
        "home",
        // The *folder*, not the entry: the handle supplies the last part.
        "Books",
        { kind: "replace", etag: "1863410-1756557164123456789" },
        false,
        "636166e92e6d7033",
      ),
    );
  });

  it("renames a file whose name is not text, by the handle it was given", async () => {
    await renderPage();
    const broken = file("caf\uFFFD.mp3", {
      unusable: "name_not_utf8",
      handle: "636166e92e6d7033",
    });
    listDirectory.mockImplementation(async (root: string, path: string) =>
      path === ""
        ? listing(root, "", [folder("Books")])
        : listing(root, path, [broken]),
    );
    moveEntry.mockResolvedValue(undefined);

    await openHome();
    await userEvent.click(screen.getByLabelText("Expand Books"));
    expect(await screen.findByText("caf\uFFFD.mp3")).toBeTruthy();

    // Offered at all, which is the point: this is the repair, and before the
    // handle existed the only thing on offer for such a file was deleting it.
    await userEvent.click(screen.getByLabelText("Actions for caf\uFFFD.mp3"));
    await userEvent.click(screen.getByText("Rename"));
    const field = await screen.findByRole("textbox");
    await userEvent.clear(field);
    await userEvent.type(field, "cafe.mp3{Enter}");

    await waitFor(() =>
      expect(moveEntry).toHaveBeenCalledWith(
        "home",
        // The folder on both sides; the handle supplies the source's name and
        // the typed text supplies the destination's.
        "Books",
        "Books/cafe.mp3",
        false,
        "636166e92e6d7033",
      ),
    );
  });

  it("says what a recursive delete means, and asks for one", async () => {
    await renderPage();
    listDirectory.mockResolvedValue(listing("home", "", [folder("Books")]));
    deleteEntry.mockResolvedValue(undefined);

    await openHome();
    await userEvent.click(screen.getByLabelText("Actions for Books"));
    await userEvent.click(screen.getByText("Delete…"));
    expect(screen.getByText(/everything in it/)).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Delete" }));

    await waitFor(() =>
      // A folder has no version to match, so it goes with `If-Match: *`.
      expect(deleteEntry).toHaveBeenCalledWith(
        "home",
        "Books",
        { kind: "force" },
        true,
        undefined,
      ),
    );
  });

  it("offers nothing that would fail on a read-only place", async () => {
    await renderPage({
      ...ROOTS,
      roots: [{ ...ROOTS.roots[0], writable: false }],
    });
    listDirectory.mockResolvedValue({
      ...listing("home", "", [folder("Books", { writable: false })]),
      writable: false,
    });

    await openHome();
    await userEvent.click(screen.getByLabelText("Actions for Books"));
    for (const label of ["Upload files here…", "New folder…", "Rename", "Delete…"]) {
      expect(screen.getByText(label).closest("li")).toHaveProperty(
        "ariaDisabled",
        "true",
      );
    }
  });

  it("moves a file through the ⋮ menu, for anybody who cannot drag", async () => {
    await renderPage();
    listDirectory.mockImplementation(async (root: string, path: string) => {
      if (path === "") return listing(root, "", [folder("Books"), file("hobbit.epub")]);
      return listing(root, path, []);
    });
    moveEntry.mockResolvedValue(undefined);

    await openHome();
    await userEvent.click(screen.getByLabelText("Actions for hobbit.epub"));
    await userEvent.click(screen.getByText("Move to…"));

    // The picker is the same tree, folders only and one place only — a move
    // cannot cross roots, so the drive is not offered.
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).queryByText("KINGSTON")).toBeNull();
    expect(within(dialog).queryByText("hobbit.epub")).toBeNull();

    await userEvent.click(within(dialog).getByLabelText("Expand Home"));
    await userEvent.click(await within(dialog).findByText("Books"));
    await userEvent.click(within(dialog).getByRole("button", { name: "Move" }));

    await waitFor(() =>
      expect(moveEntry).toHaveBeenCalledWith(
        "home",
        "hobbit.epub",
        "Books/hobbit.epub",
        false,
        // No handle: an ordinary name travels as itself.
        undefined,
      ),
    );
  });

  it("moves a file whose name is not text, once the loss is accepted", async () => {
    await renderPage();
    const broken = file("caf\uFFFD.mp3", {
      unusable: "name_not_utf8",
      handle: "636166e92e6d7033",
    });
    listDirectory.mockImplementation(async (root: string, path: string) =>
      path === "" ? listing(root, "", [folder("Books"), broken]) : listing(root, path, []),
    );
    moveEntry.mockResolvedValue(undefined);
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);

    await openHome();
    await userEvent.click(screen.getByLabelText("Actions for caf\uFFFD.mp3"));
    await userEvent.click(screen.getByText("Move to…"));
    const dialog = await screen.findByRole("dialog");
    await userEvent.click(within(dialog).getByLabelText("Expand Home"));
    await userEvent.click(await within(dialog).findByText("Books"));
    await userEvent.click(within(dialog).getByRole("button", { name: "Move" }));

    // Asked first, and in words that say what is lost -- the two names render
    // identically, so nothing about the result would show it.
    expect(confirm).toHaveBeenCalledWith(expect.stringContaining("will be lost"));
    await waitFor(() =>
      expect(moveEntry).toHaveBeenCalledWith(
        "home",
        // The folder, with the handle naming the entry in it.
        "",
        "Books/caf\uFFFD.mp3",
        false,
        "636166e92e6d7033",
      ),
    );
    confirm.mockRestore();
  });

  it("does not move it when the loss is refused", async () => {
    await renderPage();
    const broken = file("caf\uFFFD.mp3", {
      unusable: "name_not_utf8",
      handle: "636166e92e6d7033",
    });
    listDirectory.mockImplementation(async (root: string, path: string) =>
      path === "" ? listing(root, "", [folder("Books"), broken]) : listing(root, path, []),
    );
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);

    await openHome();
    await userEvent.click(screen.getByLabelText("Actions for caf\uFFFD.mp3"));
    await userEvent.click(screen.getByText("Move to…"));
    const dialog = await screen.findByRole("dialog");
    await userEvent.click(within(dialog).getByLabelText("Expand Home"));
    await userEvent.click(await within(dialog).findByText("Books"));
    await userEvent.click(within(dialog).getByRole("button", { name: "Move" }));

    expect(confirm).toHaveBeenCalled();
    expect(moveEntry).not.toHaveBeenCalled();
    confirm.mockRestore();
  });

  it("will not offer a destination the move would be refused from", async () => {
    await renderPage();
    listDirectory.mockImplementation(async (root: string, path: string) => {
      if (path === "") return listing(root, "", [folder("Books")]);
      return listing(root, path, [folder("covers")]);
    });

    await openHome();
    await userEvent.click(screen.getByLabelText("Actions for Books"));
    await userEvent.click(screen.getByText("Move to…"));

    const dialog = await screen.findByRole("dialog");
    await userEvent.click(within(dialog).getByLabelText("Expand Home"));
    await userEvent.click(await within(dialog).findByText("Books"));
    // Its own folder: nothing to do, and the button says so by being off.
    expect(within(dialog).getByText(/already is/)).toBeTruthy();
    expect(within(dialog).getByRole("button", { name: "Move" })).toHaveProperty(
      "disabled",
      true,
    );
  });

  /**
   * dnd-kit puts `role="button"` and a `tabIndex` on whatever it makes
   * draggable. On a `<tr>` inside a `treegrid` both are wrong, and stripping
   * them is the kind of thing a dependency bump silently undoes.
   */
  it("keeps the treegrid semantics under the drag handles", async () => {
    await renderPage();
    listDirectory.mockResolvedValue(listing("home", "", [file("hobbit.epub")]));

    await openHome();
    const row = screen.getByText("hobbit.epub").closest("tr")!;
    expect(row.getAttribute("role")).toBeNull();
    expect(row.getAttribute("tabindex")).toBeNull();
    expect(screen.getByRole("treegrid")).toBeTruthy();
    expect(screen.getAllByRole("row").length).toBeGreaterThan(1);
  });
});
