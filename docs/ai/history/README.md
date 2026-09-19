# About these notes

Design notes and agent prompts, one file per piece of work, named
`YYYY-MM-DD NNN topic.md` for the day it was written.

They are a **record of what was done at the time**, not living documentation.
A note is not updated when the code it describes changes; it is left as
written, and a later note supersedes it. For how the system works now, read
the crate `README.md`s, `CONTRIBUTING.md` and `docs/`.

## The names changed on 2026-09-19

The project was called **Shepherd** until 2026-09-19, when it was renamed to
**Lunchbox** (`docs/ai/history/2026-09-19 003 shepherd-to-lunchbox-rename.md`).
Every note dated before then uses the old names throughout, and was
deliberately left that way — rewriting them would misrepresent what was
actually run and read at the time.

So when reading an older note, translate as you go:

| Note says | Now |
|---|---|
| Shepherd | Lunchbox |
| `shepherd-*` crates | `lunchbox-*` |
| `shepherdd` | `lunchboxd` |
| `shepherd-launcher` (binary), `shepherd-admin` | `lunchbox-launcher`, `lunchbox-admin` |
| `./scripts/shepherd` | `./scripts/lunchbox` |
| `shepherd-webui` | `lunchbox-webui` |
| `SHEPHERD_*` env vars | `LUNCHBOX_*` |
| `org.shepherd.*` app IDs | `com.lunchbox-os.*` |
| `com.armeafamily.shepherd.{companion,media}` | `com.lunchbox_os.{companion,media}` |
| `/etc/shepherd`, `~/.config/shepherd` | `/etc/lunchbox`, `~/.config/lunchbox` |
| `/var/lib/shepherdd`, `shepherdd.db` | `/var/lib/lunchboxd`, `lunchboxd.db` |
| `git.armeafamily.com/albert/shepherd-launcher` | `github.com/aarmea/lunchbox` |

Two things in those notes are *not* simply renamed, because they did not
survive as the same idea:

* `ShepherdRecord` / `ShepherdConnection` / `ShepherdViewModel` and friends in
  the companion app became `DeviceRecord` / `DeviceConnection` /
  `DeviceViewModel`. They name a paired device, not the product, so they took
  the product's name out rather than swapping it.
* `WindowOwner::Shepherd` became `WindowOwner::Lunchbox`, and its wire spelling
  changed from `"shepherd"` to `"lunchbox"`. A note describing that JSON is
  describing a payload no current build emits.
