# CI image for shepherd-launcher.
#
# Pre-installs everything `./scripts/shepherd deps install dev` would on a
# real Ubuntu 25.10 host: apt build/run/test deps, rustup with stable
# (clippy + rustfmt) and nightly+rust-src, and bpf-linker. Subsequent CI
# jobs use this image so they can skip the multi-minute apt + rustup +
# `cargo install bpf-linker` setup that previously OOM-killed the runner
# when several jobs tried to do it in parallel.
#
# Tag scheme: `<isoyear>w<isoweek>-<sha256-12>` over .ci/Dockerfile,
# scripts/deps/{build,run,test}.pkgs, and scripts/lib/deps.sh. The week
# stamp forces a weekly rebuild so distro security updates roll in even
# when no input file changed; the hash forces a rebuild whenever the
# inputs do change.

FROM ubuntu:25.10

ENV DEBIAN_FRONTEND=noninteractive

# Base tools the workflow itself needs (git for checkout, curl for the
# rustup installer in scripts/lib/deps.sh, nodejs for the embedded web
# UI's npm build).
RUN apt-get update && \
    apt-get install -y git curl nodejs && \
    rm -rf /var/lib/apt/lists/*

# Install apt build/run/test deps + rustup (stable) + nightly+rust-src
# + bpf-linker via the project's own deps script, so the image stays in
# lockstep with what `./scripts/shepherd deps install` does on a host.
COPY scripts /opt/shepherd-deps/scripts
WORKDIR /opt/shepherd-deps
RUN ./scripts/shepherd deps install build && \
    ./scripts/shepherd deps install run && \
    ./scripts/shepherd deps install test && \
    rm -rf /var/lib/apt/lists/*

# install_rust uses --profile minimal, so neither clippy nor rustfmt is
# in the toolchain yet. Add them now so the lint and fmt jobs don't
# need to.
RUN /root/.cargo/bin/rustup component add clippy rustfmt

# Make rustup-installed tools (cargo, rustc, bpf-linker, clippy-driver,
# rustfmt) available without sourcing ~/.cargo/env in every CI step.
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /
