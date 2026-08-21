#!/usr/bin/env node
/**
 * The one discipline that living in shepherd-webui costs.
 *
 * `src/config/` builds into the standalone bundle, which has no daemon to talk
 * to. An import reaching back into `src/api/` would drag axios, react-query and
 * the wire types into a bundle that can never use them — and would quietly
 * couple the editor to a running device. Genuinely shared code goes in
 * `src/shared/` instead.
 *
 * Run by `npm run check:boundary` and by CI.
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const ROOT = new URL("../src/config", import.meta.url).pathname;
const FORBIDDEN = [
  { pattern: /from\s+["'][^"']*\/api\//, what: "src/api/" },
  { pattern: /from\s+["']axios["']/, what: "axios" },
  { pattern: /from\s+["']@tanstack\/react-query["']/, what: "@tanstack/react-query" },
];

function walk(dir) {
  const out = [];
  for (const name of readdirSync(dir)) {
    // wasm/ is wasm-pack output; its generated glue is not ours to lint.
    if (name === "wasm") continue;
    const path = join(dir, name);
    if (statSync(path).isDirectory()) out.push(...walk(path));
    else if (/\.tsx?$/.test(path)) out.push(path);
  }
  return out;
}

let failures = 0;
for (const file of walk(ROOT)) {
  const source = readFileSync(file, "utf8");
  source.split("\n").forEach((line, i) => {
    for (const { pattern, what } of FORBIDDEN) {
      if (pattern.test(line)) {
        console.error(
          `${relative(process.cwd(), file)}:${i + 1}: src/config/ must not import ${what}\n` +
            `  ${line.trim()}\n` +
            `  Move anything genuinely shared into src/shared/ instead.`,
        );
        failures++;
      }
    }
  });
}

if (failures > 0) {
  console.error(`\n${failures} boundary violation(s).`);
  process.exit(1);
}
console.log("src/config/ boundary is clean.");
