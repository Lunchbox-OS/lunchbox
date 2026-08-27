// @vitest-environment jsdom
/**
 * The editor shell in a DOM: what the toolbar says, and navigation between
 * pages — which is where the editor keeps getting caught out.
 *
 * Both navigation bugs these cover were invisible to every other check in the
 * project. `tsc` was happy, the boundary and coverage guards had nothing to
 * say, and the pure-logic tests do not render anything — because both are about
 * *behaviour across a mount*, which needs a DOM to observe.
 *
 * The wasm-backed document is stubbed out. These tests are about which page is
 * showing and which subject is open, not about editing, and stubbing the module
 * also keeps them out of the way of `src/config/wasm/` being a generated
 * directory that may not exist yet.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { RawConfig } from "./model/config.generated";

// Declared rather than pulling in @types/node, matching `target.ts` — which is
// the module this reaches through to, and which declares it the same way
// because rspack replaces the expression at build time.
declare const process: { env: Record<string, string | undefined> };

const CONFIG: RawConfig = {
  config_version: 1,
  groups: [{ id: "games", label: "Games" }],
  entries: [
    {
      id: "celeste",
      label: "Celeste",
      group: "games",
      kind: { type: "process", command: "/bin/true" },
    },
    {
      id: "tuxmath",
      label: "Tux Math",
      kind: { type: "process", command: "/bin/true" },
    },
  ],
};

const doc = {
  ready: true,
  loadError: null,
  versions: { config_version: 1, crate_version: "9.9.9" },
  view: CONFIG,
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

// Replacing the module stops its transitive import of the generated wasm
// package from being resolved at all.
vi.mock("./doc/ConfigDocProvider", () => ({
  ConfigDocProvider: ({ children }: { children: React.ReactNode }) => children,
  useConfigDoc: () => doc,
}));

const { ConfigApp } = await import("./ConfigApp");

const tab = (name: string) => screen.getByRole("tab", { name });
/** The activity drawer, identified by the heading it puts up. */
const openActivity = () => screen.queryByRole("heading", { name: "Celeste" });

describe("the shell", () => {
  afterEach(cleanup);

  // The standalone editor talks to no device, so nothing else on screen says
  // which build it is — and a stale cached bundle looks exactly like a current
  // one until it disagrees with a daemon.
  it("names the build it is", () => {
    render(<ConfigApp />);
    expect(screen.getByText("v9.9.9")).toBeTruthy();
  });

  // The default target. In a device's own management UI, "Example" beside a
  // real config would read as an offer to overwrite it.
  it("does not offer the example config in the embedded build", () => {
    render(<ConfigApp />);
    expect(screen.queryByRole("button", { name: "Example" })).toBeNull();
  });
});

/**
 * The standalone build, which needs its own module registry: `target.ts`
 * resolves `IS_STANDALONE` once, when it is first evaluated, so seeing both
 * branches in one file means re-importing the tree after changing what it
 * reads. Driving it through the real environment variable rather than a stub
 * also checks `target.ts` itself, which is the thing the build sets.
 */
describe("the standalone shell", () => {
  afterEach(() => {
    cleanup();
    delete process.env.SHEPHERD_UI_TARGET;
  });

  it("opens the bundled example config", async () => {
    const user = userEvent.setup();
    process.env.SHEPHERD_UI_TARGET = "standalone";
    vi.resetModules();
    vi.doMock("./doc/ConfigDocProvider", () => ({
      ConfigDocProvider: ({ children }: { children: React.ReactNode }) => children,
      useConfigDoc: () => doc,
    }));
    const { ConfigApp: Standalone } = await import("./ConfigApp");

    render(<Standalone />);
    await user.click(screen.getByRole("button", { name: "Example" }));

    // Opening goes through the same path as any other source, so the shell
    // needs no special case for it.
    expect(doc.openFrom).toHaveBeenCalledTimes(1);
    const source = doc.openFrom.mock.calls[0][0] as { label: string };
    expect(source.label).toBe("Example configuration");
  });
});

describe("navigating between activities and categories", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  // Testing Library only registers its own cleanup when Vitest's `globals` are
  // on, and they are not — without this the previous test's tree stays mounted
  // and every query finds two of everything.
  afterEach(cleanup);

  it("opens a category's settings from its column header on the board", async () => {
    const user = userEvent.setup();
    render(<ConfigApp />);

    // The board groups activities by category; the header is a link.
    await user.click(screen.getByRole("button", { name: "Games" }));

    await waitFor(() => expect(tab("Categories").getAttribute("aria-selected")).toBe("true"));
    // The category's own detail is showing, not the activity board.
    expect(screen.getByRole("tab", { name: "Schedule" })).toBeTruthy();
  });

  it("opens an activity from its category's member list", async () => {
    const user = userEvent.setup();
    render(<ConfigApp />);

    await user.click(tab("Categories"));
    await user.click(screen.getByText("Celeste"));

    // The drawer is modal, so it aria-hides the page tabs while open — its
    // presence is the observable outcome, not the tab's selected state.
    await waitFor(() => expect(openActivity()).not.toBeNull());
  });

  /**
   * The regression this file exists for.
   *
   * Pages are conditionally rendered, so leaving a tab unmounts one and coming
   * back mounts it fresh — and a mount runs every effect whatever its deps say.
   * A focus request left in the shell's state was therefore re-applied on every
   * return, and the activity you had closed kept reappearing, indefinitely.
   */
  it("does not re-open that activity after navigating away and back", async () => {
    const user = userEvent.setup();
    render(<ConfigApp />);

    await user.click(tab("Categories"));
    await user.click(screen.getByText("Celeste"));
    await waitFor(() => expect(openActivity()).not.toBeNull());

    await user.click(screen.getByRole("button", { name: "Close" }));
    await waitFor(() => expect(openActivity()).toBeNull());

    await user.click(tab("Device"));
    await user.click(tab("Activities"));

    // `user.click` flushes effects, so the remount's effects have already run
    // by here — if a stale request were going to re-apply, it would have.
    expect(
      openActivity(),
      "the activity drawer re-opened by itself after returning to the board",
    ).toBeNull();
  });

  /**
   * The other half of the same contract: a request must still work twice.
   * Consuming it must not make a repeat request a no-op, which is what the
   * nonce is for.
   */
  it("re-opens the same activity when asked a second time", async () => {
    const user = userEvent.setup();
    render(<ConfigApp />);

    for (const _pass of [1, 2]) {
      await user.click(tab("Categories"));
      await user.click(screen.getByText("Celeste"));
      await waitFor(() => expect(openActivity()).not.toBeNull());
      await user.click(screen.getByRole("button", { name: "Close" }));
      await waitFor(() => expect(openActivity()).toBeNull());
    }
  });
});
