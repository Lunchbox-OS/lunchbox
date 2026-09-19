# CI on GitHub Actions

> Status: **ported, green, then tuned**, 2026-09-19. Written in two passes, and
> it reads that way on purpose: first a verbatim port of the Forgejo workflows
> with the speedups only *recorded*, then those speedups implemented. Where a
> section says something was not acted on, the "What the tuning actually did"
> section near the end says what happened when it was. Companion to
> `2026-09-19 001`.

## Prompt

> then get CI working -- you may push as needed to validate workflows. for now,
> just try to do a verbatim migration, but note opportunities for speedups by
> passing artifacts between jobs. point out jobs that cannot be ported as-is
> because they require root, a newer kernel, or similar, but don't try to fix
> those yet.

## What a GitHub-hosted runner actually gives us

Measured, not assumed — `ubuntu-latest` on 2026-09-19:

| | |
|---|---|
| Image | `ubuntu-24.04`, runner 2.337.0, Azure eastus |
| Kernel | **6.17.0-1022-azure** |
| CPU / RAM / disk | 4 cores, 15 GB, 145 GB (86 GB free) |
| User | `runner`, **not root**; `sudo` available |
| Docker | 28.0.4, running on the host, cgroup v2, systemd driver |
| `DOCKER_HOST` | unset — no sidecar |
| `GITHUB_TOKEN` | `Packages: write`; `docker login ghcr.io` succeeds |

Two of those settle questions the port hinged on, and both came out in our
favour:

