import { defineConfig } from "@rsbuild/core";
import { pluginReact } from "@rsbuild/plugin-react";

/**
 * One source tree, two deploy targets.
 *
 * - `embedded` (default) builds the management UI into `dist/`, which
 *   `rust-embed` compiles into shepherdd (see
 *   `crates/shepherd-http/src/web_assets.rs`). The config editor is *not* part
 *   of it: nothing imports `src/config/ConfigApp` from `src/App.tsx`, so its
 *   chunks and its ~800 kB wasm validator stay out of `dist/`, and out of the
 *   daemon binary built from it. `src/App.tsx` records why it is unrouted and
 *   what it would take to route it.
 * - `standalone` builds only the config editor into `dist-standalone/`, for a
 *   static host. It never talks to a daemon, so it carries none of the API
 *   layer.
 *
 * The two must write different directories: anything left in `dist/` ends up
 * inside the daemon binary.
 */
const target =
  process.env.SHEPHERD_UI_TARGET === "standalone" ? "standalone" : "embedded";
const isStandalone = target === "standalone";

export default defineConfig({
  plugins: [pluginReact()],
  source: {
    entry: {
      index: isStandalone ? "./src/config/standalone.tsx" : "./src/main.tsx",
    },
    define: {
      // Read by `src/config/target.ts`. Rspack inlines this, so the branch it
      // guards becomes dead code the bundler drops.
      "process.env.SHEPHERD_UI_TARGET": JSON.stringify(target),
    },
  },
  server: isStandalone
    ? {}
    : {
        proxy: {
          "/api": {
            // `https`, since issue #156: a daemon bound anywhere but loopback
            // comes up on TLS, and `config.example.toml` binds `0.0.0.0`.
            target: "https://localhost:8080",
            // The dev stack's certificate is self-signed by construction, so
            // the proxy has to accept it. It is a loopback hop on the
            // developer's own machine.
            secure: false,
            // *Not* `changeOrigin`. Rewriting `Host` to the upstream while the
            // browser still sends `Origin: http://localhost:3000` makes every
            // cookie-authenticated write look cross-origin to the daemon's CSRF
            // check, which answers 403. Leaving `Host` alone keeps the two
            // agreeing.
            changeOrigin: false,
          },
        },
      },
  html: {
    title: isStandalone ? "Shepherd Config Editor" : "Shepherd",
    meta: {
      viewport: "width=device-width, initial-scale=1, maximum-scale=1",
    },
  },
  output: {
    // Relative asset paths for the standalone bundle: correct at a repository
    // subpath on GitHub Pages, at the root on Cloudflare Pages, and over
    // `file://` in a future Tauri shell. Override with PUBLIC_BASE_PATH when a
    // host needs an absolute prefix.
    assetPrefix: isStandalone ? (process.env.PUBLIC_BASE_PATH ?? "./") : "/",
    distPath: {
      root: isStandalone ? "dist-standalone" : "dist",
    },
    cleanDistPath: {
      keep: [/\.gitkeep$/],
    },
  },
  tools: {
    rspack: {
      module: {
        rules: [
          // `import x from "./f.toml?raw"` — the file's text as a string.
          // Vite implements this natively, so the tests get it for free;
          // rspack does not, and without this rule it hands the TOML to the
          // JavaScript parser. Used to inline `config.example.toml` into the
          // standalone editor (`src/config/sources/ExampleConfigSource.ts`).
          { resourceQuery: /^\?raw$/, type: "asset/source" },
        ],
      },
      experiments: {
        // wasm-pack's `--target web` output fetches its `.wasm` at runtime;
        // this makes rspack emit it as an asset rather than trying to inline
        // it as a module.
        asyncWebAssembly: true,
      },
    },
  },
});
