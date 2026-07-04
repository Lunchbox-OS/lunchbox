# Harmonize versions (issue #83)

## Prompt

> `/remote-control` — implement #83

## Issue #83: "Harmonize versions"

> `shepherd-launcher` is a composition of a number of packages in this monorepo:
> * `shepherdd`/`shepherd-launcher`/`shepherd-hud`: the main launcher and management system
> * `shepherd-media`: the media library
> * `shepherd-*-bridge`: input compatibility modes
>
> None of the above actually depend on each other.
>
> There are also other incoming pieces of work, each with their own major components:
> * Android+BLE management interface (#71)
> * Android build of `shepherd-media` (#72)
>
> All of these need to pull a version string from exactly one place to make it
> easier to bump.

## Starting state

The version string `0.1.0` was hardcoded in five independent places:

1. `Cargo.toml` `[workspace.package] version` — the Rust workspace source.
   (All in-workspace crates already used `version.workspace = true`.)
2. `crates/shepherd-firewall-bpf/Cargo.toml` — excluded from the workspace
   (it targets `bpfel-unknown-none` on nightly), so it can't inherit the
   workspace version.
3. `shepherd-webui/package.json` (+ `package-lock.json`, two occurrences).
4. `companion-android/app/build.gradle.kts` `versionName`.
5. `scripts/shepherd` `VERSION="0.1.0"`.

## Design

Introduce a repo-root **`VERSION`** file as the single canonical source. A plain
text file is the only thing every ecosystem here (Cargo, npm, Gradle, bash) can
read without a TOML/JSON parser.

Consumers split into two groups:

* **Pull at build/run time** (can never drift, no literal):
  * `scripts/shepherd` reads `VERSION` into its `VERSION` var.
  * `companion-android` Gradle reads `rootProject.projectDir.parentFile/VERSION`
    at configure time for `versionName`.

* **Synced literals** (Cargo and npm can't read a file at parse time):
  * `Cargo.toml [workspace.package]`, `shepherd-firewall-bpf/Cargo.toml`,
    `shepherd-webui/package.json` + `package-lock.json`.
  * Written by `shepherd version set` and verified by `shepherd version check`.

### Tooling: `scripts/lib/version.sh`

* `shepherd version` / `version get` — print the canonical version.
* `shepherd version set X.Y.Z` — validate semver, write `VERSION`, sed the two
  Cargo literals, run `npm version --no-git-tag-version --allow-same-version`
  in `shepherd-webui` (rewrites package.json + lockfile while preserving
  formatting — a hand-rolled JSON edit would reflow the whole lockfile), and
  best-effort `cargo update --workspace --offline` to refresh `Cargo.lock`.
* `shepherd version check` — compare every synced literal against `VERSION`;
  exit nonzero with a diff if any drifts.

### CI

A new lightweight `versions` job runs `./scripts/shepherd version check`. It
uses the default `node:20-bookworm` job container (bash + node + grep), so it
needs no custom CI image and no `needs:` dependency — it fails fast if a
manifest is hand-edited out of sync.

## Why not make Cargo the source of truth?

Cargo has no include mechanism for the `version` field, so its literal is
immovable — a `VERSION`-canonical design would still have to sync it. Reading
Cargo's nested `[workspace.package]` from Gradle/bash needs a TOML-aware parser,
whereas a plain `VERSION` file is trivially readable everywhere. So `VERSION` is
canonical and Cargo is a synced+checked derivative.

## Verification

* `shepherd version set 9.9.9` → all five literals + both lockfile occurrences
  + `Cargo.lock` member pins updated; `version check` passes. Round-trip back to
  `0.1.0` leaves a clean git diff.
* Hand-editing `Cargo.toml` to `0.2.0` makes `version check` exit 1 with a diff.
