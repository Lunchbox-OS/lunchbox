# Icon Autodetection (Issue #14)

**Issue:** https://git.armeafamily.com/albert/shepherd-launcher/issues/14

Most entry types required an explicit `icon` field in the config, but desktop environments
retrieve icons automatically via `.desktop` files and icon themes. This task implements
the same autodetection.

## What was done

Added `crates/shepherd-config/src/icon.rs` with `autodetect_icon(kind: &EntryKind) -> Option<String>`.
Called from `Entry::from_raw` in `policy.rs` as a fallback when `raw.icon` is `None`.

### Detection strategy per kind

| Kind    | Strategy |
|---------|----------|
| Flatpak | Use `app_id` directly — Flatpak exports icons under the app_id as the theme name |
| Snap    | Search `/var/lib/snapd/desktop/applications/<snap_name>_*.desktop` for `Icon=`; fall back to snap name |
| Steam   | Read `~/.local/share/applications/steam_<app_id>.desktop` for `Icon=`; fall back to `steam_icon_<app_id>` |
| Process | Search XDG app dirs (`$XDG_DATA_HOME/applications`, `$XDG_DATA_DIRS/applications`) for a `.desktop` file whose `Exec=` basename matches the command basename |
| Vm / Media / Custom | No autodetection |

### config.example.toml changes

- Removed explicit `icon` from Flatpak entries (`prism-launcher`, `krita`) — autodetected reliably
- Removed explicit `icon` from Snap entry (`gcompris`) — autodetected from snapd desktop files
- Commented out `icon` for Steam entries — autodetected, but kept as optional override example

### No new dependencies

Used `std::env::var_os("HOME")` instead of the `dirs` crate to avoid adding a dependency
to `shepherd-config`.
