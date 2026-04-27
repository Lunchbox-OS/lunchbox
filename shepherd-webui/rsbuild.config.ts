import { defineConfig } from "@rsbuild/core";
import { pluginReact } from "@rsbuild/plugin-react";

export default defineConfig({
  plugins: [pluginReact()],
  source: {
    entry: { index: "./src/main.tsx" },
  },
  server: {
    proxy: {
      "/api": {
        target: "http://localhost:8080",
        changeOrigin: true,
      },
    },
  },
  html: {
    title: "Shepherd",
    meta: {
      viewport: "width=device-width, initial-scale=1, maximum-scale=1",
    },
  },
  output: {
    distPath: {
      root: "dist",
    },
  },
});
