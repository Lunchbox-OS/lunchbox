# For developers

## Build for development

### tl;dr

You need a Wayland-capable Linux system, Rust, and a small set of system
dependencies. Once installed, `./run-dev` will start a development instance.

### Requirements

1. **Linux with Wayland**

   * Any modern Wayland compositor is sufficient. For Ubuntu, this means 26.04 or higher.
   * Optional (but recommended for realistic testing): TPM-based full disk encryption and a BIOS/UEFI password to prevent local tampering.

2. **System dependencies**

   * Platform-specific packages are required for building and running.
   * View packages with: `./scripts/shepherd deps print dev`
   * Install all dev dependencies: `./scripts/shepherd deps install dev`
   * **Note**: Rust is automatically installed via rustup when installing build or dev dependencies.

### Unified script system

`shepherd-launcher` provides a unified script system for managing dependencies, building, and running:

```sh
# View and install dependencies
./scripts/shepherd deps print dev        # List all dev dependencies
./scripts/shepherd deps install dev      # Install all dev dependencies

# Build binaries
./scripts/shepherd build                 # Debug build
./scripts/shepherd build --release       # Release build

# Development
./scripts/shepherd dev run               # Build and run in nested Sway
```

For CI/build-only environments:
```sh
./scripts/shepherd deps install build    # Build dependencies only
./scripts/shepherd build --release       # Production build
```

For runtime-only systems:
```sh
./scripts/shepherd deps install run      # Runtime dependencies only
```

See `./scripts/shepherd --help` for all available commands.

### Running in development

Start a development instance:

```sh
./run-dev
```

#### Adjusting the time

To avoid having to adjust the system clock or wait for timeouts, development
builds can mock the time with `SHEPHERD_MOCK_TIME`:

```sh
SHEPHERD_MOCK_TIME="2025-12-25 15:30:00" ./run-dev
```

