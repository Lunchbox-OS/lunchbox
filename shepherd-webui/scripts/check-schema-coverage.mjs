#!/usr/bin/env node
/**
 * Every field in the config schema should be reachable from the UI.
 *
 * `src/config/model/config.generated.ts` is rendered from `schema.rs`, so a
 * field added to the daemon and never wired into the editor shows up here
 * rather than in a bug report.
 *
 * **What this cannot catch**, and did not: a field the UI reaches from one
 * owner but not another. `RawGroup.tokens` went missing for exactly that
 * reason — `tokens`, `from` and `earn_ratio` were all referenced, just only
 * from the activity editor, never from the category one. A name-level sweep
 * sees those as covered. Real per-owner coverage needs a type graph the
 * generated types don't carry, so adding a `Raw*` sub-table to a second parent
 * stays a manual check.
 *
 * Run by `npm run check:coverage` and by CI.
 */
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";

const CONFIG_DIR = new URL("../src/config", import.meta.url).pathname;
const GENERATED = join(CONFIG_DIR, "model", "config.generated.ts");

/**
 * Fields deliberately left to the raw TOML pane, with the reason why. Free-form
 * JSON has no schema to render a form from.
 */
const EXEMPT = {
  payload: "kind.custom.payload is free-form JSON, like the vm/media driver args",
};

/**
 * Everything under `src/config/` that a human wrote.
 *
 * Generated files are excluded as a class, not just `config.generated.ts`:
 * a mirror of some *other* Rust type can share a property name with the config
 * schema — `wasm-types.generated.ts` has `group`, `kind`, `start` and `end` —
 * and counting those as coverage would mark a field reachable because a
 * different type happens to spell it the same way.
 */
function sourceFiles(dir) {
  const out = [];
  for (const name of readdirSync(dir)) {
    if (name === "wasm") continue; // wasm-pack output, not ours
    const path = join(dir, name);
    if (statSync(path).isDirectory()) out.push(...sourceFiles(path));
    else if (/\.tsx?$/.test(path) && !/\.generated\.tsx?$/.test(path)) out.push(path);
  }
  return out;
}

const generated = readFileSync(GENERATED, "utf8");
const source = sourceFiles(CONFIG_DIR)
  .map((f) => readFileSync(f, "utf8"))
  .join("\n");

// Property names from every generated interface and inline variant object.
// `type` is the kind discriminator, handled structurally by KindEditor.
const fields = [
  ...new Set([...generated.matchAll(/^\s{2,6}([a-z_][a-z0-9_]*)\??:/gm)].map((m) => m[1])),
].filter((f) => f !== "type");

let failed = false;

if (fields.length < 90) {
  console.error(
    `Only ${fields.length} fields parsed out of the generated types — the parser has ` +
      `probably stopped matching. Check src/config/model/config.generated.ts.`,
  );
  failed = true;
}

const missing = fields.filter((f) => !(f in EXEMPT) && !source.includes(f));
if (missing.length > 0) {
  console.error(
    `${missing.length} schema field(s) are unreachable from the UI:\n` +
      missing.map((f) => `  ${f}`).join("\n") +
      `\n\nWire them up under src/config/, or add an entry to EXEMPT in this ` +
      `script with the reason.`,
  );
  failed = true;
}

// An exemption for a field that no longer exists is stale.
for (const field of Object.keys(EXEMPT)) {
  if (!fields.includes(field)) {
    console.error(`"${field}" is exempt but no longer in the schema; drop it from EXEMPT.`);
    failed = true;
  }
}

if (failed) process.exit(1);
console.log(
  `All ${fields.length} schema fields are reachable from the UI ` +
    `(${Object.keys(EXEMPT).length} exempt).`,
);
