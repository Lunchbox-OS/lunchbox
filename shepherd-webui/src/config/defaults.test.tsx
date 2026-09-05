// @vitest-environment jsdom
/**
 * Generated defaults reach the controls that show them.
 *
 * The editor's job for an *unset* field is to render the value the daemon will
 * actually pick, and until this refactor it did that by restating the Rust:
 * a `?? true` here, a `const DEFAULT_COOLDOWN_MIN_SESSION = 120` there. Those
 * now come from `field-defaults.generated.ts`, which `rpc_codegen_drift` keeps
 * honest against `schema.rs` and `load_defaults.rs`.
 *
 * That check covers the file's *contents*. What it cannot see is whether any
 * component reads it — a migration that imported the constant and then kept
 * comparing against a literal would pass every existing test. So these render
 * the real controls with nothing set and assert the generated value is what
 * comes out, one per mechanism:
 *
 * - a serde default on a plain struct (`FIELD_DEFAULTS`),
 * - a serde default inside a `kind` variant (`KIND_FIELD_DEFAULTS`),
 * - a default the daemon resolves at policy load (`LOAD_TIME_DEFAULTS`).
 *
 * The document is stubbed exactly as in `sponsorblock.test.tsx`: these are
 * assertions about what the controls display, not about how patches apply.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { RawConfig, RawEntryKind } from "./model/config.generated";
import {
  FIELD_DEFAULTS,
  KIND_FIELD_DEFAULTS,
  LOAD_TIME_DEFAULTS,
} from "./model/field-defaults.generated";

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
const { KindEditor } = await import("./components/KindEditor");
const { BrowserEditor } = await import("./components/NetworkEditors");

afterEach(() => {
  cleanup();
  apply.mockReset();
});

/** Open a collapsed section — its children are unmounted while it is closed. */
const openSection = async (name: string) => {
  const expand = screen.queryByRole("button", { name: `Expand ${name}` });
  if (expand) await userEvent.click(expand);
};

describe("an unset control shows what the daemon will do", () => {
  it("takes a plain serde default from the generated table", async () => {
    // `disable_dev_tools` is `#[serde(default = "default_true")]`, so a browser
    // table that says nothing about it still gets the lockdown.
    render(<BrowserEditor path="entries.0.browser" value={{ profile_id: "b" }} />);

    const devTools = screen.getByRole("switch", { name: /disable devtools/i });
    expect(FIELD_DEFAULTS.RawBrowserConfig.disable_dev_tools).toBe(true);
    expect((devTools as HTMLInputElement).checked).toBe(
      FIELD_DEFAULTS.RawBrowserConfig.disable_dev_tools,
    );
  });

  it("takes a kind's own serde default from the generated table", async () => {
    // An ebook that names only its file still reads at the default size.
    const kind = { type: "ebook", book: "~/b.epub" } as RawEntryKind;
    render(<KindEditor kind={kind} onChange={vi.fn()} />);

    const size = screen.getByLabelText(/text size/i) as HTMLInputElement;
    expect(KIND_FIELD_DEFAULTS.ebook.font_size).toBe(16);
    expect(size.value).toBe(String(KIND_FIELD_DEFAULTS.ebook.font_size));
  });

  it("takes a load-time default as the placeholder the schema cannot carry", async () => {
    // `save_grace_seconds` is an `Option` the daemon resolves in
    // `Policy::from_raw`, so `schemars` reports no default for it at all and
    // this value can only have come from `LoadTimeDefaults`.
    render(<ServicePage config={{ config_version: 1 } as RawConfig} />);
    await openSection("Defaults");

    const grace = screen.getByLabelText(
      /time to save when the schedule closes/i,
    ) as HTMLInputElement;
    expect(LOAD_TIME_DEFAULTS.save_grace_seconds).toBe(120);
    expect(grace.placeholder).toBe("2m");
    expect(grace.value).toBe("");
  });

  /**
   * The one that would have caught a swapped lookup: `allow_change` exists on
   * both the volume and brightness tables, and the two are separate rows in the
   * generated file precisely so they can disagree later.
   */
  it("keeps same-named fields on different types apart", () => {
    expect(FIELD_DEFAULTS.RawVolumeConfig).not.toBe(
      FIELD_DEFAULTS.RawBrightnessConfig,
    );
    expect(FIELD_DEFAULTS.RawVolumeConfig.allow_change).toBeTypeOf("boolean");
    expect(FIELD_DEFAULTS.RawBrightnessConfig.allow_change).toBeTypeOf("boolean");
  });
});
