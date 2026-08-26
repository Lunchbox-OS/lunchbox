# shepherd-firewall-helper

A small privileged helper that lets shepherdd apply per-activity firewall
rules without itself running as root or holding `CAP_NET_ADMIN`.

## Why this exists

systemd backs `IPAddressDeny=`/`IPAddressAllow=` with cgroup BPF
(`cgroup_skb`). Attaching such programs requires `CAP_NET_ADMIN`, and per
`man systemd.resource-control`, IP address filters are not supported by
per-user instances of the service manager. shepherdd runs as the kiosk user
(no caps), so its previous approach of `systemd-run --user --scope
--property=IPAddressDeny=...` was a silent no-op -- the property was
accepted but no BPF program was attached.

This helper closes that gap. It is invoked by shepherdd via `pkexec`, runs
as root, drops privileges to the requesting user via `systemd-run`'s
`--uid=`/`--gid=` flags, and creates a *system*-level transient scope that
the system manager can attach BPF programs to.

## Trust boundary

* The helper is invoked via pkexec under a polkit rule that grants the
  caller's user the action `org.shepherd.firewall.apply-process` without a
  password prompt (see [`dist/polkit/`](../../dist/polkit/)).
* The helper validates every argument with a strict allowlist: numeric uid /
  gid, IP address tokens (`any`, `localhost`, `link-local`, `multicast`),
  literal IPv4/IPv6 addresses, and CIDRs. Anything else is rejected.
* The helper enforces `--uid` matches `$PKEXEC_UID` so a user in the granted
  group cannot launch processes as a different user.
* `--uid 0` is rejected.
* The helper has no dependencies beyond `std`, keeping the audit surface
  small.

## CLI

```
shepherd-firewall-helper apply-process \
    --uid <U> --gid <G> \
    --scope-name <transient-scope-name> \
    --default deny|allow \
    [--allow <CIDR-or-token>]... \
    [--deny <CIDR-or-token>]... \
    [--env KEY=VALUE]... \
    [--cwd <ABS-PATH>] \
    -- <command> [args...]

shepherd-firewall-helper stop-scope --scope-name <name>
```

`apply-process` `exec()`s into `systemd-run --scope --uid=U --gid=G ...
--property=IPAddress*=... -- command`, so the helper's pid becomes
`systemd-run`'s pid; shepherdd's `Command::spawn().wait()` returns when
the activity exits and the transient scope is collected.

## Install

The helper is installed by `scripts/integration-tests/setup-firewall-dev.sh`
(dev) or by `shepherd install` (production, follow-up). It must live at
`/usr/libexec/shepherd-firewall-helper` -- that path is hardcoded into the
polkit policy file.

## Testing

`apply-cgroup` is the path with the sharpest failure mode: it loads the
embedded BPF object, and if that load fails the caller gets no filter. Issue
#151 is what that looks like in practice -- a misaligned object made every
attach fail, and firewalled flatpaks ran unfiltered for months.

- `cargo test -p shepherd-firewall-helper` checks the embedded object is
  aligned and parses. Unprivileged, runs everywhere.
- `scripts/integration-tests/test-firewall-cgroup.sh` (root, no polkit or
  flatpak needed) attaches the program to a purpose-made cgroup and proves
  packets are actually filtered. This one runs in CI.
- `test-firewall.sh` / `test-firewall-flatpak.sh` / `test-firewall-snap.sh`
  are the full-stack tests, driven through shepherdd on a configured host.