Time and activity history are maintained in a SQLite database, which
`./run-dev` places at `./dev-runtime/data/shepherdd.db`. Edit this database
using a tool like [DB Browser for SQLite](https://sqlitebrowser.org/) while the
service is not running to inject application usage. The schema is defined in
the [shepherd-store crate](./crates/shepherd-store/).

### Headless development (no login session, for SSH / CI / agents)

`./run-dev` boots a *nested* Sway that needs a graphical login session. To run
and **screenshot** the full stack without one — over SSH, in CI, or from a
coding agent — use the headless session, which boots the same `sway.conf`,
`config.example.toml`, and binaries against the GPU-less headless wlroots
backend:

```sh
./scripts/shepherd deps install agent          # grim + wtype + jq (also in `deps install dev`)
./scripts/shepherd dev headless                # build + boot, detached
./scripts/shepherd dev tree                    # window tree (app_id / focus)
./scripts/shepherd dev shot home.png           # screenshot the virtual output
./scripts/shepherd dev key Down                # inject input; also: dev type / dev click
./scripts/shepherd dev stop                    # tear down
```

Useful flags on `dev headless`: `--time "2025-12-25 21:00:00"` (mock the clock
for availability/bedtime/time-limit testing), `--config PATH` (boot an arbitrary
config), `--user NAME` (run the stack as another user — their groups, `HOME`, and
default `~/.config/shepherd/config.toml`), `--size WxH`, `--gpu`, `--no-build`.
Connection state lives in `dev-runtime/headless/session.env`; the compositor log
is `dev-runtime/headless/sway.log`. See the design notes in
[`docs/ai/history`](./docs/ai/history/) for internals.

**Not usable for GPU performance work.** The headless session exports
`LIBGL_ALWAYS_SOFTWARE=1` for every client, so even `--gpu` (which only swaps
wlroots' own renderer) leaves the launcher, HUD and `shepherd-media` on
llvmpipe. To measure anything that touches the GPU — video decode, compositing,
frame pacing — boot a real session on a spare VT instead:

```sh
sudo mkdir -p /run/shepherd-perf && sudo chmod 700 /run/shepherd-perf
sudo setsid openvt -c 3 -s -- env XDG_RUNTIME_DIR=/run/shepherd-perf \
    LIBSEAT_BACKEND=builtin XDG_SESSION_TYPE=wayland sway -c sway.conf
# then, over SSH:
#   sudo env XDG_RUNTIME_DIR=/run/shepherd-perf WAYLAND_DISPLAY=wayland-1 <client>
sudo pkill -x sway && sudo chvt 1     # teardown
```

`LIBSEAT_BACKEND=builtin` under `openvt` is what lets the session take DRM
master without a graphical login. See
[`docs/ai/history/2026-07-28 001 shepherd-media performance investigation.md`](./docs/ai/history/)
for a worked example.

### Web UI

The management API HTTP server (`shepherd-http`) embeds the React SPA at compile
time from `shepherd-webui/dist/`. Build it before building the Rust code:

```sh
cd shepherd-webui
npm install       # once
npm run typecheck # tsc --noEmit; see below
npm run build     # generates shepherd-webui/dist/
cd ..
cargo build       # dist/ is now embedded in the binary
```

`npm run build` goes through rsbuild, which **transpiles without checking
types** — a genuine type error compiles and ships. `npm run typecheck` is the
only thing that checks them, so run it alongside the build; CI runs it as its
own job.

During development, run the rsbuild dev server (which proxies API calls to
`localhost:8080`) instead of embedding:

```sh
shepherd dev webui                    # hot-reloading, usually on port 3000
shepherd dev webui --standalone       # config editor only, no daemon needed
shepherd dev webui -- --port 3001     # anything after -- goes to rsbuild
```

This builds the config editor's wasm validator when it is missing, installs npm
dependencies on first run, and then hands off to rsbuild in the foreground —
Ctrl-C stops it. `npm run dev` from inside `shepherd-webui/` does the same thing
without those two steps.

The Rust binary is still needed for the API; the dev server is only for the
frontend. If the web UI has not been built, shepherdd still works normally — the
daemon just returns 404 for all non-API routes.

Unit tests, typechecking and the import boundary check:

```sh
cd shepherd-webui
npm test             # vitest, for the pure logic (day masks, durations)
npm run typecheck    # tsc --noEmit
npm run check:boundary
```

### Generated client types

The payload types both clients use are **generated** from the Rust definitions,
not hand-written. Change `crates/shepherd-api/src/types.rs` (adding a type to
`WireTypes` in `crates/shepherd-wire-codegen/src/wire_schema.rs` if it is only
reachable as an RPC parameter), then:

```sh
cargo run -p shepherd-wire-codegen --bin rpc-codegen
```

That rewrites five checked-in files: `docs/rpc-schema.json`, the two method-name
mirrors, and the payload mirrors for each client —
`companion-android/.../WireTypes.generated.kt` and
`shepherd-webui/src/api/wire-types.generated.ts`. Editing any of them by hand is
pointless; the next run overwrites it, and `tests/rpc_codegen_drift.rs` fails
until the regenerated output is committed.

The mirrors were hand-written once and drifted: four `ReasonCode` variants went
missing from the companion, and a renamed `DailyOverride` field went unnoticed
until it broke every override lookup on the phone. Neither was catchable from
the method schema alone, and neither compiler could see it — `tsc` checks
TypeScript against TypeScript, and Kotlin against Kotlin.

Note that the codegen crate is deliberately outside `default-members`, so
`cargo build` never compiles `schemars` into the shipped binaries. Reaching its
tests therefore needs `cargo test --workspace` (which is what CI runs); a bare
`cargo test` silently skips the drift check.

`shepherd-webui/src/api/types.ts` re-exports the generated types and keeps only
the presentation helpers, so the rest of the UI still imports wire shapes from
one place.

### Config editor

A graphical editor for `config.toml` lives in
[`shepherd-webui/src/config/`](shepherd-webui/src/config/) and builds two ways
from one source:

| Target | Build | Dev server | Output |
|---|---|---|---|
| Standalone static site | `shepherd build config-editor` | `shepherd dev webui --standalone` | `dist-standalone/`, for a static host |
| Embedded in shepherdd | `npm run build` | `shepherd dev webui` | `dist/` — the management UI, which does **not** route to the editor today |

The editor is not reachable from the management UI yet, and `src/App.tsx` says
why at the point where the route would go. Its only `ConfigSource` reads and
writes files on whatever computer is doing the browsing, so a "Config" tab in a
device's own web UI would read as "edit this device's configuration" while doing
nothing of the sort. That waits on a `DeviceConfigSource`, which waits on
privilege separation in `shepherd-http` — a config write runs arbitrary commands,
and one blanket auth layer currently covers all of `/api/v1`.

Leaving it unrouted also keeps the editor's chunks and its ~800 kB wasm
validator out of `dist/`, and so out of the binary `rust-embed` builds from it.

The two **must** write different directories — anything left in `dist/` is
compiled into the daemon binary by `rust-embed`. `rsbuild.config.ts` switches on
`SHEPHERD_UI_TARGET`, and `output.distPath` is resolved relative to the working
directory, so always run the npm scripts from inside `shepherd-webui/`.

The editor validates with the daemon's own parser, compiled to WebAssembly from
[`crates/shepherd-config-wasm`](crates/shepherd-config-wasm/), rather than a
TypeScript reimplementation of `validation.rs`. That crate also holds the
`toml_edit` document model that makes editing comment-preserving. Build the wasm
artifact before the npm build:

```sh
./scripts/shepherd build config-wasm   # wasm-pack -> src/config/wasm/
```

`shepherd build config-editor` and `shepherd dev webui` both do this for you when
the artifact is missing; pass `--wasm` to the latter to force a rebuild after
changing the crate. `src/config/wasm/` is generated and gitignored;
`npm run typecheck` needs it to exist.

#### Hosting

The standalone bundle is published to Cloudflare Pages at
<https://config.shepherd.armeafamily.com>, from the `config-editor` job in
[`release.yml`](.github/workflows/release.yml). It deploys on `vX.Y.Z` tags
rather than on every push to main, so the hosted editor matches the last
released shepherd — it renders a `config_version` that ships with the daemon,
and an editor ahead of the release would offer fields the installed version
cannot read. Prerelease tags (`v0.4.0-rc1`) are skipped.

Direct Upload, so Cloudflare needs no access to the repo. Two secrets:
`CLOUDFLARE_API_TOKEN` (with the "Cloudflare Pages: Edit" permission) and
`CLOUDFLARE_ACCOUNT_ID`.

It gets its own subdomain rather than a path under `shepherd.armeafamily.com`,
which is left free for a landing and documentation site. A path would have meant
either building both from one pipeline (Direct Upload replaces the whole
deployment, so one project cannot host two independently-deployed sites) or
putting a Worker in front to route `/config*` — machinery a subdomain does not
need.

The bundle needs nothing unusual from a host — no rewrite rules, since the
editor has no router; no COOP/COEP, since there are no threads. Two things do
matter if you ever serve it elsewhere: `application/wasm` for `.wasm` (a
mismatch falls back to a slower non-streaming load rather than breaking), and a
`script-src` that permits `'wasm-unsafe-eval'`.

Served at a domain root, so the default relative `assetPrefix` is correct and
`PUBLIC_BASE_PATH` stays unset. Set it (to e.g. `/config/`) only if the editor
ever moves under a subpath — relative paths would otherwise break on the
no-trailing-slash form of the URL.

Two rules keep the split working, both enforced:

* **`src/config/` must not import `src/api/`**, axios, or react-query — the
  standalone bundle has no daemon to talk to. Genuinely shared code goes in
  `src/shared/`. Checked by `npm run check:boundary`.
* **The TypeScript mirrors of the config schema are generated**, by
  `cargo run -p shepherd-wire-codegen --bin rpc-codegen`, into
  `src/config/model/config.generated.ts`. A drift test fails CI if the
  checked-in copy goes stale.

The document model is plain Rust with strings on its edges, so it tests
natively without a browser:

```sh
cargo test -p shepherd-config-wasm
```

`crates/shepherd-config-wasm/tests/preservation.rs` is the suite that matters:
it holds the line on editing `config.example.toml` without disturbing a comment.

### Android companion app

The BLE management companion app lives in [`companion-android/`](companion-android/)
and is independent of the Rust build. Install its toolchain (JDK + Android SDK +
NDK into `/opt/android-sdk`) with the dedicated deps set, then build:

```sh
./scripts/shepherd deps install android   # JDK 21 + Android SDK + NDK
cd companion-android
export ANDROID_SDK_ROOT=/opt/android-sdk   # see below
./gradlew :app:assembleDebug               # debug APK (sideload-friendly)
./gradlew :app:testDebugUnitTest           # unit tests
```

Gradle does not find the SDK on its own here: `deps install android` puts it in
`/opt/android-sdk` rather than the `~/Android/Sdk` the toolchain probes by
default, and this repo has no checked-in `companion-android/local.properties`
(it is git-ignored). Without `ANDROID_SDK_ROOT` — or `ANDROID_HOME`, or an
`sdk.dir` line in that `local.properties` — every Gradle task fails in a few
seconds with `SDK location not found`. CI does not hit this because the Android
job runs in a container image that already exports it, so a green CI run is no
evidence that a bare `./gradlew` works in your shell. The `./scripts/shepherd`
wrappers set it for you; invoking `./gradlew` directly is what needs the export.

To build *and* push it to a phone/tablet/Fire TV attached over `adb` in one
step (from a checkout this builds; from an installed `.deb` the same command
downloads the matching signed release instead):

```sh
./scripts/shepherd apps install companion   # or: media — no sudo, adb keys are per-user
```

See [`companion-android/README.md`](companion-android/README.md) for the
architecture and the BLE protocol it speaks.

The same deps set also provisions the NDK that
[`crates/shepherd-media-android`](crates/shepherd-media-android/) cross-compiles
its Rust cdylib against (via `cargo-ndk`); see that crate's README for its build.

Both apps are published to an F-Droid repository, whose listings live in
[`dist/fdroid/`](dist/fdroid/README.md). If you change them, validate against
real APKs before pushing a tag — `release.yml` runs the same command, and the
server that publishes the repository does not:

```sh
sudo apt install --no-install-recommends fdroidserver default-jdk-headless
./scripts/shepherd package fdroid --debug-keys    # --debug-keys: local debug-signed APKs
```

### Testing and linting

Run the test suite:

```sh
cargo test
# as run in CI:
cargo test --all-targets
```

Run lint checks:

```sh
cargo clippy
# as run in CI:
cargo clippy --all-targets -- -D warnings
```

### Bumping the version

`shepherd-launcher` is a composition of Rust crates, a web UI, an Android
companion app, and shell tooling — none of which depend on each other, but all
of which ship a version string. The canonical version lives in exactly one
place: the repo-root [`VERSION`](./VERSION) file.

* `scripts/shepherd` and the Android Gradle build **read** it directly, so they
  can never drift.
* Cargo and npm can't read a file at manifest-parse time, so their literals are
  **written** from `VERSION` by the bump command and **verified** by CI.

Bump every version at once:

```sh
./scripts/shepherd version set 0.2.0
```

Then commit `VERSION`, `Cargo.toml`, `Cargo.lock`, and
`shepherd-webui/package*.json` together. CI runs `shepherd version check` to
fail the build if any literal is edited by hand and drifts out of sync.

## Contribution guidelines

`shepherd-launcher` is licensed under the GPLv3 to preserve end-users' rights.
By submitting a pull request, you agree to license your contributions under the
GPLv3.

Contributions written in whole or in part by generative AI are allowed;
however, they will be reviewed as if you personally authored them. I highly
recommend adding substantial prompts and design docs provided to agents to
[docs/ai/history/](./docs/ai/history/) along with the PRs and commit hashes
associated with them.

The authors of `shepherd-launcher` do not condone software or media piracy.
Contributions that explicitly promote or facilitate piracy will be rejected.
Please support developers and creators by obtaining content legally.
