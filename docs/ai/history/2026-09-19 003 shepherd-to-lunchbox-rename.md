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
| media app | `assembleDebug` passes, packages `liblunchbox_media_android.so` |
| headless session | launcher up and focused as `com.lunchbox-os.launcher` |
| IPC | `dev-runtime/lunchbox.sock` answers a `launch` RPC |
| web UI | serves, `<title>Lunchbox</title>` |

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
