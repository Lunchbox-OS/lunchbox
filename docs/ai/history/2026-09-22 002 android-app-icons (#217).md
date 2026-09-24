# The Android apps wear the tin (#217)

<https://github.com/aarmea/lunchbox/issues/217>

## The prompt

> implement #217. you may install the android SDK as specified in this repo

The issue, "Change Android app logo":

> The companion and dedicated media apps currently use a shield as their logo.
> Replace them with the new lunchbox-based branding.
>
> (example only: when implementing, extract from the below)
>
> lunchbox-branding(1).zip

The zip is a later cut of the #207 hand-off. Beside what `assets/branding`
already had, it adds `icon/apps/` (the two sibling marks) and
`icon/placeholders/` (per-kind activity icons for the launcher). Only
`icon/apps/` belongs to this issue, so only it was brought in.

## What was built

![The companion and media icons, each in colour and as an Android 13 themed icon](2026-09-22-002-android-app-icons/icons.png)

Both apps' adaptive icons are now the hand-off's marks: the companion is the
tin with its packed column swapped for a padlock, the media app the tin with
one compartment and a large play. The background layer is the cream
`#F7F5EE` the marks were drawn on, in place of the old dark green.

Each app also gained the `<monochrome>` layer. Neither had one, so on a
themed Android 13+ home screen both showed in full colour among themed tiles.

## Translating the SVGs

The hand-off ships each layer as SVG; Android wants VectorDrawable, which has
no `<rect>`, `<circle>` or `<mask>`. The translation is
[`vector_drawables.py`](2026-09-22-002-android-app-icons/vector_drawables.py),
kept here so the drawables can be regenerated if the marks change:

- Rects and circles become path data. The mark keeps the hand-off's 256-unit
  grid inside a group with `translate(24 24) scale(60/256)`, the same
  transform the SVGs use, so the numbers can be read against them.
- The lip shadow's `rgba(28,27,24,0.14)` is `#241C1B18`.
- The monochrome layers cut the wells out with a `<mask>`. Here they are
  holes in the tin's path under `fillType="evenOdd"`. Even-odd cancels where
  two holes overlap, and the padlock's shackle overlaps its body, so the two
  are one outline: the body's rounded rect with the shackle's outer edge run
  over the top. The pocket inside the shackle is a further subpath, which
  even-odd fills back in. The handle tab overlaps the tin, so it is a path of
  its own.

Every layer was checked by turning the VectorDrawable back into SVG, rendering
it and the hand-off's SVG at 432 px with cairosvg, and diffing them. The only
differences were antialiasing along edges. cairosvg draws the monochrome
masks as solid black, so for those the reference was composited by hand: the
masked group, times the rendered mask, then the unmasked marks on top.

## Verified, and not

- `companion-android`: `assembleDebug`, `testDebugUnitTest` and `lintDebug`
  pass. Lint went from 41 warnings to 39: the two that went were
  `MonochromeLauncherIcon`, one for each adaptive icon.
- `lunchbox-media-android`: `assembleDebug` builds. `lintDebug` fails on an
  error that was already there: `windowLayoutInDisplayCutoutMode` in
  `themes.xml` needs API 27 and the app's minimum is 24. CI does not lint this
  app.
- `aapt2 dump resources` shows both APKs carry the new background colour and
  the monochrome drawable.
- **Not seen on a device** when this was first written: no phone was
  attached and the SDK here has no emulator. See the next section.

## Seen on a device

The follow-up prompt, on the branch's pull request (#222):

> validate that the icons look as they expect on device

and then, after the checks below:

> update the history note and fix the media app name

![Left to right: companion and media on a Pixel 10a in colour, the same as themed icons, then both on a moto g power (2021)](2026-09-22-002-android-app-icons/on-device.png)

Both apps were installed from this branch and drawn by two launchers:

- **Pixel 10a, Android 16 (SDK 37), Pixel Launcher.** Colour icons in the
  app drawer are the marks on cream, whole inside the circle crop, handle
  tab included. With themed icons on, both draw their monochrome layer and
  match the right-hand tiles of `icons.png`: the wells are holes, the
  padlock's shackle pocket is filled back in, the handle tab is solid, and
  the mark sits at the same size as the system's themed icons.
- **moto g power (2021), Android 11 (SDK 30), Moto launcher.** Colour icons
  are right. This Android has no themed icons, so there was no monochrome
  layer to check.

Things that got in the way, for whoever does this next:

- **The Pixel's companion was a release build** (0.5.1, a different
  signing key), so the debug APK would not install over it, and
  uninstalling it would have erased its admin records and claim tokens.
  Instead, a side-by-side copy was built with an init script that is not
  part of the repo:

  ```groovy
  allprojects {
      plugins.withId('com.android.application') {
          android { buildTypes { debug { applicationIdSuffix '.icontest' } } }
      }
  }
  ```

  `./gradlew -I suffix.gradle :app:assembleDebug` gives
  `com.lunchboxos.companion.icontest`. It was installed next to the real
  one and uninstalled after.
- **Pixel Launcher themes icons only on the home screen**, not in the app
  drawer, so each app went on the home screen (long-press, "Add to home
  screen"). The switch is in Wallpaper & style, Home screen, Icons: pick
  "Minimal" (Android 16's name for themed icons), then Apply. adb cannot
  flip it: the launcher's `grid_control` provider needs `BIND_WALLPAPER`,
  which the shell user doesn't have. The phone was put back to "Default"
  afterwards.

The drawer also showed the media app's launcher label as `lunchbox-media`.
That was the crate name, left in `strings.xml` from the app's first
scaffold (as `shepherd-media`), and it was not an icon problem. It is now
"Lunchbox Media", which the F-Droid listing and `docs/INSTALL.md` already
called it; `aapt2 dump badging` and the Pixel's drawer both show the new
name.

## Left alone

- **The companion's splash and theme are still green** (`splash_background`,
  `LunchboxGreen` in `ui/theme/Theme.kt`). The splash matches the Compose
  theme the app opens into, and retheming the app is more than a logo change.
- **The media app has no icon below API 26.** Its `minSdk` is 24, but its only
  `ic_launcher` is in `mipmap-anydpi-v26`, and that was true of the shield too.
  The hand-off's PNGs would serve as legacy mipmaps if it matters.
- **The notification icon** (`-mono-white`) is not used: neither app posts
  notifications.
- **`icon/placeholders/`** from the same zip is for the launcher, not these
  apps.
