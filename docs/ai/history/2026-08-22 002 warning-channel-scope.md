# A warning channel for the management UIs — scope (issue #143)

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/143>
> Follow-up to #127, which added three admin-facing warnings and made the gap
> obvious. Related: #136/#141 (orphaned windows), the closest precedent.

## Prompt

> posted as #143, scope out the implementation (will land via new branch, but
> I'm planning on merging the `media` kind first)

Branched from `feat/media-kind` rather than `main` for that reason: three of the
seed conditions below are the media warnings from #127, and the daemon-side
hook they attach to (`MediaPrefetcher::from_policy`) only exists on that branch.
Rebases onto `main` once #127 merges.

## Issue text

> **A warning channel for the management UIs**
>
> shepherdd notices a lot of things an administrator would want to act on, and
> tells nobody. They go to the log, which is where an admin will never look.
> [...] What this wants is a structured channel: conditions *raised* and
> *cleared* rather than logged once, each with a severity, the entry or
> subsystem it concerns, and a suggested fix — surfaced in both management UIs.

## What exists today

**Nothing structured.** Every one of the ~60 candidate sites is
`tracing::warn!`/`error!` into the subscriber installed at
`crates/shepherdd/src/main.rs:1225`, and nothing else. All three plausible
existing channels were checked and none fits:

- `HealthStatus` (`crates/shepherd-api/src/types.rs:744`) is five coarse
  booleans — `live`, `ready`, `policy_loaded`, `host_adapter_ok`, `store_ok` —
  with no reason string. It answers "is the daemon up", not "what is wrong".
- `EventPayload` (`crates/shepherd-api/src/events.rs:34`) has a `WarningIssued`
  variant, but it is the *time-limit* warning for the child's HUD.
- `AuditEventType` (`crates/shepherd-store/src/audit.rs:12`) covers
  session/policy/client lifecycle plus `ActivityEscaped`. It is also
  append-only, which is the wrong shape (see below).

**The name is taken twice.** `WarningSeverity` / `WarningThreshold`
(`types.rs:582`, `:591`) and `AuditEventType::WarningIssued` (`audit.rs:32`)
both mean "tell the child their session is nearly over". Reusing the word for an
administrator-facing condition would collide in exactly the code that has to
handle both.

**De-duplication is ad-hoc and inconsistent.** Four mechanisms already exist for
"don't say this every time":

| site | mechanism |
| --- | --- |
| `crates/shepherdd/src/input_devices.rs:142` | `self.warned_unavailable` bool |
| `crates/shepherd-ble/src/server.rs:1078` | `pin.warned.swap(true, …)`, then drops to `debug!` |
| `crates/shepherd-host-linux/src/adapter.rs:860` | `attempts % 10 == 1` throttle |
| `crates/shepherd-host-linux/src/adapter.rs:665` | a `reported` HashSet keyed by pid |

…while the per-launch firewall and browser warnings
(`adapter.rs:1388`, `:1424`, `:1498`, `:1503`) repeat unbounded. A raise/clear
model subsumes all five behaviours, which is the strongest argument for doing
this properly rather than adding a sixth.

**Startup-computed conditions are never recomputed and never retracted.** The
missing-`yt-dlp` check runs inside `MediaPrefetcher::from_policy`
(`crates/shepherdd/src/media.rs:353`, constructed at `main.rs:177`) and the
firewall probe inside `process::init` (`crates/shepherd-host-linux/src/process.rs:277`,
called from `adapter.rs:323`); neither is re-run by the reload path
(`main.rs:~745`). So an admin who adds a YouTube entry gets no warning until
they restart the daemon, and one who then installs `yt-dlp` keeps the warning
forever. **Any design that does not fix this makes the feature actively
misleading**, which is why re-evaluation is treated as core below rather than as
a refinement.

## The precedent to follow

#136/#141 had the identical shape: `report_unowned_windows` logged a warning and
stopped there, and neither UI could tell a child's game from a surface no
session owned. The fix was **a wire field, not a UI trick** — `WindowInfo`
gained a `WindowOwner`, `LinuxHost::list_windows` attributed the list before it
left the host, and both UIs led with what nothing was supervising
(`shepherd-webui/src/pages/WindowsPage.tsx`,
`companion-android/.../ui/windows/WindowsScreen.kt`).

This should follow the same route, and the transport work is genuinely cheap
because of machinery that already exists:

