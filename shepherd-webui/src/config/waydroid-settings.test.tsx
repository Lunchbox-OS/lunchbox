// @vitest-environment jsdom
/**
 * The `[service.waydroid]` controls, in a DOM (issue #2).
 *
 * Same reason as the HUD suite: the schema carried this table before the
 * editor did, and `check:coverage` found all seven of its fields unreachable.
 * What is worth checking is not that the controls exist but what they *write*,
 * and here that is mostly about the difference between "unset" and a value:
 *
 * - `preboot` has three states, not two. Unset means "decide from whether any
 *   Android activity exists", which is neither always nor never, and is what
 *   most devices should be on. A switch could not have said that.
 * - Clearing the lock mode has to remove the key rather than write a null,
 *   because TOML has no null and absent is what "inherit the default" means.
 * - `locktask` is the one mode that needs a separate provisioning step, and a
 *   device set to it without the DPC does not launch Android at all — so the
 *   menu has to say so at the moment it is picked.
 *
 * The wasm-backed document is stubbed, as in `hud-orientation.test.tsx`: these
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

const setValue = (path: string) =>
  apply.mock.calls.find(([p]) => p?.op === "set" && p?.path === path)?.[0]
    ?.value;

const wasUnset = (path: string) =>
  apply.mock.calls.some(([p]) => p?.op === "unset" && p?.path === path);

const openSection = async (title: string) => {
  const expand = screen.queryByRole("button", { name: `Expand ${title}` });
  if (expand) await userEvent.click(expand);
};

const shown = (label: string) =>
  screen.getByRole("combobox", { name: label }).textContent;

const choose = async (label: string, option: string) => {
  await userEvent.click(screen.getByRole("combobox", { name: label }));
  await userEvent.click(screen.getByRole("option", { name: option }));
};

/** A config with the table present, so the section's controls are rendered. */
const withWaydroid = (waydroid: Record<string, unknown> = {}) =>
  ({ config_version: 1, service: { waydroid } }) as unknown as RawConfig;

const SECTION = "Android (Waydroid)";

describe("the Android (Waydroid) settings", () => {
  beforeEach(() => apply.mockClear());
  afterEach(cleanup);

  it("reads as the default lock mode when the table is empty", async () => {
    render(<ServicePage config={withWaydroid()} />);
    await openSection(SECTION);
    expect(shown("Kiosk lock-in")).toBe("Status bar (the default)");
  });

  it("shows the configured lock mode", async () => {
    render(<ServicePage config={withWaydroid({ lock_mode: "locktask" })} />);
    await openSection(SECTION);
    expect(shown("Kiosk lock-in")).toBe("Lock Task (strict)");
  });

  it("writes the lock mode that was picked", async () => {
    render(<ServicePage config={withWaydroid()} />);
    await openSection(SECTION);
    await choose("Kiosk lock-in", "Off");
    expect(setValue("service.waydroid.lock_mode")).toBe("off");
  });

  // TOML has no null, so "inherit the default" is an absent key.
  it("removes the key rather than writing a null when cleared", async () => {
    render(<ServicePage config={withWaydroid({ lock_mode: "off" })} />);
    await openSection(SECTION);
    await choose("Kiosk lock-in", "Status bar (the default)");
    expect(wasUnset("service.waydroid.lock_mode")).toBe(true);
  });

  // Picking it is the moment to say so: a device left on locktask without the
  // device owner provisioned does not launch Android at all.
  it("warns that Lock Task needs the DPC, and only then", async () => {
    render(<ServicePage config={withWaydroid({ lock_mode: "statusbar" })} />);
    await openSection(SECTION);
    expect(screen.queryByText(/Device Policy Controller/)).toBeNull();
    cleanup();

    render(<ServicePage config={withWaydroid({ lock_mode: "locktask" })} />);
    await openSection(SECTION);
    expect(screen.queryByText(/Device Policy Controller/)).not.toBeNull();
  });

  // The three-state one. "Automatic" is not "never", and a switch would have
  // had to pick one of them to mean unset.
  it("keeps automatic preboot distinct from always and never", async () => {
    render(<ServicePage config={withWaydroid()} />);
    await openSection(SECTION);
    expect(shown("Keep Android warm")).toMatch(/^Automatic/);

    await choose("Keep Android warm", "Never");
    expect(setValue("service.waydroid.preboot")).toBe(false);

    apply.mockClear();
    await choose("Keep Android warm", "Always, from startup");
    expect(setValue("service.waydroid.preboot")).toBe(true);
  });

  it("goes back to automatic by removing the key", async () => {
    render(<ServicePage config={withWaydroid({ preboot: false })} />);
    await openSection(SECTION);
    expect(shown("Keep Android warm")).toBe("Never");
    await choose(
      "Keep Android warm",
      "Automatic — only if an Android activity exists (the default)",
    );
    expect(wasUnset("service.waydroid.preboot")).toBe(true);
  });

  // The boot timeout's placeholder is the daemon's own default, not a number
  // typed into the form — the thing `LOAD_TIME_DEFAULTS` exists to keep honest.
  it("offers the daemon's boot timeout as the placeholder", async () => {
    render(<ServicePage config={withWaydroid()} />);
    await openSection(SECTION);
    const field = screen.getByRole("spinbutton", { name: /Boot timeout/ });
    expect(field.getAttribute("placeholder")).toBe("60");
    expect((field as HTMLInputElement).value).toBe("");
  });
});
