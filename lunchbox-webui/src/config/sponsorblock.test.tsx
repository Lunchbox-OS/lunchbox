// @vitest-environment jsdom
/**
 * The SponsorBlock controls, in a DOM (issue #159).
 *
 * These exist because the config landed before the editor did: the schema and
 * the generated types carried `service.media.sponsorblock` for a while with no
 * way to reach it except hand-editing TOML, which is the thing this editor is
 * for. What is worth checking is not that the fields exist but what they *write*
 * — a category checkbox has to produce the whole list, not a diff, and the
 * per-entry override has to be able to say "inherit" as distinct from "off".
 *
 * The wasm-backed document is stubbed, as in `navigation.test.tsx`: these
 * assertions are about the patches the controls emit, not about how the
 * document applies them.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { RawConfig } from "./model/config.generated";

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

/** Open the Media section if it is closed — its children are unmounted while it
 * is, so nothing inside can be queried until it is open. */
const openMedia = async () => {
  const expand = screen.queryByRole("button", { name: "Expand Media" });
  if (expand) await userEvent.click(expand);
};
const { KindEditor } = await import("./components/KindEditor");

/** The value a `set` patch carries for `path`, or undefined if none was made. */
const patched = (path: string) => {
  const call = apply.mock.calls.find(([p]) => p?.path === path);
  return call?.[0]?.value;
};

/** A config carrying `[service.media.sponsorblock]`. The Media section renders
 * its contents once the table is there, which is the editor's own convention
 * for optional sections. */
const service = (sponsorblock: object) =>
  ({ config_version: 1, service: { media: { sponsorblock } } }) as RawConfig;

describe("the SponsorBlock service settings", () => {
  beforeEach(() => apply.mockClear());
  afterEach(cleanup);

  // Off is the whole privacy claim, so the editor must show it as off and keep
  // the details out of the way until somebody turns it on.
  it("is off, and hides its details, until it is turned on", async () => {
    render(<ServicePage config={service({ enabled: false })} />);
    await openMedia();
    // By label rather than by role: MUI's Switch takes its accessible name from
    // the FormControlLabel wrapping it, not from an aria-label of its own.
    const toggle = screen.getByLabelText(
      "Skip sponsored spans in YouTube videos",
    ) as HTMLInputElement;
    expect(toggle.checked).toBe(false);
    expect(screen.queryByRole("checkbox", { name: "Sponsor" })).toBeNull();
  });

  it("turns the feature on", async () => {
    render(<ServicePage config={service({ enabled: false })} />);
    await openMedia();
    await userEvent.click(
      screen.getByLabelText("Skip sponsored spans in YouTube videos"),
    );
    expect(patched("service.media.sponsorblock.enabled")).toBe(true);
  });

  // The boxes have to show what *would* happen, not an empty set, or a parent
  // turning it on sees nothing ticked and assumes nothing is skipped.
  it("shows the default categories ticked when the config names none", () => {
    render(<ServicePage config={service({ enabled: true })} />);
    for (const name of ["Sponsor", "Self-promotion", "Intro", "End cards"]) {
      expect(
        (screen.getByRole("checkbox", { name }) as HTMLInputElement).checked,
      ).toBe(true);
    }
    for (const name of [
      "Filler tangent",
      "Non-music section",
      "Opening hook",
    ]) {
      expect(
        (screen.getByRole("checkbox", { name }) as HTMLInputElement).checked,
      ).toBe(false);
    }
  });

  // A checkbox writes the whole list, because that is what the config field is;
  // writing only the change would drop every other category.
  it("writes the full category list when one is added", async () => {
    render(<ServicePage config={service({ enabled: true })} />);
    await userEvent.click(
      screen.getByRole("checkbox", { name: "Filler tangent" }),
    );
    expect(patched("service.media.sponsorblock.categories")).toEqual([
      "sponsor",
      "selfpromo",
      "interaction",
      "intro",
      "outro",
      "filler",
    ]);
  });

  it("writes the full category list when one is removed", async () => {
    render(
      <ServicePage
        config={service({ enabled: true, categories: ["sponsor", "intro"] })}
      />,
    );
    await userEvent.click(screen.getByRole("checkbox", { name: "Intro" }));
    expect(patched("service.media.sponsorblock.categories")).toEqual([
      "sponsor",
    ]);
  });

  it("takes a mirror instance", async () => {
    render(<ServicePage config={service({ enabled: true })} />);
    await userEvent.type(
      screen.getByRole("textbox", { name: /SponsorBlock instance/ }),
      "https://sb.lan",
    );
    expect(patched("service.media.sponsorblock.api")).toBeDefined();
  });
});

describe("the per-library override", () => {
  beforeEach(() => apply.mockClear());
  afterEach(cleanup);

  const mediaKind = (sponsorblock?: boolean | null) => ({
    type: "media" as const,
    library: "/etc/lunchbox/movies.toml",
    ...(sponsorblock === undefined ? {} : { sponsorblock }),
  });

  /** KindEditor hands back the whole kind, merged; capture what it emits. */
  const renderKind = (sponsorblock?: boolean | null) => {
    const onChange = vi.fn();
    render(
      <KindEditor
        kind={mediaKind(sponsorblock) as never}
        onChange={onChange}
      />,
    );
    return onChange;
  };

  // Three states, not two: an entry that says nothing follows the service, and
  // that is different from one that says "never".
  it("starts at inherit and can be set either way", async () => {
    const onChange = renderKind();
    const select = screen.getByRole("combobox", { name: "Skip sponsors" });
    expect(select.textContent).toBe("Follow the service setting");

    await userEvent.click(select);
    await userEvent.click(screen.getByRole("option", { name: "Never" }));
    expect(onChange).toHaveBeenCalledWith(
      expect.objectContaining({ sponsorblock: false }),
    );
  });

  it("shows a library that opted out", () => {
    renderKind(false);
    expect(
      screen.getByRole("combobox", { name: "Skip sponsors" }).textContent,
    ).toBe("Never");
  });

  it("returns to inherit", async () => {
    const onChange = renderKind(true);
    await userEvent.click(
      screen.getByRole("combobox", { name: "Skip sponsors" }),
    );
    await userEvent.click(
      screen.getByRole("option", { name: "Follow the service setting" }),
    );
    expect(onChange).toHaveBeenCalledWith(
      expect.objectContaining({ sponsorblock: null }),
    );
  });
});
