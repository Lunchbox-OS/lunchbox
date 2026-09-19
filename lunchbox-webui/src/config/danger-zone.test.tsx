// @vitest-environment jsdom
/**
 * The management API's danger zone (issue #185).
 *
 * Every other section on the service page is recoverable by editing it again
 * from the same place. This one is not: it decides whether the interface doing
 * the editing still answers afterwards, and on a hardened device there is no
 * SSH to go back in with. So the warning is load-bearing UI, and worth a test
 * that it is actually next to the controls it is about rather than somewhere
 * further up the page.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, within } from "@testing-library/react";
import type { RawConfig } from "./model/config.generated";

const doc = {
  ready: true,
  loadError: null,
  versions: { config_version: 1, crate_version: "9.9.9" },
  view: {} as RawConfig,
  report: { kind: "semantic" as const, errors: [] },
  text: "config_version = 1\n",
  document: { text: "config_version = 1\n", name: null },
  dirty: false,
  apply: vi.fn(),
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

afterEach(cleanup);

describe("the management API's danger zone", () => {
  it("warns that these settings can end the session that saves them", () => {
    render(<ServicePage config={{ config_version: 1 } as RawConfig} />);
    expect(
      screen.getByText(/end the session that saved the change/i),
    ).toBeTruthy();
  });

  it("puts the warning around the management API section, not beside it", () => {
    render(<ServicePage config={{ config_version: 1 } as RawConfig} />);
    const zone = screen.getByText("Danger zone").closest("div")?.parentElement
      ?.parentElement;
    expect(zone).toBeTruthy();
    // The section's own switch is inside the rule.
    expect(
      within(zone as HTMLElement).getByLabelText("Enable Management API"),
    ).toBeTruthy();
  });

  it("leaves the recoverable sections outside it", () => {
    render(<ServicePage config={{ config_version: 1 } as RawConfig} />);
    const zone = screen.getByText("Danger zone").closest("div")?.parentElement
      ?.parentElement as HTMLElement;
    // Losing Bluetooth management does not lock anyone out of HTTP, so it is
    // not in here — a danger zone that contains everything warns about
    // nothing.
    expect(
      within(zone).queryByLabelText("Enable Bluetooth management"),
    ).toBeNull();
  });
});
