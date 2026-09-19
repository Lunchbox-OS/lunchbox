# Lunchbox Scripts System

This directory contains the unified script system for Lunchbox.

## Quick Reference

```sh
# Main entry point
./lunchbox --help

# Dependencies
./lunchbox deps print build|run|dev
./lunchbox deps install build|run|dev

# Building
./lunchbox build [--release]

# Cross-compiling for another architecture (see "Cross-compiling" below)
./lunchbox deps install cross --arch arm64
./lunchbox build --arch arm64
./lunchbox package deb --arch arm64

# Configuration
./lunchbox config validate [path]

# Development
./lunchbox dev run                  # nested sway (needs a graphical login session)

# Headless, agent/SSH-drivable session (no login session, no GPU)
./lunchbox deps install agent       # grim + wtype + jq
./lunchbox dev headless [--config PATH] [--user NAME] [--time "..."] [--size WxH]
./lunchbox dev shot [out.png]       # screenshot the virtual output
./lunchbox dev tree                 # window-tree summary (app_id/focus)
./lunchbox dev key Down / type "x" / click X Y   # inject input
./lunchbox dev stop

# Installation
./lunchbox install all --user USER [--prefix PREFIX]
./lunchbox install bins [--prefix PREFIX]
./lunchbox install config --user USER

# Hardening
./lunchbox harden apply --user USER
./lunchbox harden revert --user USER

# Packaging
./lunchbox package deb

# Post-install admin tasks (shared with the .deb, where they run as the
# installed `lunchbox-admin` CLI without a source tree)
./lunchbox setup-user USER          # deploy config + add group memberships
./lunchbox apps install steam|chrome|retroarch
./lunchbox deps install run         # (includes yt-dlp)  ==  lunchbox-admin yt-dlp install
```

These admin tasks live in `lib/admin.sh` and are exposed by **both**
`./scripts/lunchbox` (from source) and `scripts/lunchbox-admin` (a slim
entrypoint the `.deb` installs as `/usr/bin/lunchbox-admin`). See
[docs/INSTALL.md](../docs/INSTALL.md).

## Structure

```
scripts/
├── lunchbox           # Main CLI dispatcher (build/dev/install/package + admin)
├── lunchbox-admin     # Slim admin CLI shipped in the .deb (no source tree)
├── dev                # Wrapper → lunchbox dev run
├── admin              # Wrapper → lunchbox install/harden
├── lib/               # Shared libraries
│   ├── common.sh      # Logging, error handling, sudo, get_data_dir
│   ├── deps.sh        # Dependency management
│   ├── admin.sh       # Shared post-install admin tasks (yt-dlp, apps, setup-user)
│   ├── build.sh       # Cargo build logic
│   ├── config.sh      # Configuration validation
│   ├── sway.sh        # Nested sway execution
│   ├── install.sh     # Installation logic
│   ├── harden.sh      # User hardening/unhardening
│   ├── bluetooth.sh   # BLE admin (clear/unpair)
│   ├── version.sh     # Canonical VERSION sync
│   └── package.sh     # .deb packaging (stages install.sh + lunchbox-admin)
└── deps/              # Package lists
    ├── build.pkgs     # Build-time dependencies
    ├── run.pkgs       # Runtime dependencies
    └── dev.pkgs       # Development extras
```

## Design Principles

1. **Single source of truth**: All dependency lists are defined once in `deps/*.pkgs`
2. **Composable**: Each command can be called independently
3. **Reversible**: All destructive actions (hardening, installation) can be undone
4. **Shared logic**: Business logic lives in libraries, not duplicated across scripts
5. **Clear separation**: Build-only, runtime-only, and development dependencies are separate

## Usage Examples

### For Developers

```sh
# First time setup (installs system packages + Rust via rustup)
./lunchbox deps install dev
./lunchbox dev run

# Or use the convenience wrapper
./run-dev
```

### For CI

```sh
# Install only build dependencies (includes Rust via rustup)
./lunchbox deps install build

# Build release binaries
./lunchbox build --release
```

### For Production Deployment

