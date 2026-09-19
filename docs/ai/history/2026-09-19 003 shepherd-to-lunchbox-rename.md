# Renaming Shepherd to Lunchbox

> Status: **done**, 2026-09-19. The tree builds, lints and tests clean, both
> Android apps build, and a headless session boots the launcher under
> `com.lunchbox-os.launcher`. Two follow-ups are listed at the end — both are
> infrastructure that has to exist in the world, not code.

## Prompt

> I'm continuing the migration from git.armeafamily.com/albert/shepherd-launcher
> to github.com/aarmea/lunchbox. Now let's do the actual rename. Go one
> component/crate at a time, handling package names, directory names,
> filenames, names mentioned in docs, etc. ensuring everything continues to
> build and run. I bought lunchbox-os.com, so use com.lunchbox-os.* in place of
> com.armeafamily.shepherd.* where relevant.

Follow-ups during the work:

> (you may need to clear the caches in ~/shepherd-launcher/target so you have
> enough space...)

> as you're verifying, note for once you get to it: you have two Android phones
> and a Bluetooth dongle

> oh use lunchbox-os.com for user-facing URLs that aren't for the source code

## What was there

8,647 occurrences of "shepherd" across 627 files, 565 of which had it in the
path: 33 workspace crates, the daemon, the web UI, two Android apps, the
script suite, the packaging tree and the docs.

## Four decisions taken before touching anything

**1. The Android package could not be what was asked for.** Android
application IDs and Kotlin package names require every segment to match
`[a-zA-Z][a-zA-Z0-9_]*`; AGP rejects a hyphen and Kotlin cannot parse one as an
identifier. `com.lunchbox-os.companion` would not build. The hyphen is fine
everywhere else it is used here — GTK application IDs and polkit action IDs
both already carried one (`org.shepherd.pairing-display`,
`org.shepherd.firewall.apply-process`) — so only Android needed an answer.
Chosen: `com.lunchbox_os.{companion,media}` for the Android apps,
`com.lunchbox-os.*` everywhere else.

**2. On-disk names are a clean break, with a migration.** Nothing in the new
tree reads an old path. `scripts/lib/migrate.sh` moves an existing device over
instead (see below).

**3. `docs/ai/history` keeps the old names.** 2,852 of the occurrences were in
155 dated notes recording work done under the old name. Rewriting them would
misrepresent what was run at the time, so they were left exactly as written and
`docs/ai/history/README.md` now carries a translation table.

**4. "Shepherd" the device-noun becomes `Device`, not `Lunchbox`.**

## The correction that mattered

The plan put `WindowOwner::Shepherd` in the device-noun bucket, which was
wrong, and reading the enum before renaming it is what caught it:

```rust
pub enum WindowOwner {
    /// Shepherd's own furniture: the launcher, the HUD, the pairing UI, ...
    Shepherd,
```

It means *the product's own windows*, not a paired device. Renaming it to
`Device` would have made it read as "this window belongs to a device" — the
opposite of what the host uses it for. It became `WindowOwner::Lunchbox`.

The device-noun rename did apply to the companion app's types, which really do
name a device the phone talks to: `ShepherdRecord` → `DeviceRecord`,
`ShepherdConnection` → `DeviceConnection`, `ShepherdViewModel` →
`DeviceViewModel`, `ShepherdRepository`, `ShepherdScanner` likewise.

`WindowOwner` is `#[serde(rename_all = "snake_case")]`, so the variant rename
also changed the wire spelling from `"shepherd"` to `"lunchbox"`. Both clients
hard-code those strings, and the codebase already knew it:

```rust
/// The wire spellings the web UI and the companion app switch on. Both
/// clients hard-code these strings, and a silent rename here would
/// downgrade every orphan to an ordinary row rather than failing.
```

That test failed exactly as designed partway through the sweep. Rust, the
companion and the web UI were changed together and the codegen mirrors
regenerated with `cargo run -p lunchbox-wire-codegen --bin rpc-codegen`.

## How the sweep was done

