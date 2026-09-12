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
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { DirEntryInfo, Listing, RootsResponse } from "../files/types";

const getFileRoots = vi.fn();
const listDirectory = vi.fn();
const downloadFile = vi.fn();

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
  };
});

const { FilesPage } = await import("./FilesPage");

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

async function renderPage() {
  getFileRoots.mockResolvedValue(ROOTS);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  render(
    <QueryClientProvider client={client}>
      <FilesPage />
    </QueryClientProvider>,
  );
  expect(await screen.findByText("Files")).toBeTruthy();
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
});
