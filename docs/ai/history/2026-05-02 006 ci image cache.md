# 2026-05-02 — Pre-baked CI image to fix runner OOM

## Prompt

> Go through the CI results at
> <https://git.armeafamily.com/albert/shepherd-launcher/pulls/32> and fix
> the failures
>
> *(after first reply, which proposed chaining jobs via `needs:`)*
>
> I'd actually like to address the root cause instead. How would I go
> about generating a custom image with the dependencies preinstalled,
> and caching that based on the packages it contains?
>
> *(after design proposal)*
>
> Yes, please revert. Let's just go with the Forgejo container registry.
> For the cache key, let's do strict plus year with week number to
> handle the security update concern.

## Symptom

Run #32 of `u/albert/4/firewall` failed Build, Test, and E2E with
`Killed` during `apt-get install` — never reaching cargo. Run #31 had
the same problem on Clippy and Rustfmt.

## Diagnosis

Each of the five heavy jobs (Build, Test, E2E, Clippy, Rustfmt) starts
its own `ubuntu:25.10` container and runs `apt-get install` for the
full build dep set in parallel. After `9bc470f` added clang +
llvm-20-dev + libpolly-20-dev for `bpf-linker`, that set unpacks to
~1 GB per container. Five concurrent unpackings exhaust runner host
memory; the OOM-killer takes down whichever apt processes happen to be
mid-flight. The dice rolled differently between runs, so #31 lost
Clippy/Rustfmt while #32 lost Build/Test/E2E.

The PR's actual code is clean — `cargo build`, `cargo test
--all-targets`, `cargo test -p shepherd-e2e -- --include-ignored`,
`cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, and
`shellcheck` all pass on the branch locally.

## Approach considered first (and reverted)

