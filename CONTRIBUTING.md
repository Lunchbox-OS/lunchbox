# For developers

## Build for development

### tl;dr

You need a Wayland-capable Linux system, Rust, and a small set of system
dependencies. Once installed, `./run-dev` will start a development instance.

### Requirements

1. **Linux with Wayland**

   * Any modern Wayland compositor is sufficient. For Ubuntu, this means 25.10 or higher.
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