- `#[shepherd_management_macros::management_rpc]` on the `ManagementService`
  trait (`crates/shepherd-management/src/service.rs:40`) expands to
  `dispatch_json`, so one new trait method is reachable from BLE and from the
  web UI's JSON-RPC client without hand-written match arms.
- Adding a type to `WireTypes` (`crates/shepherd-wire-codegen/src/wire_schema.rs:26`)
  regenerates the Kotlin and TS mirrors; `tests/rpc_codegen_drift.rs` fails CI
  until the regenerated `WireTypes.generated.kt` and
  `rpc-methods.generated.ts` are checked in.
- The web UI already consumes SSE at `GET /api/v1/events`
  (`shepherd-webui/src/api/client.ts:225`, `hooks/useEvents.ts`), so a delta
  event needs no new transport.

## Proposed design

### Name

`Diagnostic` — `Diagnostic`, `DiagnosticSeverity`, `DiagnosticCode`. It is
unambiguous against both existing "warning" meanings and reads correctly for a
condition that is *currently true*. The UIs are free to render it as "Needs
attention"; the wire spelling does not have to be the parent-facing word.

`Notice` is the main alternative and is friendlier, but it undersells a
firewall that isn't enforcing. Worth a decision before any code lands, because
it is all over the wire schema afterwards.

### Shape

```rust
pub struct Diagnostic {
    /// Stable identity. `(code, subject)` is the dedup key: raising the same
    /// pair twice updates in place rather than accumulating.
    pub code: DiagnosticCode,
    pub subject: DiagnosticSubject,
    pub severity: DiagnosticSeverity,
    /// One line, already written — most of these exist verbatim in today's
    /// log messages.
    pub message: String,
    /// What to do about it. `shepherd-admin media-deps install`, "add
    /// shepherdd's user to the `input` group", etc.
    pub remedy: Option<String>,
    pub since: DateTime<Local>,
}

pub enum DiagnosticSubject {
    Service,
    Entry(EntryId),
}

pub enum DiagnosticSeverity {
    /// A configured protection is not in effect. The config claims something
    /// the device is not doing.
    Critical,
    /// A feature is unavailable or degraded.
    Warning,
    /// Worth knowing, nothing is broken.
    Info,
}
```

`code` is an enum, not a string, so the UIs can special-case presentation and
the drift test covers the variant set. `message`/`remedy` travel as text because
the daemon already writes them and localisation is not a concern here.

### State, not events

The registry is the source of truth for *what is currently wrong*:

```rust
impl Diagnostics {
    fn raise(&self, d: Diagnostic);                       // idempotent on (code, subject)
    fn clear(&self, code: DiagnosticCode, subject: &DiagnosticSubject);
    fn current(&self) -> Vec<Diagnostic>;                 // sorted: severity, then subject
}
```

Delivered two ways, mirroring how everything else already reaches clients:

- **In `ServiceStateSnapshot`** (`types.rs:685`) as `diagnostics: Vec<Diagnostic>`,
  behind `#[serde(default)]` like `entries` and `internet_status` before it, so
  every client has the current set on subscribe with no extra round trip.
- **As `EventPayload::DiagnosticsChanged`** carrying the new full set, for
  deltas. Full set rather than a raise/clear delta: the list is small, and a
  client that missed an event would otherwise need reconciliation logic.

A new `list_diagnostics()` trait method is *probably* unnecessary given the
snapshot, but costs almost nothing via `dispatch_json` and gives the web UI a
plain refetch path alongside `listWindows`. Decide when wiring the UI.

**Size.** BLE frames cap at `MAX_FRAME_BYTES = 16 KiB`
(`crates/shepherd-ble/src/protocol.rs:45`), and `ServiceStateSnapshot` already
carries every `EntryView`. The set must therefore be bounded — cap the list
(32 is far above any real device) and carry a `truncated: bool`. An unbounded
diagnostics list on the snapshot is the one change here that could break the
companion transport.

### Re-evaluation is the core of it, not a refinement

Conditions split cleanly in two, and the split should be in the type system:

**Probed** — a pure function of the environment that can be re-run: is `yt-dlp`
on PATH, is free disk above the floor, is the firewall helper installed, does
`/dev/input` have anything readable, is there a sound backend. These register a
probe, and the registry re-runs all probes on:

- daemon start,
- **config reload** (`main.rs:~745`) — the case that is broken today,
- the existing hourly tick that the media prefetch sweep already uses,
- and, for disk, after each prefetch sweep.

A probe that returns clean clears its diagnostic. This is what makes the panel
trustworthy: everything in it was true within the last hour, and fixing
something makes it disappear without a restart.