```sh
# On a runtime-only system
sudo ./lunchbox deps install run
./lunchbox build --release
sudo ./lunchbox install all --user kiosk --prefix /usr

# Optional: lock down the kiosk user
sudo ./lunchbox harden apply --user kiosk
```

### For Package Maintainers

```sh
# Print package lists for your distro
./lunchbox deps print build > build-deps.txt
./lunchbox deps print run > runtime-deps.txt

# Install with custom prefix and DESTDIR
make -j$(nproc)  # or equivalent
sudo DESTDIR=/tmp/staging ./lunchbox install bins --prefix /usr
```

## Dependency Sets

- **build**: Packages needed to compile the Rust code (GTK, Wayland dev libs, etc.) + Rust toolchain via rustup
- **run**: Packages needed to run the compiled binaries (Sway, GTK runtime libs)
- **cross**: Cross toolchain + the target architecture's half of the build set. Needs `--arch`; see below.
- **dev**: Union of build + run + dev-specific tools (git, gdb, strace) + Rust toolchain

The dev set is computed as the union of all three package lists, automatically deduplicated.

## Cross-compiling

`--arch` takes a Debian architecture name and applies to both building and
packaging:

```sh
./lunchbox deps install cross --arch arm64   # one-time, ~1.5-2.5 GB
./lunchbox build --arch arm64                # -> target/aarch64-unknown-linux-gnu/debug
./lunchbox package deb --arch arm64          # -> dist/pkg/..._arm64.deb
```

Notes:

- An `--arch` naming the **host's own** architecture is a no-op: it builds
  natively, into the usual `target/{debug,release}`. That is deliberate, so a CI
  matrix can pass `--arch` on every leg without giving the native one a second
  target directory. Use `build --target <triple>` to force the triple path.
  Because every CI job passes `--arch`, the no-`--arch` and host-`--arch` paths
  never run there; `ci/check-default-arch.sh` covers them by faking the host's
  `dpkg --print-architecture`. Run it after touching either.
- `deps install cross` tells dpkg about the architecture and, only if the
  configured mirror does not serve it, adds an entry for Ubuntu's ports mirror.
  Many mirrors carry every architecture in one tree, so this is decided by
  probing rather than assumed.
- It **refuses** if installing the target's `-dev` set would uninstall the
  host's: those chains are not always co-installable, and apt would remove the
  native half silently. Cross-compile in a container instead, or pass
  `--allow-remove` and restore with `deps install build`.
- Cross-compiling buys no test coverage: nothing in `cargo test`, the e2e suite
  or the firewall BPF suites runs on the target architecture. Those need a
  native host.
- Do **not** cross-compile by exporting `CARGO_BUILD_TARGET`. It would override
  `crates/lunchbox-firewall-bpf`'s own target and try to build the eBPF program
  for the host triple. `lunchbox build` passes `--target` on the command line
  instead, and `lunchbox-firewall-helper`'s build script strips the variable.

## Hardening

The hardening system makes reversible changes to restrict a user to kiosk mode:

```sh
# Apply hardening
sudo ./lunchbox harden apply --user kiosk

# Check status
sudo ./lunchbox harden status --user kiosk

# Revert all changes
sudo ./lunchbox harden revert --user kiosk
```

All changes are tracked in `/var/lib/lunchboxd/hardening/<user>/` for rollback.

Applied restrictions:
- SSH access denied
- Console (TTY) login restricted
- Sudo access denied
- Shell restricted to Sway sessions only
- Home directory permissions secured

## Adding New Dependencies

Edit the appropriate package list in `deps/`:

- `deps/build.pkgs` - Build-time dependencies
- `deps/run.pkgs` - Runtime dependencies
- `deps/dev.pkgs` - Developer tools
- `deps/cross.pkgs` - Cross toolchain + the target architecture's `-dev` halves

Format: One package per line, `#` for comments. In `cross.pkgs` only, `@ARCH@`
is replaced with the architecture passed to `--arch`.

Adding a `-dev` package to `build.pkgs` means adding its `:@ARCH@` counterpart
to `cross.pkgs`; `ci/check-cross-pkgs.sh` fails the build if you forget, in
either direction. If the package is genuinely host-only, record it with its
reason in that script's `host_only` map instead.

The CI workflow will automatically use these lists.
