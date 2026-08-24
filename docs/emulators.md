# Emulated games

`shepherdd` runs emulated games through [RetroArch][retroarch], the libretro
frontend, with `type = "retroarch"` entries. Compared with launching RetroArch
as a plain `type = "process"` activity, the dedicated kind exists to make an
emulator behave like every other supervised activity:

- **Closing the activity saves; opening it restores.** The child resumes
  exactly where they stopped — mid-battle, mid-cutscene — rather than at the
  title screen.
- **The in-game save survives**, including a crash or a forced kill.
- **The child stays in the game**: RetroArch's own menu, file browser, and
  settings are locked.
- **Each activity keeps its own saves**, under a directory shepherd owns.

**No games are included, and none can be.** Supply your own content, and only
content you have the right to. This document uses a freely licensed homebrew
test ROM for its examples.

[retroarch]: https://www.retroarch.com/

## Installing

```sh
sudo shepherd-admin apps install retroarch            # RetroArch + the mgba core
sudo shepherd-admin apps install retroarch mgba nestopia snes9x
shepherd-admin apps install retroarch help            # list the available cores
```

Cores are named the way an entry's `core =` field names them (`mgba`, not the
`libretro-mgba` package), so there is one spelling to learn — and the naming is
the same whichever source they come from, so an entry never has to care.

The name you install is the name you configure, even where the shared object on
disk disagrees. It often does: `libretro-genesisplusgx` ships
`genesis_plus_gx_libretro.so`, `libretro-mupen64plus-next` ships
`mupen64plus_next_libretro.so`, and the Beetle cores are libretro's forks of
Mednafen and keep that name (`libretro-beetle-psx` →
`mednafen_psx_hw_libretro.so`). shepherd matches against the files actually
present, ignoring `-` versus `_`, and knows the Beetle aliases — so
`core = "beetle-psx"` and `core = "mednafen_psx_hw"` both work.

By default everything comes from the **Ubuntu archive** (`universe`), which
packages RetroArch itself and 14 cores:

| System | `core =` |
| --- | --- |
| Game Boy Advance, Game Boy / Color | `mgba` |
| Game Boy / Color | `gambatte`, `sameboy` |
| NES | `nestopia` |
| SNES | `snes9x`, `bsnes-mercury-accuracy`, `bsnes-mercury-balanced`, `bsnes-mercury-performance` |
| Mega Drive / Genesis / Master System | `genesisplusgx` |
| Nintendo DS | `desmume` |
| PlayStation | `beetle-psx` |
| PC Engine / TurboGrafx-16 | `beetle-pce-fast` |
| Virtual Boy | `beetle-vb` |
| WonderSwan | `beetle-wswan` |

[The full catalog](#full-core-catalog) below lists these plus everything the
PPA adds.

### Cores the archive doesn't package

That list stops well short of what libretro supports — no N64, GameCube/Wii,
Saturn, or arcade. Those are packaged only by the libretro team's PPA, which
`--ppa` opts into:

```sh
sudo shepherd-admin apps install retroarch --ppa mupen64plus-next dolphin
```

It is opt-in because a PPA is a third-party apt source for the **whole
system**, not just for RetroArch: once added it can install and upgrade
packages from then on. Remove it with
`sudo add-apt-repository --remove ppa:libretro/testing`.

Two channels exist and the names are misleading:

| | Cores | Frontend |
| --- | --- | --- |
| `--ppa` / `--ppa=testing` | ~98, including everything the archive has | yes |
| `--ppa=stable` | **none** | yes |

`ppa:libretro/stable` publishes the RetroArch frontend only — no cores at all —
and on recent Ubuntu releases at the same upstream version the archive ships
(1.22.2 on 26.04). It is there for tracking upstream frontend builds between
Ubuntu releases; it will not give you a single extra system. So a bare `--ppa`
selects `testing`, the one that changes what you can install.

With `--ppa`, core names are checked against apt after the repository is added
rather than against the built-in list, since the whole point is that the
catalog is now larger. `apt-cache search --names-only '^libretro-'` lists what
is actually available.

### What is never used

RetroArch's built-in "core downloader" fetches unsigned binaries at runtime,
which is not something a supervised kiosk should be doing behind the operator's
back — so cores always come from apt, from one of the sources above. Ubuntu's
build hides the downloader by default (`menu_show_core_updater = "false"`), and
`kiosk = true` locks the menu it lives in regardless.

Some systems additionally need a BIOS image that cannot be distributed (the
PlayStation core is the common case). Those go in
`~/.config/retroarch/system/`; the core's own documentation names the files it
expects.

## Configuring an activity

```toml
[[entries]]
id = "pokemon-firered"
label = "Pokemon FireRed"

[entries.kind]
type = "retroarch"
core = "mgba"
content = "~/Games/retroarch/pokemon-firered.gba"

[entries.limits]
max_run_seconds = 2700       # long enough to reach a save point
daily_quota_seconds = 5400
cooldown_seconds = 1800
```

Everything else an entry supports — availability windows, groups, token gates,
volume and brightness limits, `requires_input` — works the same as for any
other kind.

### Fields

| Field | Default | Meaning |
| --- | --- | --- |
| `core` | — | Core short name; resolved to `<core>_libretro.so`. Exactly one of `core` / `core_path` is required. |
| `core_path` | — | Absolute path to a `*_libretro.so`, bypassing name resolution. |
| `content` | — | The ROM or disc image. **Must be absolute or start with `~/`.** |
| `save_state` | `"auto"` | `"auto"` saves state on close and restores on open; `"off"` boots the content fresh every time. |
| `reset` | `true` | Show the HUD's reset button. |
| `kiosk` | `true` | Lock RetroArch's own menu. |
| `command` | `retroarch` | The RetroArch binary. |
| `args` | `[]` | Extra arguments, appended after the ones shepherd derives, so they win. |
| `env` | `{}` | Extra environment variables. |

`content` is strict about relative paths on purpose: a bare `Games/game.gba`
would be resolved against the *daemon's* working directory, not the operator's,
and would fail at launch with a confusing error rather than at config
validation. `shepherd config validate` rejects it up front.

Compressed content works — RetroArch reads `.zip` and `.7z` directly — but the
uncompressed file is easier to reason about when checking what a save belongs
to.

## How saving works

Two different things are called "saving", and they are not interchangeable.

**The in-game save** (SRAM / battery save, `.srm`) is the one the game itself
writes — what a child means by "my save". RetroArch flushes it when content
unloads, and shepherd additionally sets `autosave_interval = 10`, so it is
written every ten seconds of play. A crash, a power cut, or a forced kill costs
seconds, not an afternoon.

**The save state** (`.state.auto`) is a snapshot of the whole emulator. With
`save_state = "auto"` (the default), closing the activity writes one and
opening restores it. This is what makes an emulator behave like the rest of the
kiosk: a session that ends at a time limit picks up exactly where it stopped.

Both depend on RetroArch exiting cleanly, so shepherd gives these sessions a
15-second graceful-stop window instead of the usual 5 — long enough to unload
the core and write both kinds of save on slow storage.

### The reset button

Save-state resume has a consequence: if every launch restores where the child
left off, a game's own title screen becomes unreachable. That is what the HUD's
reset button (the circular arrow, left of the "X") is for. It:

1. stops the activity cleanly, so the in-game save is flushed;
2. deletes the save state that would otherwise resume;
3. relaunches the game **under the same session** — the same time limit, the
   same clock, no new cooldown.

The child's actual saved game is untouched: resetting a console returns it to
its title screen, it does not wipe the cartridge. The prompt says so.

Set `reset = false` to hide the button. With `save_state = "off"` it is much
less useful — every launch is already a fresh boot — but it still works as a
"this game is stuck, restart it" escape.

## Where files live

The two kinds of save live in two different places, on purpose.

**The in-game save stays where RetroArch puts it** — normally
`~/.config/retroarch/saves/<Core>/<content>.srm`. Shepherd does not relocate
it, so one game has one save whether it was launched from here or from a
desktop session, and a save made before the entry existed is found without any
migration. Back it up by copying `~/.config/retroarch/saves/`, the same
directory you would back up for RetroArch on its own.

**The resume state is shepherd's**, since nothing outside a supervised session
produces one:

```
~/.local/share/shepherdd/retroarch/pokemon-firered/
├── append.cfg              # generated on every launch; see below
└── states/mGBA/…state.auto # the resume state (+ .png thumbnail)
```

Keyed by entry id, so two entries pointing at the same ROM resume
independently even though they share the underlying save. The `mGBA/` level is
RetroArch's own doing — it files states under the core that wrote them, which
is what you want, since a state written by one core cannot be loaded by
another.

To reset an activity from the admin side rather than the HUD, delete its
`states/` tree while the activity is not running.

> **Coming from a `type = "process"` entry?** Nothing to do. That entry used
> RetroArch's default save location, and so does this one, so the child's
> existing save carries over untouched.

## What shepherd generates, and what it leaves alone

Before each launch shepherd writes `append.cfg` and passes it to RetroArch with
`--appendconfig`. **Your `~/.config/retroarch/retroarch.cfg` is never edited**
— cores, controller bindings, shaders and everything else you set up in
RetroArch stay yours, and shepherd's settings apply only to activities it
launches.

That last guarantee takes an explicit setting to hold: RetroArch's
`config_save_on_exit` defaults to *true*, so a clean exit would otherwise write
its entire live settings block — including everything appended — back into your
config, making shepherd's per-activity choices permanent and global. The
generated fragment turns it off for the run.

The fragment sets, and only sets:

| Setting | Why |
| --- | --- |
| `config_save_on_exit = false` | The above. |
| `savestate_directory` | The per-entry resume-state directory. The in-game save is deliberately *not* redirected. |
| `savestate_auto_save`, `savestate_auto_load` | Save on close, restore on open. |
| `autosave_interval = 10` | Flush the in-game save while playing. |
| `pause_nonactive = false` | The HUD takes keyboard focus for its prompts; left at RetroArch's default the game would pause whenever one opened. |
| `video_fullscreen = true` | One activity, no window furniture. |
| `video_context_driver = "wayland"` | Take the native Wayland path rather than whatever auto-detection lands on, so the picture is the panel's own pixel grid instead of an upscaled XWayland one. A preference, not a demand: RetroArch falls back to its usual search if Wayland will not initialize, and the Vulkan path keeps its own ordering (its drivers are named `vk_wayland`). |
| `kiosk_mode_enable` | Lock the menu (from `kiosk`). |

Shepherd does *not* isolate RetroArch's playlists, history, or runtime logs —
those still live under `~/.config/retroarch/`. Nothing there affects a
supervised session; it is worth knowing if you expected the activity to leave
no trace at all.

### Settings you make outside shepherd carry in

Configure RetroArch however you like from a normal desktop session — bind your
controllers, pick a video driver, set per-core options — and shepherd picks it
all up. Every launch loads your `~/.config/retroarch/retroarch.cfg` first and
appends its fragment on top:

```
[INFO] [Config] Loading config: "~/.config/retroarch/retroarch.cfg".
[INFO] [Config] Appending config: "…/append.cfg".
```

Only the settings in the table above are shepherd's; everything else is yours.
Controller autoconfig profiles (`autoconfig/`), input remaps (`remaps/`) and
per-core options (`retroarch-core-options.cfg`, the `.opt` files) are separate
files shepherd never touches. Traffic is one-way — `config_save_on_exit =
"false"` means a supervised session cannot write back into your config, so
shepherd's per-activity choices never become your global ones.

### …and per-core overrides beat shepherd

One sharp edge. RetroArch applies **overrides** —
`~/.config/retroarch/config/<Core>/<Core>.cfg`, and the per-content-directory
and per-game files beside it — *after* `--appendconfig`, so an override that
names one of shepherd's settings wins.

Mostly that is what you want: overrides are how per-core video and input tuning
carries into a session. But for the nine settings shepherd relies on it is a
footgun, and two of them fail quietly:

- `kiosk_mode_enable = "false"` unlocks RetroArch's menu inside a supervised
  session — the child can reach the file browser again.
- `savestate_auto_save` / `savestate_auto_load` break resume with no error at
  all. The activity runs fine; the child just loses their place.

The rest are `config_save_on_exit`, `savestate_directory`,
`autosave_interval`, `pause_nonactive`, `video_fullscreen` and
`video_context_driver`.

shepherd checks for this at every launch and warns, naming the file and the
keys:

```
WARN shepherd_host_linux::retroarch: RetroArch override sets settings shepherd
relies on; RetroArch applies overrides after --appendconfig, so these win …
override_file=~/.config/retroarch/config/mGBA/mGBA.cfg
settings=savestate_auto_save, kiosk_mode_enable
```

It is a warning, not an error: your overrides are yours, and shepherd will not
silently discard them. Remove those keys from the override file to hand the
settings back.

### The network command interface is not enabled

RetroArch can expose a UDP control port (`network_cmd_enable`), and shepherd
deliberately does not use it. It binds to all interfaces, cannot be restricted
to localhost, and has no authentication — anyone on the network could quit a
child's game or load different content into it. Everything shepherd needs
(including the reset button) is done without it.

The "cannot be restricted" half is upstream's to fix, and it has been asked:
[libretro/RetroArch#19459][ra-19459] requests a configurable bind address, and a
collaborator has said it looks like a reasonable addition. If it lands, this
section is worth revisiting — a loopback-only socket would still be
unauthenticated to anything running as the same user, but it would take the rest
of the network out of the picture.

[ra-19459]: https://github.com/libretro/RetroArch/issues/19459

## Full core catalog

Every core installable through `shepherd-admin apps install retroarch`, with
the value to put in `core =`. "In Ubuntu? yes" means no `--ppa` needed.

Built from the packages themselves — `dpkg -c` over every `libretro-*` in both
sources, joined with the `libretro-core-info` database — rather than from the
package descriptions, because the two disagree often enough to matter.

| System | `core =` | apt package | In Ubuntu? |
| --- | --- | --- | --- |
| 2048 Game Clone | `2048` | `libretro-2048` | PPA only |
| 3D Engine | `3dengine` | `libretro-3dengine` | PPA only |
| Acorn — BBC Micro | `b2` | `libretro-b2` | PPA only |
| Amstrad — CPC | `crocods` | `libretro-crocods` | PPA only |
| Anarch | `anarch` | `libretro-anarch` | PPA only |
| Arcade (various) | `fbalpha2012` | `libretro-fbalpha2012` | PPA only |
| Arcade (various) | `fbneo` | `libretro-fbneo` | PPA only |
| Arcade (various) | `mame2003` | `libretro-mame2003` | PPA only |
| Arcade (various) | `mame2010` | `libretro-mame2010` | PPA only |
| Arduboy | `ardens` | `libretro-ardens` | PPA only |
| Arduboy | `arduous` | `libretro-arduous` | PPA only |
| Atari 2600 | `stella2014` | `libretro-stella2014` | PPA only |
| Atari 5200 | `a5200` | `libretro-a5200` | PPA only |
| Atari 7800 | `prosystem` | `libretro-prosystem` | PPA only |
| Atari 8-bit Family | `atari800` | `libretro-atari800` | PPA only |
| Atari ST/STE/TT/Falcon | `hatari` | `libretro-hatari` | PPA only |
| Atari — Lynx | `beetle-lynx` | `libretro-beetle-lynx` | PPA only |
| Atari — Lynx | `handy` | `libretro-handy` | PPA only |
| Bandai — WonderSwan/Color | `beetle-wswan` | `libretro-beetle-wswan` | yes |
| BK-0010/BK-0011(M) | `bk` | `libretro-bk` | PPA only |
| Capcom — CP System I | `fbalpha2012-cps1` | `libretro-fbalpha2012-cps1` | PPA only |
| Capcom — CP System II | `fbalpha2012-cps2` | `libretro-fbalpha2012-cps2` | PPA only |
| Capcom — CP System III | `fbalpha2012-cps3` | `libretro-fbalpha2012-cps3` | PPA only |
| Cave Story Game Engine | `nxengine` | `libretro-nxengine` | PPA only |
| Commodore — C128 | `vice_x128` | `libretro-vice` | PPA only |
| Commodore — C64 | `vice_x64` | `libretro-vice` | PPA only |
| Commodore — C64 | `vice_x64sc` | `libretro-vice` | PPA only |
| Commodore — C64 SuperCPU | `vice_xscpu64` | `libretro-vice` | PPA only |
| Commodore — C64DTV | `vice_x64dtv` | `libretro-vice` | PPA only |
| Commodore — CBM-5x0 | `vice_xcbm5x0` | `libretro-vice` | PPA only |
| Commodore — CBM-II | `vice_xcbm2` | `libretro-vice` | PPA only |
| Commodore — PET | `vice_xpet` | `libretro-vice` | PPA only |
| Commodore — PLUS/4 | `vice_xplus4` | `libretro-vice` | PPA only |
| Commodore — VIC-20 | `vice_xvic` | `libretro-vice` | PPA only |
| Dinothawr Game Engine | `dinothawr` | `libretro-dinothawr` | PPA only |
| Id Software — DOOM Game Engine | `prboom` | `libretro-prboom` | PPA only |
| Id Software — Quake Game Engine | `tyrquake` | `libretro-tyrquake` | PPA only |
| Java — J2ME | `freej2me` | `libretro-freej2me` | PPA only |
| Lutro | `lutro` | `libretro-lutro` | PPA only |
| Magnavox/Philips — Magnavox Odyssey2 / Philips Videopac+ | `o2em` | `libretro-o2em` | PPA only |
| Microsoft — DOS | `dosbox` | `libretro-dosbox` | PPA only |
| Microsoft — Minecraft Game Clone | `craft` | `libretro-craft` | PPA only |
| Mr.Boom | `mrboom` | `libretro-mrboom` | PPA only |
| Music | `pocketcdg` | `libretro-pocketcdg` | PPA only |
| NEC — PC Engine SuperGrafx | `beetle-supergrafx` | `libretro-beetle-supergrafx` | PPA only |
| NEC — PC Engine/PCE-CD | `beetle-pce-fast` | `libretro-beetle-pce-fast` | yes |
| NEC — PC-98 | `np2` | `libretro-np2` | PPA only |
| NEC — PC-FX | `beetle-pcfx` | `libretro-beetle-pcfx` | PPA only |
| Nintendo 64 | `mupen64plus-next` | `libretro-mupen64plus-next` | PPA only |
| Nintendo DS | `desmume` | `libretro-desmume` | yes |
| Nintendo DS | `melonds` | `libretro-melonds` | PPA only |
| Nintendo Entertainment System | `fceumm` | `libretro-fceumm` | PPA only |
| Nintendo Entertainment System | `nestopia` | `libretro-nestopia` | yes |
| Nintendo Entertainment System | `quicknes` | `libretro-quicknes` | PPA only |
| Nintendo — Game Boy Advance | `beetle-gba` | `libretro-beetle-gba` | PPA only |
| Nintendo — Game Boy Advance | `gpsp` | `libretro-gpsp` | PPA only |
| Nintendo — Game Boy Advance | `meteor` | `libretro-meteor` | PPA only |
| Nintendo — Game Boy Advance | `vba-next` | `libretro-vba-next` | PPA only |
| Nintendo — Game Boy/Game Boy Color | `gambatte` | `libretro-gambatte` | yes |
| Nintendo — Game Boy/Game Boy Color | `sameboy` | `libretro-sameboy` | yes |
| Nintendo — Game Boy/Game Boy Color | `tgbdual` | `libretro-tgbdual` | PPA only |
| Nintendo — Game Boy/Game Boy Color/Game Boy Advance | `mgba` | `libretro-mgba` | yes |
| Nintendo — Game Boy/Game Boy Color/Game Boy Advance | `vbam` | `libretro-vbam` | PPA only |
| Nintendo — GameCube / Wii | `dolphin` | `libretro-dolphin` | PPA only |
| Nintendo — Pokemon Mini | `pokemini` | `libretro-pokemini` | PPA only |
| Nintendo — Virtual Boy | `beetle-vb` | `libretro-beetle-vb` | yes |
| Panasonic/GoldStar/Sanyo — 3DO | `opera` | `libretro-opera` | PPA only |
| Rick Dangerous Game Engine | `xrick` | `libretro-xrick` | PPA only |
| Sega 8/16-bit (Various) | `genesisplusgx` | `libretro-genesisplusgx` | yes |
| Sega 8/16-bit + 32X (Various) | `picodrive` | `libretro-picodrive` | PPA only |
| Sega — Saturn | `beetle-saturn` | `libretro-beetle-saturn` | PPA only |
| Sega — Saturn | `yabasanshiro` | `libretro-yabasanshiro` | PPA only |
| Sega — Saturn | `yabause` | `libretro-yabause` | PPA only |
| Sharp X68000 | `px68k` | `libretro-px68k` | PPA only |
| Sinclair — ZX81 | `81` | `libretro-81` | PPA only |
| Sinclair/Amstrad — ZX Spectrum (various) | `fuse` | `libretro-fuse` | PPA only |
| Smith Engineering/General Consumer Electronics — Vectrex | `vecx` | `libretro-vecx` | PPA only |
| SNK — Neo Geo | `fbalpha2012-neogeo` | `libretro-fbalpha2012-neogeo` | PPA only |
| SNK — Neo Geo Pocket (Color) | `beetle-ngp` | `libretro-beetle-ngp` | PPA only |
| Sony PlayStation 2 | `lrps2` | `libretro-lrps2` | PPA only |
| Sony — PlayStation | `mednafen_psx` | `libretro-beetle-psx` | yes |
| Sony — PlayStation | `mednafen_psx_hw` | `libretro-beetle-psx` | yes |
| Sony — PlayStation | `pcsx-rearmed` | `libretro-pcsx-rearmed` | PPA only |
| MSX / SVI / ColecoVision / SG-1000 | `bluemsx` | `libretro-bluemsx` | PPA only |
| Super Nintendo Entertainment System | `bsnes` | `libretro-bsnes` | PPA only |
| Super Nintendo Entertainment System | `bsnes-mercury-accuracy` | `libretro-bsnes-mercury-accuracy` | yes |
| Super Nintendo Entertainment System | `bsnes-mercury-balanced` | `libretro-bsnes-mercury-balanced` | yes |
| Super Nintendo Entertainment System | `bsnes-mercury-performance` | `libretro-bsnes-mercury-performance` | yes |
| Super Nintendo Entertainment System | `snes9x` | `libretro-snes9x` | yes |
| Super Nintendo Entertainment System | `snes9x2005` | `libretro-snes9x2005` | PPA only |
| Super Nintendo Entertainment System | `snes9x2005plus` | `libretro-snes9x2005plus` | PPA only |
| Super Nintendo Entertainment System | `snes9x2010` | `libretro-snes9x2010` | PPA only |
| Various — Handheld Electronic | `gw` | `libretro-gw` | PPA only |
| Various — MSX | `fmsx` | `libretro-fmsx` | PPA only |
| Various — Music | `gme` | `libretro-gme` | PPA only |
| Arcade (various) | `fbalpha` | `libretro-fbalpha` | PPA only |

### Notes on that table

**Two packages ship more than one core**, so the package name alone is not a
`core =` value for them:

- `libretro-vice` ships ten Commodore cores (`vice_x64`, `vice_x128`,
  `vice_xvic`, …). There is no plain `vice` core — name the machine you want.
- `libretro-beetle-psx` ships both PlayStation renderers. `core = "beetle-psx"`
  selects the hardware one (`mednafen_psx_hw`); use `core = "mednafen_psx"` for
  the software renderer.

**Some names differ from the shared object on disk** and shepherd translates
them, so the name you install is the name you configure: the ten `beetle-*`
cores are libretro's Mednafen forks (`beetle-saturn` → `mednafen_saturn`),
`lrps2` → `pcsx2`, and `np2` → `nekop2`.

**Eight packages are transitional** — `catsfc`, `eightyone`, `fba`,
`fba-cps1`, `fba-cps2`, `fba-neogeo`, `glupen64`, `snes9x-next` — and only
depend on the renamed real package. Installing one works, but its old name is
not a valid `core =`; use the name it pulls in (`snes9x2005`, `81`, `fbalpha`,
`fbalpha2012-cps1`, …), which is what the table lists.

**`libretro-bash-launcher` is deliberately omitted.** It is not an emulator: it
makes RetroArch run shell scripts as "content". Installing it into a supervised
kiosk would turn any activity into arbitrary command execution, which defeats
the point of the sandbox.

**Cores that need a BIOS** cannot ship it — PlayStation, Saturn, PC-FX, 3DO and
the DS cores are the usual cases. Put the files the core's documentation names
in `~/.config/retroarch/system/`; without them the core loads and then fails on
the content.

## Trying it without a game you own

Freely licensed homebrew is a good way to check the setup end to end.
[jsmolka/gba-tests][gba-tests] is MIT-licensed, ships its ROMs in the
repository, and draws its result on screen, so you can tell at a glance whether
the emulator actually ran:

```sh
mkdir -p ~/Games/retroarch
curl -fL -o ~/Games/retroarch/gba-tests-arm.gba \
  https://raw.githubusercontent.com/jsmolka/gba-tests/master/arm/arm.gba
```

Point an entry's `content` at it with `core = "mgba"`. (It reports a failed
test number — that is the ROM grading the *emulator core*, not a problem with
shepherd.) [Homebrew Hub][hh] collects freely distributable homebrew for
several systems if you want an actual game to test with.

[gba-tests]: https://github.com/jsmolka/gba-tests
[hh]: https://hh.gbdev.io/

## Troubleshooting

Turn on `service.capture_child_output` and read the session log — RetroArch is
verbose about all of this, and `args = ["--verbose"]` makes it more so.

**The activity exits immediately.** Usually the core or the content could not
be loaded. `[Core] Loading dynamic libretro core from: …` names the core path
shepherd resolved. A core that isn't there gives

```
[WARN] --libretro argument "…" is not a file, core name or directory. Ignoring.
[ERROR] [Core] Frontend is built for dynamic libretro cores, but path is not set.
Fatal error received in: "init_libretro_symbols()"
```

You should not have to read a log to find this out: shepherd checks every
RetroArch entry's core *and* its content on its diagnostic sweep, and reports
what is missing against the activity, in the admin UIs and on the phone — a
missing core with the `shepherd-admin` command that installs it, missing content
with the path it looked for. An entry broken both ways says so once for each.
The conditions clear on the next sweep once the file is there — no restart, so
a ROM on removable media comes and goes with the drive.

A core named with `core =` that resolves nowhere is reported but not certain:
shepherd passes the bare filename on to RetroArch, which resolves it against its
own configured `libretro_directory`, so this can also mean "installed somewhere
shepherd does not search" — set `core_path` to the absolute path in that case. A
`core_path` that is not a file is reported as the plain error it is.

**Progress is lost between sessions.** Look for
`[State] Auto save state to "…" succeeded` and `[SRAM] Saving RAM type #0 to
"…"` at the end of the log, followed by `[Core] Unloading game…`. If those are
missing the process did not exit cleanly. Note that a game that never wrote to
its battery save has no `.srm` to keep — save inside the game once first.

**It does not resume where it stopped.** The next launch should log
`[State] Found auto save state in "…"` and `Auto-loading save state … succeeded`.
Nothing there means either `save_state = "off"`, or the state file was removed
— which is exactly what the reset button does.

**The game pauses whenever the HUD is touched.** `pause_nonactive` is being
overridden; check for a stray value in `args`.

**The picture only fills part of the screen** on a HiDPI panel. Do *not* reach
for `xwayland_native_resolution` here. That flag exists for XWayland clients
(issue #45), and RetroArch should not be one: the fragment asks for
`video_context_driver = "wayland"`, and measured on Ubuntu 26.04 with sway at
`scale 1.5` that gives a native Wayland client (`shell=xdg_shell` in
`swaymsg -t get_tree`, `[GL] Found GL context: "wayland"` in the session log)
rendering at the panel's full pixel grid. Setting the flag would drop every
output to scale 1.0 for the duration of the activity and buy nothing.

If the picture is blurry anyway, check which path it actually took:

```sh
swaymsg -t get_tree | jq -r '.. | objects | select(.pid) | "\(.name)\t\(.shell)"'
```

`xdg_shell` is Wayland. `xwayland` means something beat the fragment to it —
almost certainly a per-core or per-game override setting `video_context_driver`
(see above; shepherd warns about exactly this), or a `video_driver` in your own
`retroarch.cfg` that does its own windowing, such as `sdl2`. Fix that rather
than reaching for a compositor-wide scale override.

## Not covered

- **The Flatpak RetroArch** (`org.libretro.RetroArch`). Only the distro package
  is supported today; the Flatpak needs the generated fragment visible inside
  its sandbox and keeps its config elsewhere.
- **Other emulator frontends.** Every mechanism here is RetroArch-specific —
  its config keys, its save-state naming — which is why the entry kind is
  `retroarch` rather than a vague `emulator`. A future kind can sit beside it.
- **Netplay, achievements, shaders, per-core options.** Configure them in
  RetroArch itself; shepherd only appends the settings listed above.
