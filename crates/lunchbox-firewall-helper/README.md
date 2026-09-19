# lunchbox-firewall-helper

A small privileged helper that lets lunchboxd apply per-activity firewall
rules without itself running as root or holding `CAP_NET_ADMIN`.

## Why this exists

systemd backs `IPAddressDeny=`/`IPAddressAllow=` with cgroup BPF
(`cgroup_skb`). Attaching such programs requires `CAP_NET_ADMIN`, and per
`man systemd.resource-control`, IP address filters are not supported by
per-user instances of the service manager. lunchboxd runs as the kiosk user
(no caps), so its previous approach of `systemd-run --user --scope
--property=IPAddressDeny=...` was a silent no-op -- the property was
accepted but no BPF program was attached.

This helper closes that gap. It is invoked by lunchboxd via `pkexec`, runs
as root, drops privileges to the requesting user via `systemd-run`'s
`--uid=`/`--gid=` flags, and creates a *system*-level transient scope that
the system manager can attach BPF programs to.

## Trust boundary

* The helper is invoked via pkexec under a polkit rule that grants the
  caller's user the action `com.lunchboxos.firewall.apply-process` without a
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
lunchbox-firewall-helper apply-process \
    --uid <U> --gid <G> \
    --scope-name <transient-scope-name> \
    --default deny|allow \
    [--allow <CIDR-or-token>]... \
    [--deny <CIDR-or-token>]... \
    [--env KEY=VALUE]... \
    [--cwd <ABS-PATH>] \
    -- <command> [args...]

lunchbox-firewall-helper stop-scope --scope-name <name>
```

`apply-process` `exec()`s into `systemd-run --scope --uid=U --gid=G ...
--property=IPAddress*=... -- command`, so the helper's pid becomes
`systemd-run`'s pid; lunchboxd's `Command::spawn().wait()` returns when
the activity exits and the transient scope is collected.

## The scope dies with the session (issue #172)

The scope this creates is a **system** manager unit — it has to be, because the
`cgroup_skb` programs behind `IPAddressDeny=` need `CAP_NET_ADMIN` and a
per-user manager cannot attach them. That put it outside `user-<uid>.slice`,
where nothing that ends a kiosk session reached it: not logind's
`TerminateSession`, not its teardown of `user@<uid>.service`, not `KillUser`.
The only thing that ever stopped one was lunchboxd calling back through
`stop-scope` — and issue #172 is about lunchboxd being killed, which is exactly
when nothing calls anything.

So `apply-process` adds two properties (`lifetime_args`):

| | |
| --- | --- |
| `--slice=user-<uid>.slice` | puts the scope in the kiosk user's slice. A unit implicitly `Requires=` its slice, so logind stopping that slice at the user's last logout stops this too — and it is the slice `KillUser` kills, so the session watchdog's escalation reaches it |
| `--property=BindsTo=`/`After=` the caller's session scope | ends it when *that session* ends, which is the sharper statement: a device with two kiosk users has one slice each but a session per login, and an activity belongs to a session |

**The session is derived, never passed.** The polkit rule admits the
`lunchbox-firewall` group, which is the kiosk user, so an argument would be
attacker-chosen. It is read from this process's own cgroup — the caller's,
inherited through `pkexec`, before `systemd-run` moves anything — and validated
to logind's shape (`session-`, alphanumeric, `.scope`) because it ends up in an
argv. A caller not in a session scope (a development stack, a hand-run helper)
gets the slice and no binding, rather than a guess: naming a unit that does not
exist would fail the scope's start, and that is an activity that will not launch
for the sake of defence in depth.

Declaring the lifetime here is what keeps the authority where it belongs.
The alternative was to let the state custodian stop these units when it ends a
session, which would have meant `org.freedesktop.systemd1.manage-units` — the
right to stop *any* unit on the machine — for a daemon whose whole discipline is
custody rather than judgment.

## Install

The helper is installed by `scripts/integration-tests/setup-firewall-dev.sh`
(dev) or by `lunchbox install` (production, follow-up). It must live at
`/usr/libexec/lunchbox-firewall-helper` -- that path is hardcoded into the
polkit policy file.

## Testing

`apply-cgroup` is the path with the sharpest failure mode: it loads the
embedded BPF object, and if that load fails the caller gets no filter. Issue
#151 is what that looks like in practice -- a misaligned object made every
attach fail, and firewalled flatpaks ran unfiltered for months.

- `cargo test -p lunchbox-firewall-helper` checks the embedded object is
  aligned and parses. Unprivileged, runs everywhere.
- `scripts/integration-tests/test-firewall-cgroup.sh` (root, no polkit or
  flatpak needed) attaches the program to a purpose-made cgroup and proves
  packets are actually filtered. This one runs in CI.
- `test-firewall.sh` / `test-firewall-flatpak.sh` / `test-firewall-snap.sh`
  are the full-stack tests, driven through lunchboxd on a configured host.