* **The kernel clears the floor.** `test` and `e2e` set
  `SHEPHERD_REQUIRE_PEER_CGROUP=1`, which turns a skipped peer-cgroup check
  into a failure (issue #144). That check needs `SO_PEERPIDFD` (Linux 6.5) and
  `PIDFD_GET_INFO` (Linux 6.13). At 6.17 both are present, and a container gets
  the host kernel, so these jobs keep their teeth. This was the single biggest
  risk in the port, since the floor is above several LTS kernels — a
  self-hosted runner on Ubuntu 24.04's 6.8 would fail both jobs.
* **`--privileged` is permitted.** So the `firewall` job's shape survives, and
  the full job then passed: systemd reached `running`/`degraded` inside the
  container, `firewall_cgroup` and `firewall_real` both passed, and the
  `grep -q private.cgroup2.mount` assertion held.

## What changed, and only what had to

Forgejo ran every job inside `node:20-bookworm` as root, against a
`docker:dind` sidecar. GitHub runs jobs on a VM as `runner`, with docker
already there. Everything below follows from that one difference.

1. **Registry: Forgejo → GHCR.** `ghcr.io/aarmea/lunchbox-ci{,-android,-cross-arm64}`.
2. **`REGISTRY_TOKEN` deleted.** The ambient `GITHUB_TOKEN` with
   `permissions: packages: write` both pushes the images and pulls them for
   `container:`. The PAT existed because Forgejo's registry authenticated the
   ambient token for read but 401'd on push.
3. **The DinD preamble deleted** from all four jobs that carried it (three
   image jobs + `firewall`): no `apt-get install docker.io`, no `DOCKER_HOST`
   bridge-gateway discovery, no "Wait for DinD daemon".
4. **ShellCheck installs under `sudo`.**

Unchanged at this point: every job, every assertion, every cache key, the
`settle` job, and the "Assert this job got the arm64 cross image" guards. (The
`settle` job and the shared cache keys did not survive the tuning pass below.)

### The one that bit

Deleting the DinD steps **silently deleted the checkout**. In the original the
order was `Install docker CLI` → `Point DOCKER_HOST` → `- uses:
actions/checkout@v4` → `Compute image tag`, and a step-removal pass that
recognised only `- name:` as the next-step boundary swallowed the bare `- uses:`
line with the step before it. The image jobs then hashed five files that were
not there. That hash is `e3b0c44298fc` — `sha256("")` — so the failure surfaced
as `docker build` not finding `.ci`, several steps after the actual damage. If
an image ref ever carries `e3b0c44298fc`, the tree was empty, not the Dockerfile
wrong.

## Where the time goes

Two runs: the first built all three images cold, the second hit them
(`Image already published`) and reused warm `target/` caches. The warm column is
the one to reason about.

| Job | cold | warm |
|---|---:|---:|
| Package (.deb, arm64) | 12m03s | **5m46s** |
| Package (.deb, amd64) | 11m46s | 4m57s |
| Firewall E2E | 6m24s | **6m42s** |
| E2E | 9m56s | 4m42s |
| Build (arm64 cross) | 10m00s | 4m13s |
| Test | 13m19s | 3m37s |
| Android companion | 3m47s | 4m03s |
| Config editor | 3m03s | 3m16s |
| Build | 9m07s | 3m13s |
| Android media | 4m25s | 2m17s |
| Clippy | 4m33s | 2m17s |
| Warm cargo registry | 2m25s | 1m53s |
| Rustfmt | 2m06s | 1m38s |
| Web UI | 1m05s | 58s |
| CI image / Android / cross | 7m51s / 5m23s / 4m37s | **7s / 6s / 7s** |
| ShellCheck, Version harmony, Arch neutrality, Workflow syntax, settle | <15s | <15s |
| **Wall clock** | **29m** | **8.2m** |

The critical path on a warm run is `warmup` (1m53s) → `Package (arm64)` (5m46s).
`Firewall E2E` is the longest single job at 6m42s but only `needs: images`, so it
starts immediately and is not on the path.

Note which jobs do *not* get faster when warm: `Firewall E2E`, `Android
companion`, `Config editor`. The first builds the workspace *inside* its
privileged container, which has no route to `actions/cache` — so it pays a cold
compile on every run, forever.

## Speedups available here that Forgejo could not do

Both workflows say, in their headers, that they avoid cross-job artifacts
because Forgejo does not implement the `upload-artifact@v4` protocol (the runner
reports as GHES and the action hard-fails). **GitHub implements it**, so the
constraint is lifted. _Written before any of it was done; all five were acted on
afterwards, and the results are below._

1. **`build` → `e2e`.** `e2e` opens with `./scripts/shepherd build`, compiling
   what `build` just compiled. Worth being careful about the size of this one:
   cold it looks like ~9 minutes, but warm `E2E` is 4m42s against `Build`'s
   3m13s, so the duplicated work is nearer 3 minutes than 9. Still worth having,
   and it removes a whole `target/` cache from the quota below. `test`, `lint`
   and `config-editor` are similar but not identical — each wants different
   `--all-targets` output.
2. **`build` → `firewall`.** The bigger prize, and the one caching cannot touch.
   `Firewall E2E` runs `./scripts/shepherd build` inside its privileged
   container, which cannot reach `actions/cache`, so it compiles the workspace
   from scratch on every single run — 6m42s warm, the longest job in the suite.
   The workspace already crosses into the container by tar-pipe; sending
   `target/debug` the same way, from a `build` artifact, would cut most of it.
3. **`release.yml`'s build→publish handoff** — done, though the file is now
   commented out, so it lands whenever publishing is settled. `create-release` had to make
   the release *first* so the parallel `deb` and `apk` jobs can each push their
   own asset into it. With artifacts the natural shape inverts: build jobs
   upload, one publish job creates the release and attaches everything. That
   also closes a real hole — right now a failed asset upload leaves a
   half-populated release behind.
4. **`config-editor`'s bundle.** The job explicitly notes it does not upload,
   because Forgejo could not. Uploading `dist-standalone/` would let a release
   reuse the PR's bundle instead of rebuilding it.
5. **`ubuntu-24.04-arm` runners exist now** and are free for public repos. The
   arm64 story is currently cross-compile-only, with native correctness checked
   by hand on an M1 VM (see `2026-09-04 002`). A native arm64 leg is now
   possible in CI. Not a speedup — a coverage gain.

### A caching risk that is new here, and worth acting on first

**GitHub gives a repository 10 GB of Actions cache, with LRU eviction.** This
workflow keeps *nine* separate `target/` caches by design — per-job keys so
`cargo test --all-targets`, `cargo clippy --all-targets` and a plain build stop
clobbering each other's save — plus the shared dep registry and the Gradle
cache. A Rust workspace this size puts each `target/` in the low gigabytes, so
the set cannot fit, and they will evict each other run after run. The per-job-key
design was the right answer to a save race; under a hard repo-wide quota it
turns into thrash. Passing artifacts between jobs is the fix, which makes items 1
and 2 above the first things to do rather than the cheapest. (The warm run did
hit its caches, so this is a ceiling being approached, not a fire.)

## What the tuning actually did

Measured, on the same workflow, after implementing everything above:

| | Before | After |
|---|---:|---:|
| Actions cache | 10.49 GB / 9 caches | **8.06 GB / 8** |
| `Firewall E2E` | 6m42s | **4m00s** |
| `Android companion` | 4m03s | **2m19s** |
| Base image → first consumer starting (cold) | 5m36s | **3s** |
| Wall clock, warm | 8.2m | 9.8m |

What changed, and the parts worth keeping in mind:

* **Debug info is the cache, mostly.** `CARGO_PROFILE_DEV_DEBUG:
  line-tables-only`, set in the workflow so a local `cargo build` is unaffected,
  took ~30% off every `target/` cache — not the ~50% guessed above.
* **Merging `lint` into `test` barely helped the cache**, though it did remove a
  duplicate compile: 2.91 + 0.64 GB became a single 3.48 GB, because the two
  jobs' artifacts are additive rather than overlapping.
* **The Gradle cache had never worked.** Not eviction — `~/.gradle/...` was not
  where Gradle wrote, so every run ended with `Path(s) specified in the action
  for caching do(es) not exist, hence no cache is being saved` and re-downloaded
  the dependency set. That is why the job took 227s cold and 243s warm. Fixed by
  setting `GRADLE_USER_HOME` explicitly and caching that same absolute path.
* **`firewall` gets `target/debug` from `build`.** It still compiles 70 crates in
  24s — dev-dependencies and test targets, which `cargo build` never produces —
  against ~200s for the full build it used to do.
* **Unpacking that artifact needs `tar -xzf`, not `-xf`.** tar auto-detects
  compression from a file but not from stdin, and the firewall job pipes it in:
  `tar: Archive is compressed. Use -z option`. `e2e` reads the same artifact from
  a file and needed no flag, so this failed in exactly one of the two consumers.
* **`settle` is gone**, and with it the reusable-workflow call that built all
  three images as a unit. `needs:` on such a call is all-or-nothing, so every job
  waited for every image: on a cold build the base was ready at 16:14:35 and
  nothing started until 16:20:11. `images.yml` became `image.yml`, one image per
  `kind` input, instantiated three times.
* **Wall clock did not improve**, which is the honest headline. The critical path
  is `warmup` → `Package (arm64)`, and none of this touches it. The wins are
  cache headroom, cold-start latency on an image rebuild, and work that was being
  done twice.

## Jobs that need more than this, flagged not fixed

* **`Firewall E2E` — expected to be the blocker, and is not.** It needs systemd
  as PID1, polkit, dbus, a writable cgroup hierarchy and BPF `cgroup_skb`
  attach, via a privileged container, and it passes (6m24s at the time of
  writing, 4m00s once it stopped rebuilding the workspace). Worth recording
  what its log shows, because it answers a question the job's own comment leaves
  open: the plain leg found `/sys/fs/cgroup` **writable**, and the deliberately
  forced-read-only leg took the private-cgroup2-mount fallback
  (`/sys/fs/cgroup is not writable (… ro,nosuid,nodev,noexec …)`, remounted at
  `/tmp/shepherd-cgroup2-…`). So both paths are exercised here rather than one
  of them depending on which host the run lands on. Still the job to watch
  first if CI starts flaking.
* **Native arm64** — not in CI on either forge; correctness is validated by hand
  on an M1 VM. Cross-compilation buys no test coverage.
* **`release.yml` — commented out in full, not ported.** Not a runner
  limitation but an infrastructure one: `create-release` and
  `upload-release-asset.sh` speak the Forgejo release API, and `publish-apt.sh`
  pushes into Forgejo's Debian registry. Both still live on
  `git.armeafamily.com`, and GitHub has no apt-registry equivalent. After the
  move those endpoints resolve against `github.server_url` — `https://github.com`
  — with Forgejo's `/api/v1/...` paths appended, so there is no version of the
  file that works until the destinations are chosen.

  Commented rather than deleted, because the build half is correct and worth
  keeping: the tag-vs-VERSION guard, the per-arch `.deb`, the signed APK matrix,
  the F-Droid metadata validation, and the build → artifacts → single `publish`
  restructuring. What it is waiting on is recorded as #203 (sign the `.deb`),
  #204 (Launchpad PPA) and #205 (f-droid.org proper).

  The 18 `v*` tags are deliberately still local. Pushing them while this file was
  live would have fired 18 release runs, each building both `.deb`s and both
  APKs before failing at `publish`.
* **Nothing needs root that it cannot get.** The only root assumption that broke
  was ShellCheck's `apt-get`, fixed with `sudo`. Jobs with `container:` still run
  as root inside the container, so `test`'s `chown` + `sudo -u ci` dance and
  `package`'s fakeroot-free `dpkg-deb` are unaffected.
* **And nothing needed a newer kernel than it got.** Worth stating plainly
  because it was the expected blocker: `test` and `e2e` both pass with
  `SHEPHERD_REQUIRE_PEER_CGROUP=1`, and the only `ignored` test in the run is
  `input_devices::tests::scan_real_devices`, which is ignored by design. The
  peer-cgroup check ran.
