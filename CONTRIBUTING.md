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
npm install      # once
npm run build    # generates shepherd-webui/dist/
cd ..
cargo build      # dist/ is now embedded in the binary
```

During development, you can run the rsbuild dev server (which proxies API calls
to `localhost:8080`) instead of embedding:

```sh
cd shepherd-webui
npm run dev      # hot-reloading dev server, usually on port 3000
```

The Rust binary is still needed for the API; the dev server is only for the
frontend. If the web UI has not been built, shepherdd still works normally — the
daemon just returns 404 for all non-API routes.

### Android companion app

The BLE management companion app lives in [`companion-android/`](companion-android/)
and is independent of the Rust build. Install its toolchain (JDK + Android SDK +
NDK into `/opt/android-sdk`) with the dedicated deps set, then build:

```sh
./scripts/shepherd deps install android   # JDK 21 + Android SDK + NDK
cd companion-android
./gradlew :app:assembleDebug               # debug APK (sideload-friendly)
./gradlew :app:testDebugUnitTest           # unit tests
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
