# CI on GitHub Actions

> Status: **ported and green**, 2026-09-19. All 22 jobs pass on
> `ubuntu-latest` (run 35453898976, 29m wall clock). A verbatim port of the
> Forgejo workflows; speedups are recorded here but deliberately not attempted.
> Companion to `2026-09-19 001`.

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

Unchanged: every job, every assertion, every cache key, the `settle` job, and
the "Assert this job got the arm64 cross image" guards.

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

## Where the 29 minutes went

First run, so all three images were built cold; later runs skip that on a
content-hash hit.

| Job | | Job | |
|---|---:|---|---:|
| Test | 13m19s | Clippy | 4m33s |
| Package (arm64) | 12m03s | Android media | 4m25s |
| Package (amd64) | 11m46s | Android companion | 3m47s |
| Build (arm64 cross) | 10m00s | Config editor | 3m03s |
| E2E | 9m56s | Warm cargo registry | 2m25s |
| Build | 9m07s | Rustfmt | 2m06s |
| CI image | 7m51s | Web UI | 1m05s |
| Firewall E2E | 6m24s | ShellCheck | 14s |
| CI image (Android) | 5m23s | Arch neutrality | 9s |
| CI image (arm64 cross) | 4m37s | Version harmony, Workflow syntax, settle | <10s |

## Speedups available here that Forgejo could not do

Both workflows say, in their headers, that they avoid cross-job artifacts
because Forgejo does not implement the `upload-artifact@v4` protocol (the runner
reports as GHES and the action hard-fails). **GitHub implements it**, so the
constraint is lifted. Not acted on — recorded for when it is.

1. **`build` → `e2e`.** `e2e` opens with `./scripts/shepherd build`, compiling
   what `build` just compiled. Measured here: `Build` 9m07s, `E2E` 9m56s, of
   which almost all is that rebuild — the e2e suite itself finishes in seconds.
   Handing `target/debug` over as an artifact should take `E2E` to about a
   minute and take ~9 minutes off the critical path. This is the single largest
   win available. `test`, `lint` and `config-editor` are similar but not
   identical — each wants different `--all-targets` output.
2. **`release.yml`'s build→publish handoff.** Today `create-release` must make
   the release *first* so the parallel `deb` and `apk` jobs can each push their
   own asset into it. With artifacts the natural shape inverts: build jobs
   upload, one publish job creates the release and attaches everything. That
   also closes a real hole — right now a failed asset upload leaves a
   half-populated release behind.
3. **`config-editor`'s bundle.** The job explicitly notes it does not upload,
   because Forgejo could not. Uploading `dist-standalone/` would let a release
   reuse the PR's bundle instead of rebuilding it.
4. **`ubuntu-24.04-arm` runners exist now** and are free for public repos. The
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
turns into thrash. Passing artifacts between jobs is the fix, which makes item 1
above the first thing to do rather than the cheapest.

## Jobs that need more than this, flagged not fixed

* **`Firewall E2E` — expected to be the blocker, and is not.** It needs systemd
  as PID1, polkit, dbus, a writable cgroup hierarchy and BPF `cgroup_skb`
  attach, via a privileged container, and it passes in 6m24s. Worth recording
  what its log shows, because it answers a question the job's own comment leaves
  open: the plain leg found `/sys/fs/cgroup` **writable**, and the deliberately
  forced-read-only leg took the private-cgroup2-mount fallback
  (`/sys/fs/cgroup is not writable (… ro,nosuid,nodev,noexec …)`, remounted at
  `/tmp/shepherd-cgroup2-…`). So both paths are exercised here rather than one
  of them depending on which host the run lands on. Still the job to watch
  first if CI starts flaking.
* **Native arm64** — not in CI on either forge; correctness is validated by hand
  on an M1 VM. Cross-compilation buys no test coverage.
* **`release.yml`'s publishing** — not a runner limitation but an infrastructure
  one: `create-release` and `upload-release-asset.sh` talk to the Forgejo
  release API, and `publish-apt.sh` pushes into Forgejo's Debian registry. Both
  still live on `git.armeafamily.com`, and GitHub has no apt-registry
  equivalent. A `v*` tag pushed here would publish *there*. Flagged in the file;
  needs a decision, not a patch.
* **Nothing needs root that it cannot get.** The only root assumption that broke
  was ShellCheck's `apt-get`, fixed with `sudo`. Jobs with `container:` still run
  as root inside the container, so `test`'s `chown` + `sudo -u ci` dance and
  `package`'s fakeroot-free `dpkg-deb` are unaffected.
* **And nothing needed a newer kernel than it got.** Worth stating plainly
  because it was the expected blocker: `test` and `e2e` both pass with
  `SHEPHERD_REQUIRE_PEER_CGROUP=1`, and the only `ignored` test in the run is
  `input_devices::tests::scan_real_devices`, which is ignored by design. The
  peer-cgroup check ran.
