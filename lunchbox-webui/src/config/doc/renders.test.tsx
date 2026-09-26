// @vitest-environment jsdom
/**
 * What one edit costs in renders (issue #235).
 *
 * Every text field in the editor hands each keystroke to `apply`, so whatever
 * an edit re-renders, typing re-renders once per character. When that was the
 * whole editor, twice, fast typing in Firefox lost characters.
 *
 * The wasm document is replaced by a stand-in that appends each patch to its
 * text. These tests are about when the provider tells React something changed,
 * not about what a patch does to the file, and stubbing the module keeps them
 * runnable without the generated `src/config/wasm/`, as the other DOM tests do.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";

class FakeDoc {
  private undos: string[] = [];
  private redos: string[] = [];
  constructor(private src: string) {}
  static blank() {
    return new FakeDoc("config_version = 1\n");
  }
  static open(src: string) {
    return new FakeDoc(src);
  }
  text() {
    return this.src;
  }
  view() {
    return JSON.stringify({ config_version: 1 });
  }
  validate() {
    return JSON.stringify({ kind: "semantic", errors: [] });
  }
  apply(patch: string) {
    this.undos.push(this.src);
    this.redos = [];
    this.src += `# ${patch}\n`;
    return true;
  }
  endGesture() {}
  replaceText(src: string) {
    this.undos.push(this.src);
    this.src = src;
    return true;
  }
  undo() {
    const prev = this.undos.pop();
    if (prev === undefined) return false;
    this.redos.push(this.src);
    this.src = prev;
    return true;
  }
  redo() {
    const next = this.redos.pop();
    if (next === undefined) return false;
    this.undos.push(this.src);
    this.src = next;
    return true;
  }
  canUndo() {
    return this.undos.length > 0;
  }
  canRedo() {
    return this.redos.length > 0;
  }
}

vi.mock("../wasm/lunchbox_config", () => ({
  default: async () => {},
  ConfigDoc: FakeDoc,
  versions: () => JSON.stringify({ config_version: 1, crate_version: "0.0.0" }),
}));

const { ConfigDocProvider, useConfigDoc } = await import("./ConfigDocProvider");
const { set } = await import("./patches");

type Doc = ReturnType<typeof useConfigDoc>;

/** Counts its own renders, and hands out the context it last saw. */
function probe() {
  const seen = { renders: 0, doc: null as Doc | null };
  function Probe() {
    seen.renders += 1;
    seen.doc = useConfigDoc();
    return null;
  }
  return { seen, Probe };
}

/** Renders the provider and waits for the (stubbed) module to load. */
async function mount(children: React.ReactNode) {
  render(<ConfigDocProvider>{children}</ConfigDocProvider>);
  await act(async () => {});
  // Let the first projection land, so it is not counted against an edit.
  act(() => vi.advanceTimersByTime(1000));
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("an edit", () => {
  it("renders the editor once, not once for the version and again for the text", async () => {
    const { seen, Probe } = probe();
    await mount(<Probe />);
    expect(seen.doc?.ready).toBe(true);

    seen.renders = 0;
    act(() => seen.doc!.apply(set("entries", [])));

    expect(seen.renders).toBe(1);
    expect(seen.doc!.text).toContain("entries");
  });

  it("keeps the text in step through undo and redo", async () => {
    const { seen, Probe } = probe();
    await mount(<Probe />);

    act(() => seen.doc!.apply(set("entries", [])));
    const edited = seen.doc!.text;

    act(() => seen.doc!.undo());
    expect(seen.doc!.text).toBe("config_version = 1\n");

    act(() => seen.doc!.redo());
    expect(seen.doc!.text).toBe(edited);
  });
});
