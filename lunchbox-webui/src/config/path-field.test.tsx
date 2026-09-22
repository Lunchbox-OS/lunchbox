// @vitest-environment jsdom
/**
 * The browse button, and the rule that it is only ever an accelerator
 * (issue #186).
 *
 * The two halves worth pinning down are what the picker is *asked* — a kind, a
 * noun, and where the field points now — and what happens either side of it:
 * nothing at all without a picker in context, and nothing to the field when
 * somebody cancels. A cancel that cleared a ROM's path would be a data loss
 * dressed as a no-op.
 *
 * The document is stubbed as in `kind-editing.test.tsx`; this is about the
 * field, not about what the document does with the patch.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { RawConfig, RawEntry } from "./model/config.generated";
import type { FilePicker, PickRequest } from "./pick/FilePicker";

const apply = vi.fn();

const doc = {
  ready: true,
  loadError: null,
  versions: { config_version: 1, crate_version: "9.9.9" },
  view: {} as RawConfig,
  report: { kind: "semantic" as const, errors: [] },
  text: "config_version = 1\n",
  document: { text: "config_version = 1\n", name: null },
  dirty: false,
  apply,
  endGesture: vi.fn(),
  replaceText: vi.fn(() => null),
  undo: vi.fn(),
  redo: vi.fn(),
  canUndo: false,
  canRedo: false,
  availabilityFor: () => null,
  openFrom: vi.fn(),
  save: vi.fn(),
  startBlank: vi.fn(),
  error: null,
  clearError: vi.fn(),
};

vi.mock("./doc/ConfigDocProvider", () => ({
  ConfigDocProvider: ({ children }: { children: React.ReactNode }) => children,
  useConfigDoc: () => doc,
}));

const { EntryDetail } = await import("./components/EntryDetail");
const { FilePickerProvider } = await import("./pick/FilePicker");

const hobbit = {
  id: "the-hobbit",
  label: "The Hobbit",
  kind: {
    type: "ebook",
    book: "~/Books/the-hobbit.epub",
    viewer: "okular",
    open_at: null,
    layout: "facing_first_centered",
    font_size: 16,
    font_family: "Noto Serif",
    command: null,
    args: [],
    env: {},
    kiosk: true,
  },
} as RawEntry;

const show = (picker: FilePicker | null) =>
  render(
    <FilePickerProvider picker={picker}>
      <EntryDetail
        config={{ config_version: 1, entries: [hobbit] } as RawConfig}
        entry={hobbit}
      />
    </FilePickerProvider>,
  );

const patches = () => apply.mock.calls.map(([p]) => p);

describe("a path field with no picker", () => {
  beforeEach(() => apply.mockClear());
  afterEach(cleanup);

  it("offers no browse button", () => {
    show(null);
    expect(screen.queryByRole("button", { name: /Browse/ })).toBeNull();
  });

  it("still takes a typed path", async () => {
    show(null);
    await userEvent.type(screen.getByRole("textbox", { name: /^Book/ }), "x");
    expect(patches()).toEqual([
      {
        op: "set",
        path: "entries[id=the-hobbit].kind.book",
        value: "~/Books/the-hobbit.epubx",
      },
    ]);
  });
});

describe("a path field with a picker", () => {
  let asked: PickRequest[] = [];

  const picker = (answer: string | null): FilePicker => ({
    pick: (request) => {
      asked.push(request);
      return Promise.resolve(answer);
    },
  });

  beforeEach(() => {
    apply.mockClear();
    asked = [];
  });
  afterEach(cleanup);

  it("asks for what the field holds, starting from where it points", async () => {
    show(picker(null));
    await userEvent.click(screen.getByRole("button", { name: "Browse for a book" }));
    expect(asked).toEqual([
      { kind: "file", what: "a book", start: "~/Books/the-hobbit.epub" },
    ]);
  });

  it("writes what came back", async () => {
    show(picker("~/Books/redwall.epub"));
    await userEvent.click(screen.getByRole("button", { name: "Browse for a book" }));
    expect(patches()).toEqual([
      {
        op: "set",
        path: "entries[id=the-hobbit].kind.book",
        value: "~/Books/redwall.epub",
      },
    ]);
  });

  it("leaves the field alone when the dialog is cancelled", async () => {
    show(picker(null));
    await userEvent.click(screen.getByRole("button", { name: "Browse for a book" }));
    expect(patches()).toEqual([]);
  });
});