Crate by crate in dependency order, leaves first, `cargo check --workspace
--all-targets` after each group, `git mv` for directories so history follows.

The substitution helper used a negative lookahead — `\Qold\E(?![A-Za-z0-9_-])`
— because `shepherd-media` is a prefix of `shepherd-media-core` and a plain
`sed` would have renamed half a crate name. Order within a family then does not
matter.

Two bugs in that helper are worth recording, because both failed *quietly*:

* **Slashes.** The first version interpolated the search string into `s/.../.../`,
  so the first URL argument (`https://git.armeafamily.com/...`) terminated the
  substitution early. Perl failed to compile, `2>/dev/null` swallowed it, and
  the script cheerfully reported "rewrote ... in 0 files". Nothing was
  corrupted — a Perl compile error happens before `-i` opens anything — but
  three URL rewrites silently did nothing. Fixed by passing the strings through
  the environment and using `s{}{}`.
* **Symlinks.** `perl -i` replaces a symlink with a regular file. `CLAUDE.md`
  was the repo's only symlink (`-> AGENTS.md`) and was silently converted to a
  copy. Restored with the original blob hash; `git ls-tree` confirms mode
  120000 again. Any future tree-wide rewrite here must skip symlinks.

## The migration

`scripts/lib/migrate.sh`, called from `install_all` and restated in POSIX sh in
the `.deb`'s postinst. It moves the custodian tree (`/var/lib/shepherdd` →
`/var/lib/lunchboxd`, `shepherdd.db` → `lunchboxd.db`), the per-user XDG
directories and the sway config; removes superseded polkit/udev/systemd/
bluetooth files; and renames the `shepherd-firewall` group and `shepherd-state`
user *in place* with `groupmod -n`/`usermod -l`, which keeps the gid/uid and so
preserves every membership and every file they own.

Three properties, and the ordering is the subtle one:

* **Idempotent**, and a no-op on a device that was never a shepherd.
* **Never clobbers.** A legacy path moves only onto a path that does not exist.
  If both exist the new one wins and the legacy copy is left on disk, because
  only the operator can know which is real.
* **Runs before the install steps, not after.** `install_state` would otherwise
  create an empty `/var/lib/lunchboxd` first, and the migration would then
  refuse to move onto it — stranding the device's real state and bringing it up
  as if it were new. The postinst has the same hazard with `groupadd`: creating
  `lunchbox-firewall` before the rename leaves both groups present and the
  memberships on the old one.

The apt source list and Chrome policy are *reported*, not removed: the operator
wrote those by hand following `docs/INSTALL.md`, so they are not the
installer's to delete.

## Verification

