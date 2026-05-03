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
