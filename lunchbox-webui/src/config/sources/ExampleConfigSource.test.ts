/**
 * That the example really is inlined, and really is the repo's own file.
 *
 * The import that does it (`config.example.toml?raw`) is build plumbing rather
 * than code: it needs a Vite feature here and a matching rspack rule in
 * `rsbuild.config.ts` for the bundles. Break either and this file goes empty or
 * stops resolving — which nothing else would notice, because the button would
 * still render and would simply open a blank document.
 */
import { describe, expect, it } from "vitest";
import { ExampleConfigSource } from "./ExampleConfigSource";

describe("the bundled example config", () => {
  it("opens as the file the repo ships", async () => {
    const doc = await new ExampleConfigSource().open();

    expect(doc.name).toBe("config.example.toml");
    expect(doc.text).toContain("config_version");
    // Some of the comment block the editor exists to preserve.
    expect(doc.text).toContain("#");
    // The real file is ~40 kB. A stub, an empty string or a resolved-but-wrong
    // module would all be far short of that.
    expect(doc.text.length).toBeGreaterThan(10_000);
  });

  it("has nowhere to save back to, so it saves as a file", () => {
    // `canSaveInPlace` is what the toolbar reads to label the button "Save"
    // rather than "Download"; an inlined asset can never be the former.
    expect(new ExampleConfigSource().canSaveInPlace()).toBe(false);
  });
});
