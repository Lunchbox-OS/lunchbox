// @vitest-environment jsdom
/**
 * The HUD-placement controls, in a DOM (issue #171).
 *
 * Same reason as the SponsorBlock suite: the schema and the generated types
 * carried `service.hud` and `hud_orientation` before the editor did, and a
 * field only reachable by hand-editing TOML is a field this editor is failing
 * at. What is worth checking is not that the menus exist but what they *write*
 * — specifically the two cases where the obvious implementation is wrong:
 *
 * - Clearing the device setting has to remove the whole `[service.hud]` table,
 *   not leave an empty one behind. The daemon reads "top" either way, so the
 *   difference is invisible to it and very visible in a file people annotate.
 * - The per-activity menu has to be able to say *inherit* as a state distinct
 *   from explicitly choosing the edge the device already uses — because the
 *   two behave differently the moment the device setting changes.
 *
 * The wasm-backed document is stubbed, as in `navigation.test.tsx`: these
 * assertions are about the patches the controls emit, not about how the
 * document applies them.
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

const { ServicePage } = await import("./pages/ServicePage");

/** The value a `set` patch carries for `path`, or undefined if none was made. */
const setValue = (path: string) =>
  apply.mock.calls.find(([p]) => p?.op === "set" && p?.path === path)?.[0]
    ?.value;

/** Whether an `unset` patch was made for `path`. */
const wasUnset = (path: string) =>
  apply.mock.calls.some(([p]) => p?.op === "unset" && p?.path === path);

/** Open a `Section` — its children are unmounted while it is collapsed, so
 * nothing inside can be queried until it is open. */
const openSection = async (title: string) => {
  const expand = screen.queryByRole("button", { name: `Expand ${title}` });
  if (expand) await userEvent.click(expand);
};

/** The text a MUI select currently shows. `toHaveTextContent` would be
 * neater, but this project does not install the jest-dom matchers. */
const shown = (label: string) =>
  screen.getByRole("combobox", { name: label }).textContent;

/** Pick `option` from the MUI select labelled `label`. MUI renders a listbox
 * rather than a native `<select>`, so this is a click on the control followed
 * by a click on the option. */
const choose = async (label: string, option: string) => {
  await userEvent.click(screen.getByRole("combobox", { name: label }));
  await userEvent.click(screen.getByRole("option", { name: option }));
};

describe("the device-wide HUD edge", () => {
  beforeEach(() => apply.mockClear());
  afterEach(cleanup);

  it("reads as the default when no table is configured", async () => {
    render(<ServicePage config={{ config_version: 1 } as RawConfig} />);
    await openSection("HUD");
    expect(shown("HUD edge")).toBe("Top (the default)");
  });

  it("shows the configured edge", async () => {
    render(
      <ServicePage
        config={
          {
            config_version: 1,
            service: { hud: { orientation: "left" } },
          } as RawConfig
        }
      />,
    );
    await openSection("HUD");
    expect(shown("HUD edge")).toBe("Left (vertical)");
  });

  it("writes the edge that was picked", async () => {
    render(<ServicePage config={{ config_version: 1 } as RawConfig} />);
    await openSection("HUD");
    await choose("HUD edge", "Left (vertical)");
    expect(setValue("service.hud.orientation")).toBe("left");
  });

  // The table is the unit that goes, not the key: `[service.hud]` with nothing
  // under it means exactly what no table means, and only one of the two is
  // worth leaving in a file somebody reads.
  it("removes the whole table rather than emptying it", async () => {
    render(
      <ServicePage
        config={
          {
            config_version: 1,
            service: { hud: { orientation: "left" } },
          } as RawConfig
        }
      />,
    );
    await openSection("HUD");
    await choose("HUD edge", "Top (the default)");
    expect(wasUnset("service.hud")).toBe(true);
    expect(setValue("service.hud.orientation")).toBeUndefined();
  });
});

const { EntryDetail } = await import("./components/EntryDetail");

const entry = (extra: Partial<RawEntry> = {}) =>
  ({
    id: "probe",
    label: "Probe",
    kind: { type: "process", command: "/usr/bin/true" },
    ...extra,
  }) as RawEntry;

const withEntry = (e: RawEntry) =>
  ({ config_version: 1, entries: [e] }) as RawConfig;

/** Render an entry and get to its HUD control: the Behaviour section is on the
 * Advanced tab, and neither an unselected tab nor a collapsed section mounts
 * its children. */
const openBehaviour = async (e: RawEntry) => {
  render(<EntryDetail config={withEntry(e)} entry={e} />);
  await userEvent.click(screen.getByRole("tab", { name: "Advanced" }));
  await openSection("Behaviour");
};

describe("an activity's own HUD edge", () => {
  beforeEach(() => apply.mockClear());
  afterEach(cleanup);

  it("inherits the device setting until it is given one", async () => {
    await openBehaviour(entry());
    expect(shown("HUD edge while this runs")).toBe("Use the device setting");
  });

  it("writes the edge that was picked", async () => {
    await openBehaviour(entry());
    await choose("HUD edge while this runs", "Left (vertical)");
    expect(setValue("entries[id=probe].hud_orientation")).toBe("left");
  });

  // "Inherit" and "top" are different answers: an activity pinned to top keeps
  // the top bar when the device moves to a side bar, and an inheriting one
  // follows. Clearing therefore has to unset the key, not write "top".
  it("goes back to inheriting rather than writing the default", async () => {
    await openBehaviour(entry({ hud_orientation: "left" }));
    await choose("HUD edge while this runs", "Use the device setting");
    expect(wasUnset("entries[id=probe].hud_orientation")).toBe(true);
    expect(setValue("entries[id=probe].hud_orientation")).toBeUndefined();
  });
});
