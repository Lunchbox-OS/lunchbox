/**
 * Which of the two builds this bundle is.
 *
 * Set by `rsbuild.config.ts` via `source.define`, so rspack inlines the value
 * and drops the dead branch. `standalone` is the static-host bundle;
 * `embedded` is the copy that rides inside shepherdd.
 */
export type UiTarget = "embedded" | "standalone";

// Declared rather than pulling in @types/node: rspack replaces this expression
// at build time, so it never actually reads a Node global.
declare const process: { env: Record<string, string | undefined> };

export const UI_TARGET: UiTarget =
  (process.env.SHEPHERD_UI_TARGET as UiTarget | undefined) ?? "embedded";

export const IS_STANDALONE = UI_TARGET === "standalone";
