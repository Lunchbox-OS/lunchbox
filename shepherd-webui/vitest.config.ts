import { defineConfig } from "vitest/config";

// Unit tests for the editor's pure logic — day-mask conversion, window
// merge/split, duration formatting. The parts with real edge cases and no DOM.
export default defineConfig({
  test: {
    include: ["src/**/*.test.ts"],
    environment: "node",
  },
});