Initial fix in commit `30fcd71` (later reverted) chained Build → Test
→ E2E and Build → Clippy via `needs:`, plus stripped the build-deps
install from Rustfmt (`cargo fmt --check` doesn't compile). That
reduced max concurrent heavy installs from 5 to 2, which would
probably keep CI green, but accepted a 2× wall-time hit and didn't
address the underlying redundancy: every job repeating the same
multi-minute apt + rustup + `cargo install bpf-linker` setup from
scratch.

## What landed

Pre-bake a CI image with everything already installed and host it in
Forgejo's built-in container registry, so heavy jobs skip setup
entirely.

### `.ci/Dockerfile`

`FROM ubuntu:25.10`, then runs `./scripts/shepherd deps install
build/run/test` so the image stays lockstep with what a real Ubuntu
host would install. `install_rust` uses `--profile minimal`, so an
extra `rustup component add clippy rustfmt` line follows. `PATH`
exports `/root/.cargo/bin` so subsequent CI steps don't need to
source `~/.cargo/env`.

### `.github/workflows/ci.yml`

New `image` job runs first:

  1. `actions/checkout@v4`
  2. Compute tag = `<isoyear>w<isoweek>-<sha256-12>` over the strict
     input set: `.ci/Dockerfile`, `scripts/deps/{build,run,test}.pkgs`,
     `scripts/lib/deps.sh`. The week prefix forces a weekly rebuild
     so distro security updates roll in even when no input file
     changed.
  3. `docker login git.armeafamily.com` with `secrets.GITHUB_TOKEN`
     (Forgejo provisions this with `permissions: packages: write`).
  4. `docker manifest inspect "$ref"` — if the tag exists, exit. Else
     `docker build && docker push`.
  5. Output: full image ref (`git.armeafamily.com/albert/shepherd-launcher-ci:<tag>`).

All Rust jobs (`build`, `test`, `e2e`, `lint`, `fmt`) declare
`needs: image` and `container.image: ${{ needs.image.outputs.ref }}`
with `credentials:` for the registry pull. They drop every
"Install git", "Install build dependencies", "Add Rust to PATH",
"Add clippy/rustfmt component" step — the image already has all of
that. They keep the `actions/cache@v4` step but narrow it to
`~/.cargo/registry`, `~/.cargo/git`, and `target/` (the bits that
vary per branch). `~/.cargo/bin` and `~/.rustup/toolchains` are now
inside the image, so the cache no longer has to schlep them around.

`shellcheck` stays on plain `ubuntu:25.10` — its install is one
package (~1 MB), nowhere near OOM territory.

## Cost

  - First run (cache miss): image job adds ~5–8 min for image build,
    dominated by `cargo install bpf-linker`. Heavy jobs that follow
    skip ~3 min of apt setup each.
  - Subsequent runs (cache hit): image job pulls a manifest and
    exits in seconds. Heavy jobs skip apt entirely; their wall time
    is dominated by `cargo` work.
  - Weekly rollover: one rebuild per week, baseline cost.

## Why the heavy jobs were OOMing in the first place

The runner's `config.yml` has
`container.options: "--cpus=2 --memory=2g"`. Every job container is
hard-capped at 2 GB. Once 9bc470f added clang + llvm-20-dev +
libpolly-20-dev, the apt-get unpacking step exceeded that cap and
the kernel SIGKILLed apt mid-flight — that's the "Killed" we saw in
runs #31 and #32. The image-cache approach sidesteps this entirely:
the apt unpacking now happens inside `docker build`, whose child
containers don't inherit the `--memory=2g` cap from the job that
spawned the build.

(Heavy jobs that *consume* the prebaked image are still capped at
2 GB. Cargo compilation can spike toward that ceiling, so this may
need revisiting if rust-side OOMs reappear — either lift the cap
in the runner config or pin `cargo build --jobs 1` for the heaviest
crates.)

## Talking to docker from a job

The `image` job needs to run `docker build`/`docker push` from inside
its job container. Three iterations got that working:

  - **Run #33** failed with `docker: command not found` — act-runner's
    default job container (`node:20-bookworm`) has git but no docker
    CLI. Fix: `apt-get install -y docker.io` as the first step.
  - **Run #34** failed at the new `docker info` check — the CLI
    couldn't reach a daemon. First guess was that act-runner just
    needed `DOCKER_HOST=tcp://docker:2375` set (the same value the
    runner config has), but the hostname `docker` doesn't resolve
    from inside a job container.
  - **Run #35** failed for the same reason as #34, just with the
    explicit DOCKER_HOST. The user's setup is docker-compose with
    two services, `runner` and `docker:dind`, on a shared compose
    network. The runner reaches dind via the compose-network DNS
    name `docker`. But job containers are spawned by dind itself
    and live on *dind's* bridge network, where `docker` doesn't
    resolve. The dind daemon is, however, reachable at the bridge
    gateway IP — which is the dind container itself, listening on
    `0.0.0.0:2375` (TLS off via `DOCKER_TLS_CERTDIR=""`).

    Fix: parse `/proc/net/route` to read the default gateway IP at
    job startup and set `DOCKER_HOST=tcp://<gateway>:2375`. This
    works without depending on iproute2 in the base image.

Switching the runner to `container.docker_host: -` (host-socket
mount) was considered and rejected — it would erode isolation for
*other* repos served by the same runner that today get a fresh
ephemeral DinD per workflow. This repo's existing workflows don't
run docker from inside a job, so they're unaffected by the
`docker_host` value either way.

## Other manual setup

  - **`secrets.GITHUB_TOKEN` needs package write scope.** Forgejo
    grants this when the workflow declares `permissions: packages:
    write` (per Gitea convention). If the runner's token policy
    disagrees, the user may need a personal access token kept in
    `secrets.CI_REGISTRY_TOKEN` instead.
  - **First push creates the package.** Forgejo may default it to
    private; visibility can be flipped on the package settings page
    if it's convenient to allow unauthenticated pulls (e.g. for
    forks). Heavy jobs already pass `credentials:` so private works.

## Open questions / follow-ups

  - Mid-week security update with no input change: bump the
    Dockerfile (a comment is enough) to force a fresh hash, or wait
    a week.
