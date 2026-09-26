import { existsSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { defineConfig, type Plugin } from "vitest/config";

const ABSENT_WASM = "\0lunchbox-config-wasm-absent";

/**
 * Lets a test `vi.mock` the wasm module when it has not been generated.
 *
 * `src/config/wasm/` is wasm-pack output, and the CI job that runs these tests
 * has no Rust toolchain to make it. A mock still needs its target to resolve,
 * so when the file is missing the import resolves to a placeholder instead —
 * one that fails loudly if a test forgets to mock it and actually loads it.
 */
function absentWasm(): Plugin {
  return {
    name: "absent-config-wasm",
    enforce: "pre",
    resolveId(id, importer) {
      if (!importer || !id.endsWith("/wasm/lunchbox_config")) return null;
      return existsSync(resolve(dirname(importer), `${id}.js`)) ? null : ABSENT_WASM;
    },
    load(id) {
      if (id !== ABSENT_WASM) return null;
      return `export default async () => {
  throw new Error("src/config/wasm/ has not been generated; mock it, or run \`lunchbox dev webui --wasm\`");
};
export const ConfigDoc = undefined;
export const versions = undefined;
`;
    },
  };
}

/**
 * Two kinds of test live here.
 *
 * Most are pure logic — day masks, window merging, duration parsing — and run
 * in plain node. A few need a DOM: component behaviour that only shows up
 * across mount and unmount, which no static check can see. Those opt in with
 * `// @vitest-environment jsdom` at the top of the file, so the pure ones stay
 * fast.
 */
export default defineConfig({
  plugins: [absentWasm()],
  test: {
    include: ["src/**/*.test.{ts,tsx}"],
    environment: "node",
  },
});
