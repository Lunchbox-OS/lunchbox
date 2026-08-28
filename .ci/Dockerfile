# CI image for shepherd-launcher.
#
# Pre-installs everything `./scripts/shepherd deps install dev` would on a
# real Ubuntu 26.04 host: apt build/run/test deps, rustup with stable
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

# 26.04, matching the platform shepherd-launcher is developed and tested on
# (docs/INSTALL.md) and the floor `check_ubuntu_version` recommends. CI had sat
# a release behind on 25.10, which is how it ended up running an npm below the
# `engines` floor shepherd-webui declares, and a Node wrangler refuses to start
# on. 26.04 ships nodejs 22.
FROM ubuntu:26.04

ENV DEBIAN_FRONTEND=noninteractive

# Base tools the workflow itself needs (git for checkout, curl for the
# rustup installer in scripts/lib/deps.sh, nodejs for the embedded web
# UI's npm build and for wrangler in release.yml's config-editor job).
RUN apt-get update && \
    apt-get install -y git curl nodejs && \
    rm -rf /var/lib/apt/lists/*

# Install apt build/run/test deps + rustup (stable) + nightly+rust-src
# + bpf-linker via the project's own deps script, so the image stays in
# lockstep with what `./scripts/shepherd deps install` does on a host.
#
# `scripts/shepherd` reads the canonical repo-root VERSION file at startup
# (for any subcommand), resolving it relative to the script location as
# `scripts/../VERSION` — i.e. /opt/shepherd-deps/VERSION here. Copy it in so
# the deps subcommand doesn't die with "Version file not found". Its value is
# irrelevant to what gets installed; only its presence matters.
COPY VERSION /opt/shepherd-deps/VERSION
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

# systemd + polkit + dbus + sudo are needed by the firewall E2E job
# (firewall_real.rs), which boots the container with `--entrypoint
# /sbin/init` so it can drive activity scopes through a real system
# systemd manager and pkexec the privileged helper. Other jobs use
# this image with the default entrypoint and never touch any of this.
RUN apt-get update && \
    apt-get install -y \
        systemd \
        systemd-sysv \
        dbus \
        polkitd \
        pkexec \
        sudo \
    && rm -rf /var/lib/apt/lists/*

# Mask units that are useless or fail noisily inside a container so
# `systemctl is-system-running` reaches `running` (not perpetually
# `starting`) once dbus/polkit are up.
RUN systemctl mask \
        systemd-udevd.service \
        systemd-udev-trigger.service \
        systemd-modules-load.service \
        systemd-resolved.service \
        systemd-networkd.service \
        NetworkManager.service \
        ModemManager.service \
        getty@.service \
        serial-getty@.service \
        systemd-firstboot.service \
        systemd-machine-id-commit.service \
    || true

# Make rustup-installed tools (cargo, rustc, bpf-linker, clippy-driver,
# rustfmt) available without sourcing ~/.cargo/env in every CI step.
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /
