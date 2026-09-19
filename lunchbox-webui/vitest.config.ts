import { defineConfig } from "vitest/config";

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
  test: {
    include: ["src/**/*.test.{ts,tsx}"],
    environment: "node",
  },
});