| | |
|---|---|
| `cargo check --workspace --all-targets` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace --all-targets` | 1488 passed, 0 failed |
| `shellcheck` (as CI runs it) | clean |
| `lunchbox-webui` — `tsc --noEmit`, `vitest` | clean, 213 passed |
| config-wasm | builds, emits `lunchbox_config` |
| `config.example.toml` | validates |
| companion app | `assembleDebug` + unit tests pass as `com.lunchbox_os.companion` |
| media app | builds, installs, runs on device (below) |
| headless session | launcher up and focused as `com.lunchbox-os.launcher` |
| IPC | `dev-runtime/lunchbox.sock` answers a `launch` RPC |
| web UI | serves, `<title>Lunchbox</title>` |
| `.deb` | builds; every shipped path renamed; postinst is valid POSIX sh |
| BLE pairing | first pairing + reconnect on real hardware (below) |

### Pairing on real hardware

Done with the Pixel 10a and the Realtek dongle, per the `companion-pairing`
skill, because the app-ID change forces a re-pair and the regenerated Kotlin
wire types had never been run against the daemon.

Both bonds were cleared first — the host's with `bluetoothctl remove`, the
phone's through Settings — so this was a genuine first pairing rather than a
reconnect over a surviving bond:

* Numeric Comparison matched (phone `168563` == device `168563`).
* Bond came up `LE:Y`, `EncryptionStatus{keySize=16, algorithm=2}`, listed on
  the phone as **`lunchbox`** (the renamed advertised default).
* `claim` RPC landed: *First admin claimed the device … device=Pixel 10a*, and
  `admin.toml` was written.
* Reconnect after `am force-stop`: RPCs resumed (`service_state`,
  `list_groups`, `get_volume`, …) and the bounded drain fired —
  `BLE outbox backlog drained bytes=9770 reads=21`.

That reconnect is the real test of the regenerated mirrors: every one of those
RPCs decodes Kotlin types rendered from the Rust definitions, so a wire
mismatch would have failed there rather than in a unit test.

`WindowOwner`'s new spelling agrees on all three sides: Rust `"lunchbox"`,
Kotlin `LUNCHBOX("lunchbox")`, and the regenerated TS mirror.

### The media app

Its one rename-specific hazard is the JNI chain, because the loaded library is
named after the crate: `lunchbox-media-android` builds
`liblunchbox_media_android.so`, and `AndroidManifest.xml`'s
`android.app.lib_name` has to say the same thing. A mismatch is invisible at
build time and an `UnsatisfiedLinkError` at launch, so it was checked on the
device rather than by reading:

```
nativeloader: Load .../lib/arm64/liblunchbox_media_android.so ... ok
lunchbox_media_androi..: registered the JavaVM with FFmpeg; MediaCodec decoding is available
```

The activity resolves as `com.lunchbox_os.media/.LunchboxMediaActivity`, the
process stays up, and the UI renders titled `lunchbox-media`. There are no
Android-side unit tests here (the source set is `main` only) — the logic is in
Rust, and `cargo test` covers the six media crates: 342 tests, 0 failures.

The desktop `lunchbox-media` binary ships in the `.deb` and answers `--help`.

### The app-ID rename is not an upgrade

Android keys an install by application ID, so `com.lunchbox_os.companion`
installs **alongside** `com.armeafamily.shepherd.companion` rather than over
it — confirmed on both bench phones, which now list both. The consequences are
the user-visible half of this rename:

* The new app starts with no data: no admin records, **no claim tokens**. Those
  are not recoverable from the old app, so every companion user re-pairs.
* The OS bond is per-device, not per-app, so the old bond survives and is
  attributed to the old package. A device whose host-side bond is then cleared
  leaves the phone holding a key the device does not — the asymmetric lockout
  the skill warns about. Clearing *both* sides is the reliable order.
* The old app stays installed and still holds its bond, and a peripheral stops
  advertising while any peer is connected — so the old app has to be
  force-stopped or uninstalled before the new one can see the device at all.

Worth saying plainly in the release notes; it is not something the installer
migration can fix.

`cargo test` needed `libmpv-dev` installed for amd64 — the box had only the
arm64 cross variant, so linking failed with `unable to find library -lmpv`.
Pre-existing, unrelated to the rename, and invisible to `cargo check`, which
does not link.

## Left deliberately

* **`shepherd-launcher-patch@albertarmea.com`** — the maintainer patch address
  in `README.md` and `scripts/lib/package.sh`. A live mailbox; renaming it in
  the tree would not create the new one.
* **`git.armeafamily.com` in `.github/workflows/release.yml.disabled`** — those
  comments say the publishing infrastructure still lives on the Forgejo
  instance, which is *true*, and is why that workflow is disabled.

## Follow-ups

1. **`apt.lunchbox-os.com` and `lunchbox-os.com/fdroid/repo` do not exist.**
   `docs/INSTALL.md` now tells people to install from them, per "use
   lunchbox-os.com for user-facing URLs". Standing them up (or changing the
   docs) is a prerequisite for the next release; a note to that effect is in
   `release.yml.disabled` next to the existing "decide where releases live".
   The F-Droid `fingerprint` in `INSTALL.md` stays valid across the move — it
   pins the repository signing key, not the host.
2. **The `.deb` is still `lunchbox-launcher`**, a faithful 1:1 rename of
   `shepherd-launcher`. Now that the repo is just `lunchbox`, `-launcher` may
   be redundant; renaming the package again is a separate decision with its own
   apt-upgrade consequences, so it was left alone.