**Observed** — raised at the moment something happens and not re-derivable:
"this entry launched without its configured firewall". These are raised at the
site (`adapter.rs:1424` and friends), and cleared when the same entry next
launches successfully or on config reload. Note that today these fire *per
launch*, so the registry immediately fixes their unbounded repetition.

### Seeding: which of the ~60

Deliberately not all of them. A large share of what's in the log is transient
per-operation failure that should stay there. The line worth drawing is **"a
condition an administrator can fix"**, which yields a first set of nine:

| code | site | kind |
| --- | --- | --- |
| `FirewallUnenforceable` | `process.rs:277` | probed |
| `FirewallNotApplied` (per entry) | `adapter.rs:1424`, `:1498`, `:1503` | observed |
| `BrowserPolicyIgnored` (per entry) | `adapter.rs:1388` | observed |
| `YtDlpMissing` | `media.rs:353` | probed |
| `MediaCacheDiskLow` | `media.rs:251` | probed |
| `MediaLibraryUnreadable` (per entry) | `media.rs:290` | observed |
| `NoSoundBackend` | `main.rs:142` | probed |
| `InputDevicesUnavailable` | `input_devices.rs:142` | probed |
| `BlePairingAgentUnavailable` | `server.rs:1344` | observed |

Every one already has its message and most have their remedy text written; the
work is moving them, not inventing them. `BrightnessCtlMissing`
(`brightness.rs:126`), `ConfigWatchUnavailable` (`main.rs:550`) and
`BleBearerPinUnavailable` (`server.rs:1078`) are the obvious next tier.

### Presentation

Severity drives it:

- **Critical** — the config claims a protection the device is not providing.
  Deserves to be unmissable: a persistent banner in both UIs.
- **Warning / Info** — a list, plus a badge on the affected entry for
  `Subject::Entry` ones.

Per-entry diagnostics render on the entry itself, next to the availability
reasons already there, by joining on entry id client-side. Putting them *on*
`EntryView` was considered and rejected: it duplicates them into the snapshot
twice and makes the bounded-size problem worse.

## Phases

1. **The registry and the wire.** `Diagnostic` types in `shepherd-api`, the
   registry in `shepherdd`, snapshot field, `DiagnosticsChanged` event, the
   `WireTypes` entry plus regenerated Kotlin/TS. Nothing raises anything yet.
2. **Probes and the re-evaluation triggers**, seeded with the five probed
   conditions. This is where the reload gap is closed.
3. **The observed four**, which also deletes their ad-hoc dedup state.
4. **Web UI** — a panel modelled on `WindowsPage.tsx`, plus the entry badge.
5. **Companion** — the same, modelled on `WindowsScreen.kt`.

Phases 1–3 are useful on their own: `shepherd-admin`-style inspection over the
existing RPC beats journald even before either UI lands.

## Open questions

- **`Diagnostic` vs `Notice`** (above). Needs deciding first; it is pervasive.
- **Does the companion push?** "Critical deserves to interrupt" assumes a
  notification path exists on the phone. If it doesn't, Critical degrades to a
  banner and the interrupt question moves to its own issue.
- **Does anything go in the audit log too?** A protection silently not applied
  seems worth an audit row, distinct from the current-state registry. Probably
  yes for Critical, but it is a second write path and can wait.
- **Overlap with `shepherd-admin config validate`.** It already catches a class
  of these statically. Worth deciding whether the two share a vocabulary, or
  whether validate stays purely about the file and diagnostics purely about the
  running system.
- **Do diagnostics survive a restart?** Probed ones re-derive themselves, so
  no persistence is needed. Observed ones vanish on restart, which is arguably
  correct (nothing has launched yet) but means the panel is briefly emptier
  than the truth.

## Testing

The registry is pure and easily unit-tested: idempotent raise, clear-by-key,
severity ordering, the size cap and its `truncated` flag. Probe re-evaluation
wants a fake clock and a fake environment so "install yt-dlp, reload, warning
clears" is a test rather than a manual check — the bug this whole issue exists
to prevent.

The drift test (`crates/shepherd-wire-codegen/tests/rpc_codegen_drift.rs`)
covers the wire mirrors for free once the type is listed in `WireTypes`.

End-to-end, the headless dev session is the right harness, and #127's experience
argues for using it early rather than at the end: the prefetch sweep bug there —
every library swept several times a second — was invisible to every unit test
and obvious within seconds of a real event bus. A diagnostics registry fed by
config reloads and an hourly tick has exactly the same failure mode available
to it.
