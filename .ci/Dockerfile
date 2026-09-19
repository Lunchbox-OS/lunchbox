# CI image for Lunchbox.
#
# Pre-installs everything `./scripts/lunchbox deps install dev` would on a
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

# 26.04, matching the platform Lunchbox is developed and tested on
# (docs/INSTALL.md) and the floor `check_ubuntu_version` recommends. CI had sat
# a release behind on 25.10, which is how it ended up running an npm below the
# `engines` floor lunchbox-webui declares, and a Node wrangler refuses to start
# on. 26.04 ships nodejs 22.
FROM ubuntu:26.04

ENV DEBIAN_FRONTEND=noninteractive

# Base tools the workflow itself needs (git for checkout, curl for the
# rustup installer in scripts/lib/deps.sh, nodejs for the embedded web
# UI's npm build and for wrangler in release.yml's config-editor job).
RUN apt-get update && \
    apt-get install -y git curl nodejs && \
    rm -rf /var/lib/apt/lists/*

# Rust lives in /usr/local rather than /root/.cargo so a non-root job can use
# it. Tests run as an unprivileged user (see the `ci` user below), and a
# toolchain under root's 0700 home would be unreachable to them; rustup honours
# these two variables, so relocating is a matter of setting them before the
# installer runs.
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

# Install apt build/run/test deps + rustup (stable) + nightly+rust-src
# + bpf-linker via the project's own deps script, so the image stays in
# lockstep with what `./scripts/lunchbox deps install` does on a host.
#
# `scripts/lunchbox` reads the canonical repo-root VERSION file at startup
# (for any subcommand), resolving it relative to the script location as
# `scripts/../VERSION` — i.e. /opt/lunchbox-deps/VERSION here. Copy it in so
# the deps subcommand doesn't die with "Version file not found". Its value is
# irrelevant to what gets installed; only its presence matters.
COPY VERSION /opt/lunchbox-deps/VERSION
COPY scripts /opt/lunchbox-deps/scripts
WORKDIR /opt/lunchbox-deps
RUN ./scripts/lunchbox deps install build && \
    ./scripts/lunchbox deps install run && \
    ./scripts/lunchbox deps install test && \
    rm -rf /var/lib/apt/lists/*

# install_rust uses --profile minimal, so neither clippy nor rustfmt is
# in the toolchain yet. Add them now so the lint and fmt jobs don't
# need to.
RUN rustup component add clippy rustfmt

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

# An unprivileged user for the jobs that should not run as root.
#
# CI ran everything as root, and that hid a real bug: `classify` accepts root
# from any cgroup by design, so three of the peer tests never reached the cgroup
# comparison they exist to make — one failed outright once the kernel floor was
# met, and two had been passing without testing anything (issue #144). A device
# never runs an activity as root, so neither should the suite that stands in for
# one.
#
# The toolchain and the shared dep registry have to be writable by whoever runs
# cargo, and `CARGO_HOME` is one path shared by both users here, so open it up
# rather than duplicate the cache per uid.
RUN useradd --create-home --shell /bin/bash --uid 1001 ci && \
    chmod -R a+rwX "$CARGO_HOME" "$RUSTUP_HOME"

# Left as root deliberately: `actions/checkout` writes into a workspace the
# runner owns, the firewall job boots this image on `/sbin/init`, and two jobs
# apt-install at runtime. Jobs that want to be unprivileged step down to `ci`
# themselves, which keeps that choice visible in the workflow instead of
# implicit in the image.

WORKDIR /
