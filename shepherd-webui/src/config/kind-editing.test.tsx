// @vitest-environment jsdom
/**
 * What editing an activity's kind writes, in a DOM (issue #192).
 *
 * The form is built on the view, where serde spells every unset option as
 * `null`, and `KindEditor` hands the whole kind back. Written back whole, a
 * kind stored as a standard `[entries.kind]` table failed on the first null —
 * so typing a book's path raised "cannot write null; use unset" and changed
 * nothing. These check that a keystroke writes the one field it touched.
 *
 * The wasm-backed document is stubbed, as in `navigation.test.tsx`; how the
 * document applies a whole kind is covered in `tests/preservation.rs`.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { RawConfig, RawEntry } from "./model/config.generated";

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

/** The Hobbit, as the view hands it over. */
const hobbit = (kind: object = {}) =>
  ({
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
      ...kind,
    },
  }) as RawEntry;

const show = (e: RawEntry) =>
  render(
    <EntryDetail config={{ config_version: 1, entries: [e] } as RawConfig} entry={e} />,
  );

const patches = () => apply.mock.calls.map(([p]) => p);

describe("editing a book", () => {
  beforeEach(() => apply.mockClear());
  afterEach(cleanup);

  it("writes the path, and nothing else, as it is typed", async () => {
    show(hobbit());
    await userEvent.type(screen.getByRole("textbox", { name: /^Book/ }), "x");
    expect(patches()).toEqual([
      {
        op: "set",
        path: "entries[id=the-hobbit].kind.book",
        value: "~/Books/the-hobbit.epubx",
      },
    ]);
  });

  it("removes the starting page when it is cleared", async () => {
    show(hobbit({ open_at: 12 }));
    await userEvent.clear(
      screen.getByRole("spinbutton", { name: "Open at page (optional)" }),
    );
    expect(patches()).toEqual([
      { op: "unset", path: "entries[id=the-hobbit].kind.open_at" },
    ]);
  });
});

// The projection these forms are controlled by is re-derived 120ms after an
// edit, and the stub never re-derives it at all — so the prop stays exactly
// where it started, which is what makes the draft visible to a test.
describe("typing into a path field", () => {
  beforeEach(() => apply.mockClear());
  afterEach(cleanup);

  it("shows the character without waiting for the document", async () => {
    show(hobbit());
    const field = screen.getByRole("textbox", { name: /^Book/ }) as HTMLInputElement;
    await userEvent.type(field, "xyz");
    expect(field.value).toBe("~/Books/the-hobbit.epubxyz");
  });

  it("goes back to what the document says once it is left", async () => {
    show(hobbit());
    const field = screen.getByRole("textbox", { name: /^Book/ }) as HTMLInputElement;
    await userEvent.type(field, "xyz");
    await userEvent.tab();
    // In the app the projection has caught up by now and says "…epubxyz"; here
    // it never moves, so this is the draft being dropped rather than kept.
    expect(field.value).toBe("~/Books/the-hobbit.epub");
  });
});

describe("a change that touches two fields", () => {
  beforeEach(() => apply.mockClear());
  afterEach(cleanup);

  it("is one undo step", async () => {
    const e = {
      id: "firered",
      label: "FireRed",
      kind: {
        type: "retroarch",
        core: "mgba",
        core_path: null,
        content: "~/Games/firered.gba",
      },
    } as RawEntry;
    show(e);
    await userEvent.click(screen.getByRole("combobox", { name: "Core" }));
    await userEvent.click(screen.getByRole("option", { name: "By path" }));

    expect(patches()).toEqual([
      { op: "unset", path: "entries[id=firered].kind.core" },
      { op: "set", path: "entries[id=firered].kind.core_path", value: "" },
    ]);
    const keys = apply.mock.calls.map(([, key]) => key);
    expect(keys[0]).toBeDefined();
    expect(keys[1]).toBe(keys[0]);
    expect(doc.endGesture).toHaveBeenCalled();
  });
});

const { EntriesPage } = await import("./pages/EntriesPage");

describe("adding an activity", () => {
  beforeEach(() => apply.mockClear());
  afterEach(cleanup);

  const openDialog = async () => {
    render(<EntriesPage config={{ config_version: 1, entries: [] } as RawConfig} />);
    await userEvent.click(screen.getByRole("button", { name: "Add activity" }));
  };

  // The dialog offered five of the nine, so a book, an emulated game, a VM or
  // a custom kind could not be created at all — only made by hand in the TOML
  // and then edited here.
  it("offers every kind the editor knows", async () => {
    await openDialog();
    await userEvent.click(screen.getByRole("combobox", { name: "Type" }));
    expect(screen.getAllByRole("option").map((o) => o.textContent)).toEqual([
      "Program",
      "Snap",
      "Steam game",
      "Flatpak",
      "Virtual machine",
      "Media library",
      "Emulated game",
      "Book",
      "Custom",
    ]);
  });

  it("creates the kind it was asked for, with the fields the schema requires", async () => {
    await openDialog();
    await userEvent.type(screen.getByRole("textbox", { name: /^Label/ }), "The Hobbit");
    await userEvent.click(screen.getByRole("combobox", { name: "Type" }));
    await userEvent.click(screen.getByRole("option", { name: "Book" }));
    await userEvent.click(screen.getByRole("button", { name: "Add" }));

    expect(apply).toHaveBeenCalledWith({
      op: "insert",
      path: "entries",
      value: {
        id: "the-hobbit",
        label: "The Hobbit",
        kind: { type: "ebook", book: "", args: [], env: {} },
      },
    });
  });

  // A media activity was built with `library_id`, which the schema has never
  // had: the entry parsed as missing its required `library` instead.
  it("builds a media library on the field the schema actually has", async () => {
    await openDialog();
    await userEvent.type(screen.getByRole("textbox", { name: /^Label/ }), "Films");
    await userEvent.click(screen.getByRole("combobox", { name: "Type" }));
    await userEvent.click(screen.getByRole("option", { name: "Media library" }));
    await userEvent.click(screen.getByRole("button", { name: "Add" }));

    const calls = apply.mock.calls;
    const value = calls[calls.length - 1]?.[0]?.value as { kind: object };
    expect(value.kind).toEqual({ type: "media", library: "" });
  });
});

const { KIND_LABELS, blankKind } = await import("./model/kinds");

describe("every kind", () => {
  it("has a blank shape carrying its required fields", () => {
    for (const type of Object.keys(KIND_LABELS) as (keyof typeof KIND_LABELS)[]) {
      const blank = blankKind(type) as Record<string, unknown>;
      expect(blank.type).toBe(type);
      for (const [key, v] of Object.entries(blank)) {
        expect(v, `${type}.${key}`).toBeDefined();
      }
    }
  });
});
