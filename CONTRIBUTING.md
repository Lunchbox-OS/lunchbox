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
   * View packages with: `./scripts/lunchbox deps print dev`
   * Install all dev dependencies: `./scripts/lunchbox deps install dev`
   * **Note**: Rust is automatically installed via rustup when installing build or dev dependencies.

### Unified script system

Lunchbox provides a unified script system for managing dependencies, building, and running:

```sh
# View and install dependencies
./scripts/lunchbox deps print dev        # List all dev dependencies
./scripts/lunchbox deps install dev      # Install all dev dependencies

# Build binaries
./scripts/lunchbox build                 # Debug build
./scripts/lunchbox build --release       # Release build

# Development
./scripts/lunchbox dev run               # Build and run in nested Sway
```

For CI/build-only environments:
```sh
./scripts/lunchbox deps install build    # Build dependencies only
./scripts/lunchbox build --release       # Production build
```

For runtime-only systems:
```sh
./scripts/lunchbox deps install run      # Runtime dependencies only
```

See `./scripts/lunchbox --help` for all available commands.

#### The launcher's typeface

`deps install dev` and `deps install run` also link **Baloo 2** — the display
face the launcher is branded with (issue #207) — from `assets/fonts` into your
own font directory and refresh the font cache. It is shipped in the repository
under the SIL Open Font License because no Ubuntu release packages it, and an
installed device gets it under `/usr/share/fonts` from `lunchbox install`
instead.

The launcher names it first in a fallback stack, so skipping this only means
ordinary lettering, not a broken screen. If the launcher renders in an ordinary
sans when you expect Baloo 2, suspect a **stale fontconfig cache** before a
missing file: run `fc-cache -f` and look again. Refreshing only the font's own
directory is not enough, and the failure is silent.

### Running in development

Start a development instance:

```sh
./run-dev
```

#### Adjusting the time

To avoid having to adjust the system clock or wait for timeouts, development
builds can mock the time with `LUNCHBOX_MOCK_TIME`:

```sh
LUNCHBOX_MOCK_TIME="2025-12-25 15:30:00" ./run-dev
```

Time and activity history are maintained in a SQLite database, which
`./run-dev` places at `./dev-runtime/data/lunchboxd.db`. Edit this database
using a tool like [DB Browser for SQLite](https://sqlitebrowser.org/) while the
service is not running to inject application usage. The schema is defined in
the [lunchbox-store crate](./crates/lunchbox-store/).

### Headless development (no login session, for SSH / CI / agents)

`./run-dev` boots a *nested* Sway that needs a graphical login session. To run
and **screenshot** the full stack without one — over SSH, in CI, or from a
coding agent — use the headless session, which boots the same `sway.conf`,
`config.example.toml`, and binaries against the GPU-less headless wlroots
backend:

```sh
./scripts/lunchbox deps install agent          # grim + wtype + jq (also in `deps install dev`)
./scripts/lunchbox dev headless                # build + boot, detached
./scripts/lunchbox dev tree                    # window tree (app_id / focus)
./scripts/lunchbox dev shot home.png           # screenshot the virtual output
./scripts/lunchbox dev key Down                # inject input; also: dev type / dev click
./scripts/lunchbox dev stop                    # tear down
```

Useful flags on `dev headless`: `--time "2025-12-25 21:00:00"` (mock the clock
for availability/bedtime/time-limit testing), `--config PATH` (boot an arbitrary
config), `--user NAME` (run the stack as another user — their groups, `HOME`, and
default `~/.config/lunchbox/config.toml`), `--size WxH`, `--gpu`, `--no-build`.
Connection state lives in `dev-runtime/headless/session.env`; the compositor log
is `dev-runtime/headless/sway.log`. See the design notes in
[`docs/ai/history`](./docs/ai/history/) for internals.

**Not usable for the management socket's peer check** (issue #144). `lunchboxd`
normally accepts a client on its own socket only from its own cgroup, which on a
device is the display manager's root-owned session scope — one no activity can
join. Started from a shell, the whole stack instead shares the launching
terminal's cgroup, inside the user manager's delegated subtree where anything at
this uid can join anything. So a dev session can neither pass the check
meaningfully nor fail it honestly, and every dev entry point passes
`--no-restrict-ipc-peers`; `dev headless --harden-ipc` deliberately does *not*
take that off, unlike the compositor unlink. The check itself is covered by unit
tests in `lunchbox-ipc`, which can put a peer in a cgroup of its own without a
session. To see it work end to end, run `lunchboxd` by hand without the flag and
connect from `systemd-run --user --scope`; the measurements are in
[`docs/ai/history/2026-08-29 002`](./docs/ai/history/).

**To get the production cgroup shape on a dev box**, install to a second user
and start the session through `su`, which is what makes logind create a real
session scope — the placement a display manager gives it:

```sh
sudo ./scripts/lunchbox install all --user kiosk
sudo su - kiosk -c 'exec env WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
    WLR_RENDERER=pixman WLR_RENDERER_ALLOW_SOFTWARE=1 XDG_SESSION_TYPE=wayland \
    sway -c /etc/sway/lunchbox.conf --unsupported-gpu'
```

The whole stack then lands in `/user.slice/user-<uid>.slice/session-<n>.scope`
(root-owned, unjoinable) and activities land in `app.slice` or `system.slice`,
so the peer check is a real boundary and `--no-restrict-ipc-peers` is not
needed. `sudo loginctl terminate-user kiosk` tears it down. This is a manual
recipe, not a harness: the session is headless, so drive it over the management
API rather than expecting `dev shot` to work against it. Measurements taken this
way are in [`docs/ai/history/2026-08-29 005`](./docs/ai/history/).

**Not usable for GPU performance work.** The headless session exports
`LIBGL_ALWAYS_SOFTWARE=1` for every client, so even `--gpu` (which only swaps
wlroots' own renderer) leaves the launcher, HUD and `lunchbox-media` on
llvmpipe. To measure anything that touches the GPU — video decode, compositing,
frame pacing — boot a real session on a spare VT instead:

```sh
sudo mkdir -p /run/lunchbox-perf && sudo chmod 700 /run/lunchbox-perf
sudo setsid openvt -c 3 -s -- env XDG_RUNTIME_DIR=/run/lunchbox-perf \
    LIBSEAT_BACKEND=builtin XDG_SESSION_TYPE=wayland sway -c sway.conf
# then, over SSH:
#   sudo env XDG_RUNTIME_DIR=/run/lunchbox-perf WAYLAND_DISPLAY=wayland-1 <client>
sudo pkill -x sway && sudo chvt 1     # teardown
```

`LIBSEAT_BACKEND=builtin` under `openvt` is what lets the session take DRM
master without a graphical login. See
[`docs/ai/history/2026-07-28 001 lunchbox-media performance investigation.md`](./docs/ai/history/)
for a worked example.

### Testing against a real kiosk session over SSH

Some things are not reachable from the headless session at all — anything that
turns on logind seeing a *graphical* session for the kiosk user: the state
custodian's peer check (#157), the session watchdog (#172), a firewalled
activity's transient scope. A dev stack passes `--no-state-custodian` and its
sway is not a logind graphical session, so those paths simply do not run.

What works, with no graphical login of your own, is driving gdm's autologin:

```sh
# 1. install this branch for the kiosk user (skip `install all` -- its
#    `install config` step overwrites the device's real policy with the example)
for step in bins "firewall --user lunchbox-kiosk" "state --user lunchbox-kiosk"             sway-config desktop-entry udev "groups --user lunchbox-kiosk"; do
    sudo ./scripts/lunchbox install $step
done
# 2. autologin, then restart gdm to make it take effect now
sudo sed -i '/^\[daemon\]/a AutomaticLoginEnable=true\nAutomaticLogin=lunchbox-kiosk' \
    /etc/gdm3/custom.conf
sudo systemctl restart gdm
# 3. teardown: put custom.conf back, then `sudo systemctl restart gdm`
```

Three things that will waste your time otherwise:

* **`systemctl restart gdm` does not end a session that is already running.** It
  kills the session *leader*, which orphans `lunchboxd` (nothing sets
  `PDEATHSIG`), and with `KillUserProcesses=no` logind then leaves the session in
  state `closing` waiting for the scope to empty. `TerminateSession` on an
  already-closing session is a no-op. End the session first
  (`loginctl terminate-session`), confirm it is gone, and only then restart gdm.
* **Do not suspend this VM.** On the libvirt host the only `/sys/power/mem_sleep`
  is `s2idle` and it does not come back — a `systemctl suspend` needed a hard
  reset. Anything that has to be measured across a suspend needs real hardware.
* **The agent scratchpad is under `/tmp`, which a reboot clears.** Back up the
  device's policy and database somewhere that survives one before you start.

### Web UI

The management API HTTP server (`lunchbox-http`) embeds the React SPA at compile
time from `lunchbox-webui/dist/`. Build it before building the Rust code:

```sh
cd lunchbox-webui
npm install       # once
npm run typecheck # tsc --noEmit; see below
npm run build     # generates lunchbox-webui/dist/
cd ..
cargo build       # dist/ is now embedded in the binary
```

`npm run build` goes through rsbuild, which **transpiles without checking
types** — a genuine type error compiles and ships. `npm run typecheck` is the
only thing that checks them, so run it alongside the build; CI runs it as its
own job.

During development, run the rsbuild dev server (which proxies API calls to
`https://localhost:8080` — the daemon serves TLS on any non-loopback bind since
issue #156, and the proxy is configured to accept its self-signed certificate)
instead of embedding:

```sh
lunchbox dev webui                    # hot-reloading, usually on port 3000
lunchbox dev webui --standalone       # config editor only, no daemon needed
lunchbox dev webui -- --port 3001     # anything after -- goes to rsbuild
```

This builds the config editor's wasm validator when it is missing, installs npm
dependencies on first run, and then hands off to rsbuild in the foreground —
Ctrl-C stops it. `npm run dev` from inside `lunchbox-webui/` does the same thing
without those two steps.

The Rust binary is still needed for the API; the dev server is only for the
frontend. If the web UI has not been built, lunchboxd still works normally — the
daemon just returns 404 for all non-API routes.

The management API requires a login (issue #156). A dev stack that has never
been signed into prints its setup code at startup and shows it on the device's
screen; it is also in `dev-runtime/data/web-auth.toml`. Enter it in the browser
to choose a password, or `rm` that file and restart to get back to a fresh
device. Scripts that want no browser can set `auth_token` under
`[service.management_api]` and send `Authorization: Bearer` — that token
authenticates a request and deliberately cannot open a session.

Unit tests, typechecking and the import boundary check:

```sh
cd lunchbox-webui
npm test             # vitest
npm run typecheck    # tsc --noEmit
npm run check:boundary
npm run check:coverage
```

Most tests are pure logic — day masks, window merging, duration parsing — and
run in plain node. A few need a DOM and opt in with `// @vitest-environment
jsdom` at the top of the file, so the pure ones stay fast.

**Those need Node 22 or newer** (`engines` in `package.json` says so, and CI
pins its container accordingly). jsdom loads undici, which needs
`worker_threads.markAsUncloneable`; on an older Node the DOM test files fail to
load with `TypeError: webidl.util.markAsUncloneable is not a function`, while
every pure test still passes — so the run reports a smaller number of passing
files rather than anything that looks like a version problem. Those cover
behaviour that only appears *across a mount*, which no static check can see:
the editor's pages are conditionally rendered, so switching tabs unmounts one
and returning mounts it fresh, and a mount runs every effect regardless of its
deps. Both navigation bugs found so far were of that shape.

Testing Library only registers its own cleanup when Vitest's `globals` are on,
and they are not, so a DOM test must `afterEach(cleanup)` itself or every query
will find two of everything.

### Generated client types

The payload types both clients use are **generated** from the Rust definitions,
not hand-written. Change `crates/lunchbox-api/src/types.rs` (adding a type to
`WireTypes` in `crates/lunchbox-wire-codegen/src/wire_schema.rs` if it is only
reachable as an RPC parameter), then:

```sh
cargo run -p lunchbox-wire-codegen --bin rpc-codegen
```

That rewrites every checked-in mirror: `docs/rpc-schema.json`, the two
method-name mirrors, the payload mirrors for each client —
`companion-android/.../WireTypes.generated.kt` and
`lunchbox-webui/src/api/wire-types.generated.ts` — the config editor's mirrors
of the `config.toml` schema, `lunchbox-webui/src/config/model/config.generated.ts`,
and the two *value* mirrors described below. Editing any of them by hand is
pointless; the next run overwrites it, and `tests/rpc_codegen_drift.rs` fails
until the regenerated output is committed.

The last of those comes from `crates/lunchbox-config/src/schema.rs` rather than
the wire types, but goes through the same renderer: `ts_types.rs` takes a
schema and a preamble, so the only thing that differs between the two outputs is
which Rust file the banner tells you to edit. It refuses, loudly, to render a
schema shape it does not recognise rather than emitting a plausible mirror —
so an exotic serde attribute on either side fails codegen instead of quietly
producing types that typecheck and decode wrongly.

**Doc-comment a fieldless enum's variants either all or none.** `schemars`
renders a unit-only enum as a plain `"enum": [...]` array when no variant
carries a doc comment, and as a `oneOf` of string `const`s when they all do.
Document *some* of them and it emits a mix — the documented ones as `const`s,
each undocumented run collapsed into one `enum` entry — which is neither shape
the Kotlin renderer knows, so codegen fails with

```
NetworkInterfaceKind is an untagged or externally-tagged enum with no Kotlin
equivalent; add it to HAND_WRITTEN in kotlin_types.rs
```

That message names the wrong fix for this cause. The right one is to document
the remaining variants (or none of them); `HAND_WRITTEN` is for enums that
genuinely have no Kotlin equivalent.

### Generated defaults

Two of the generated files carry *values* rather than types: what a field falls
back to when the config leaves it out, which the editor has to show so an unset
control reads as what the daemon will actually do.

| File | Answers |
| --- | --- |
| `kind-defaults.generated.ts` | What an entry's **kind** supplies for a field on the entry (`confirm_on_close`, `input_compat`) |
| `field-defaults.generated.ts` | What each **field** falls back to, in three tables |

The second has three tables because the daemon applies defaults two ways, and
only one of them is visible to `schemars`:

- `FIELD_DEFAULTS` and `KIND_FIELD_DEFAULTS` — serde defaults
  (`#[serde(default = "…")]`). These reach the JSON Schema on their own, so
  adding one needs no work beyond re-running the generator.
- `LOAD_TIME_DEFAULTS` — `Option<T>` fields whose `None` means "fall back",
  resolved in `Policy::from_raw` long after deserialization. `schemars` sees
  only `"default": null` for these, so they come from
  `crates/lunchbox-config/src/load_defaults.rs`. **Adding one means adding a
  field there too**, and its module doc explains what belongs (and what
  deliberately does not — a percentage slider's `0`/`100` extents are not a
  default).

`load_defaults.rs` carries a test that parses a config setting none of these
fields and asserts the advertised values are what the parser actually produces.
That catches a call site drifting from its constant, which is how the editor and
the daemon came to disagree in the first place. It cannot catch *changing* a
constant — both sides read the same one — but a change flows into the generated
TypeScript on the next codegen run, and the drift test fails until someone makes
that run.

About forty of these were spelled out in the editor by hand before this existed:
a `?? true`, a `?? "kiosk"`, a `const DEFAULT_COOLDOWN_MIN_SESSION = 120`, a
`placeholder="2m"`. `lunchbox-webui/src/config/defaults.test.tsx` renders real
controls with nothing set and asserts the generated value comes out, because the
drift test can only see the file's contents, not whether any component reads it.

The mirrors were hand-written once and drifted: four `ReasonCode` variants went
missing from the companion, and a renamed `DailyOverride` field went unnoticed
until it broke every override lookup on the phone. Neither was catchable from
the method schema alone, and neither compiler could see it — `tsc` checks
TypeScript against TypeScript, and Kotlin against Kotlin.

Note that the codegen crate is deliberately outside `default-members`, so
`cargo build` never compiles `schemars` into the shipped binaries. Reaching its
tests therefore needs `cargo test --workspace` (which is what CI runs); a bare
`cargo test` silently skips the drift check.

`lunchbox-webui/src/api/types.ts` re-exports the generated types and keeps only
the presentation helpers, so the rest of the UI still imports wire shapes from
one place.

### Config editor

A graphical editor for `config.toml` lives in
[`lunchbox-webui/src/config/`](lunchbox-webui/src/config/) and builds two ways
from one source:

| Target | Build | Dev server | Output |
|---|---|---|---|
| Standalone static site | `lunchbox build config-editor` | `lunchbox dev webui --standalone` | `dist-standalone/`, for a static host |
| Embedded in lunchboxd | `npm run build` | `lunchbox dev webui` | `dist/` — the management UI, whose **Config** tab is this editor |

Which config it edits is a prop, not a build flag. The standalone bundle passes
`FileConfigSource` (files on whatever computer is doing the browsing); the
management UI passes `DeviceConfigSource`
([`src/sources/`](lunchbox-webui/src/sources/)), which reads and writes *this
device's* policy over `GET`/`PUT /api/v1/config`. Everything in between asks
the source what it can do rather than asking which build it is in.

`DeviceConfigSource` lives outside `src/config/` because
`scripts/check-boundary.mjs` forbids that tree from importing `src/api/` — it
also builds into the standalone bundle, which has no daemon to talk to.

Routing the editor is what puts its chunks and its ~950 kB wasm validator into
`dist/`, and so into the binary `rust-embed` builds from it: about +1.5 MB on
`dist/` and +5% on a release `lunchboxd`. Both are lazy chunks, so a browser
that never opens the tab never fetches them.

The two **must** write different directories — anything left in `dist/` is
compiled into the daemon binary by `rust-embed`. `rsbuild.config.ts` switches on
`LUNCHBOX_UI_TARGET`, and `output.distPath` is resolved relative to the working
directory, so always run the npm scripts from inside `lunchbox-webui/`.

The editor validates with the daemon's own parser, compiled to WebAssembly from
[`crates/lunchbox-config-wasm`](crates/lunchbox-config-wasm/), rather than a
TypeScript reimplementation of `validation.rs`. That crate also holds the
`toml_edit` document model that makes editing comment-preserving. Build the wasm
artifact before the npm build:

```sh
./scripts/lunchbox build config-wasm   # wasm-pack -> src/config/wasm/
```

`lunchbox build config-editor` and `lunchbox dev webui` both do this for you when
the artifact is missing; pass `--wasm` to the latter to force a rebuild after
changing the crate. `src/config/wasm/` is generated and gitignored;
`npm run typecheck` needs it to exist.

**Rebuild it after changing the config schema, too** — not only after changing
`lunchbox-config-wasm` itself. The artifact embeds the parser, so a stale one
does not know a field you have just added: the editor writes it to the document
happily (that path is `toml_edit`, which needs no schema) and then drops it when
parsing back, so a new control renders, refuses to hold its value, and reports
nothing wrong. `tsc` and the component tests do not catch it, because neither
runs the wasm.

#### Hosting

The standalone bundle is published to Cloudflare Pages at
<https://config.lunchbox-os.com>, from the `config-editor` job in
[`release.yml`](.github/workflows/release.yml). It deploys on `vX.Y.Z` tags
rather than on every push to main, so the hosted editor matches the last
released Lunchbox — it renders a `config_version` that ships with the daemon,
and an editor ahead of the release would offer fields the installed version
cannot read. Prerelease tags (`v0.4.0-rc1`) are skipped.

Direct Upload, so Cloudflare needs no access to the repo. Two secrets:
`CLOUDFLARE_API_TOKEN` (with the "Cloudflare Pages: Edit" permission) and
`CLOUDFLARE_ACCOUNT_ID`.

It gets its own subdomain rather than a path under `lunchbox-os.com`,
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
  `cargo run -p lunchbox-wire-codegen --bin rpc-codegen`, into
  `src/config/model/config.generated.ts`. A drift test fails CI if the
  checked-in copy goes stale, and `npm run check:coverage` fails if a generated
  field is never referenced under `src/config/` — a field the editor cannot set
  is a field nobody can set. That check works on field *names*, so it does not
  catch a sub-table wired to one parent but not another; adding a `Raw*` table
  to a second owner stays a manual check.

The document model is plain Rust with strings on its edges, so it tests
natively without a browser:

```sh
cargo test -p lunchbox-config-wasm
```

`crates/lunchbox-config-wasm/tests/preservation.rs` is the suite that matters:
it holds the line on editing `config.example.toml` without disturbing a comment.

### Android companion app

The BLE management companion app lives in [`companion-android/`](companion-android/)
and is independent of the Rust build. Install its toolchain (JDK + Android SDK +
NDK into `/opt/android-sdk`) with the dedicated deps set, then build:

```sh
./scripts/lunchbox deps install android   # JDK 21 + Android SDK + NDK
cd companion-android
export ANDROID_SDK_ROOT=/opt/android-sdk   # see below
./gradlew :app:assembleDebug               # debug APK (sideload-friendly)
./gradlew :app:testDebugUnitTest           # unit tests
```

**Gradle here needs JDK 21, and the system default may be newer.** Ubuntu 26.04
ships JDK 25 as `default-java`; the Gradle wrapper this repo pins (8.10.2)
cannot parse that version string and every task dies with a bare

```
* What went wrong:
25.0.4
```

— no mention of Java, Gradle, or a version. Export the JDK the Android deps set
installs before invoking the wrapper directly:

```sh
export JAVA_HOME=/usr/lib/jvm/java-21-openjdk-amd64
```

Gradle does not find the SDK on its own here: `deps install android` puts it in
`/opt/android-sdk` rather than the `~/Android/Sdk` the toolchain probes by
default, and this repo has no checked-in `companion-android/local.properties`
(it is git-ignored). Without `ANDROID_SDK_ROOT` — or `ANDROID_HOME`, or an
`sdk.dir` line in that `local.properties` — every Gradle task fails in a few
seconds with `SDK location not found`. CI does not hit this because the Android
job runs in a container image that already exports it, so a green CI run is no
evidence that a bare `./gradlew` works in your shell. The `./scripts/lunchbox`
wrappers set it for you; invoking `./gradlew` directly is what needs the export.

To build *and* push it to a phone/tablet/Fire TV attached over `adb` in one
step (from a checkout this builds; from an installed `.deb` the same command
downloads the matching signed release instead):

```sh
./scripts/lunchbox apps install companion   # or: media — no sudo, adb keys are per-user
```

See [`companion-android/README.md`](companion-android/README.md) for the
architecture and the BLE protocol it speaks.

The same deps set also provisions the NDK that
[`crates/lunchbox-media-android`](crates/lunchbox-media-android/) cross-compiles
its Rust cdylib against (via `cargo-ndk`); see that crate's README for its build.

Both apps are published to an F-Droid repository, whose listings live in
[`dist/fdroid/`](dist/fdroid/README.md). If you change them, validate against
real APKs before pushing a tag — `release.yml` runs the same command, and the
server that publishes the repository does not:

```sh
sudo apt install --no-install-recommends fdroidserver default-jdk-headless
./scripts/lunchbox package fdroid --debug-keys    # --debug-keys: local debug-signed APKs
```

### Cross-compiling for arm64 (aarch64)

`--arch` takes a Debian architecture name and applies to both the build and the
package:

```sh
./scripts/lunchbox deps install cross --arch arm64   # one-time; ~1.5-2.5 GB
./scripts/lunchbox build --arch arm64                # target/aarch64-unknown-linux-gnu/
./scripts/lunchbox package deb --arch arm64          # dist/pkg/..._arm64.deb
```

`deps install cross` installs the cross toolchain, the target architecture's
half of `deps/build.pkgs` (Ubuntu's multiarch makes it co-installable with the
host's own), and rustc's std for the derived triple. It tells dpkg about the
architecture and, only if the configured mirror does not already serve it, adds
an entry for Ubuntu's ports mirror — on 26.04 the main archive carries arm64
too, so that is decided by probing rather than assumed.

**It will probably refuse, and that is the useful part.** The two
architectures' `-dev` chains are not co-installable here: `libmpv-dev` depends
on `libcdio-dev`, `libext2fs-dev`, `libgirepository1.0-dev` and `libtool-bin`,
several of which are `Multi-Arch: no`, so apt makes room by removing the host's
half — and `apt-get install -y` does that silently and exits 0. The native
build then fails at link time with `cannot find -lmpv`, a long way from
anything that mentions cross-compiling. `deps install cross` simulates the
install first and stops rather than let that happen.

So on a machine you also build natively on, **cross-compile in a container**
(that is what CI does — see `.ci/Dockerfile.cross`). If you would rather take
the trade on the host, pass `--allow-remove`, and restore the native set
afterwards with `lunchbox deps install build`.

Installing a foreign architecture's libraries also prints a handful of
`Exec format error` lines from their `postinst` scripts (glib schemas,
gdk-pixbuf loaders). Those are expected on a multiarch host with no emulator,
and harmless: the helpers matter to *running* that architecture's software, not
to compiling against its headers.

An `--arch` naming the **host's own** architecture builds natively, into the
usual `target/{debug,release}`, so it is a no-op rather than a second target
directory. `build --target <triple>` forces the triple path when you want it.

Two things not to do:

- **Do not export `CARGO_BUILD_TARGET`.** It beats
  `crates/lunchbox-firewall-bpf/.cargo/config.toml` and makes the eBPF program
  build for your triple instead of `bpfel-unknown-none`. `lunchbox build`
  passes `--target` on the command line for this reason, and
  `lunchbox-firewall-helper`'s build script strips the variable.
- **Do not treat a cross build as tested.** It buys no coverage at all: nothing
  in `cargo test`, `lunchbox-e2e`, the firewall BPF suites or the headless
  session runs on the target architecture. The BPF path is the most exposed,
  because the verifier runs on the *target* kernel — see issue #151.

### Running the suites natively on arm64

Because of that last point, correctness on arm64 is covered by a native
aarch64 Ubuntu 26.04 machine rather than by the cross build. Any aarch64 host
will do — a VM on an Apple-silicon Mac is what this project uses — and it needs
no special setup beyond the usual:

```sh
./scripts/lunchbox deps install dev
cargo test --workspace --all-targets
cargo test -p lunchbox-e2e -- --include-ignored --test-threads=1
./scripts/lunchbox dev headless && ./scripts/lunchbox dev shot && ./scripts/lunchbox dev stop
```

Set `LUNCHBOX_REQUIRE_PEER_CGROUP=1` for the e2e run, so a kernel too old for
the socket peer check fails rather than skips (issue #144).

The firewall suites need **root**, and self-skip without it — reporting `ok` in
0.00s, which reads exactly like a pass. Run them the way `ci.yml` does, with
`LUNCHBOX_FIREWALL_CGROUP_REQUIRED=1` to turn "not applicable here" into a
failure:

```sh
sudo -E env "PATH=$PATH" LUNCHBOX_FIREWALL_CGROUP_REQUIRED=1 \
    cargo test -p lunchbox-e2e --test firewall_cgroup -- \
        --include-ignored --test-threads=1 --nocapture
```

If `sudo` asks for a password despite a `NOPASSWD` rule, check the *order*:
`sudo -l` applies the **last** matching rule, and Ubuntu's `/etc/sudoers` ends
with `@includedir /etc/sudoers.d`, so a NOPASSWD rule must live in a file there
to beat the `%sudo` group rule.

Results from the first such run are in
[`docs/ai/history/2026-09-04 002`](./docs/ai/history/).

### Testing and linting

Run the test suite:

```sh
cargo test
# as run in CI:
cargo test --workspace --all-targets
```

Run lint checks:

```sh
cargo clippy
# as run in CI:
cargo clippy --workspace --all-targets -- -D warnings
```

`--workspace` is the part that matters: `lunchbox-config-wasm` and
`lunchbox-wire-codegen` are kept out of `default-members` so `cargo build` never
compiles `wasm-bindgen` or `schemars` into the shipped binaries, and the side
effect is that a bare `cargo test` skips them silently — including the codegen
drift check.

Shell scripts are linted with ShellCheck, as run in CI:

```sh
shellcheck -e SC1091 scripts/lunchbox scripts/lunchbox-admin scripts/dev scripts/admin
shellcheck -e SC1091 scripts/lib/*.sh scripts/ci/*.sh
shellcheck -e SC1091 run-dev
```

**CI's ShellCheck is older than yours.** The job installs the runner's Ubuntu
24.04 package (0.9) — it was Debian bookworm's, also 0.9, while the job still ran
in a container — while Ubuntu 26.04 ships 0.11, and the two disagree on some codes —
0.10 split SC2329 ("function never invoked") out of SC2317 ("command
unreachable"), so a `disable=SC2329` that satisfies 0.11 does nothing for 0.9.
Where a disable is genuinely needed for a check that was renumbered, list both
codes. To reproduce CI exactly, run the 0.9.0 release binary from
<https://github.com/koalaman/shellcheck/releases/tag/v0.9.0>.

Also, a comment line that *begins* with the word `shellcheck` is parsed as a
directive, so prose that wraps onto one fails with SC1073.

### Editing a workflow file

Run this before pushing:

```sh
./scripts/ci/check-workflows.sh
```

Two jobs in one file may not share a name. Forgejo rejects the *entire*
workflow when they do — "mapping key ... already defined at line ..." — and
nothing in it runs, so there is no failing job to point at the mistake and no
CI job that can catch it for you. A merge is how it happens: two branches each
add a job, they land far enough apart that git merges both without a conflict,
and the result is a clean diff and a dead workflow.

Note that `python3 -c "import yaml; yaml.safe_load(...)"` will **not** catch it.
PyYAML accepts duplicate keys and keeps the last one; Forgejo's parser does not.

### Bumping the version

Lunchbox is a composition of Rust crates, a web UI, an Android
companion app, and shell tooling — none of which depend on each other, but all
of which ship a version string. The canonical version lives in exactly one
place: the repo-root [`VERSION`](./VERSION) file.

* `scripts/lunchbox` and the Android Gradle build **read** it directly, so they
  can never drift.
* Cargo and npm can't read a file at manifest-parse time, so their literals are
  **written** from `VERSION` by the bump command and **verified** by CI.

Bump every version at once:

```sh
./scripts/lunchbox version set 0.2.0
```

Then commit `VERSION`, `Cargo.toml`, `Cargo.lock`, and
`lunchbox-webui/package*.json` together. CI runs `lunchbox version check` to
fail the build if any literal is edited by hand and drifts out of sync.

## Contribution guidelines

Lunchbox is licensed under the GPLv3 to preserve end-users' rights.
By submitting a pull request, you agree to license your contributions under the
GPLv3.

Contributions written in whole or in part by generative AI are allowed;
however, they will be reviewed as if you personally authored them. I highly
recommend adding substantial prompts and design docs provided to agents to
[docs/ai/history/](./docs/ai/history/) along with the PRs and commit hashes
associated with them.

The authors of Lunchbox do not condone software or media piracy.
Contributions that explicitly promote or facilitate piracy will be rejected.
Please support developers and creators by obtaining content legally.
