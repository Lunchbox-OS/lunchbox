# Typing in the config editor dropped characters (#235)

The prompt was "investigate #235", then, after the investigation below, "go
ahead with the render fix, then push a draft PR". The issue:

> The config editor is slow to type in some fields, likely due to validation
> being run on every keypress.
>
> In particular, I noticed this when typing in a Flatpak name.

In follow-up: the device UI built from `main`'s HEAD, in desktop Firefox;
characters "failed to appear" when typing quickly, and the page never went
blank.

## Not validation

Validation was already debounced (120ms, `ConfigDocProvider`), and cheap.
Loading the shipped wasm in Node against the 58 KB `config.example.toml`: an
`apply` is 0.6ms, `text()` 0.2ms, `view()` 0.5ms, `validate()` 0.4ms.

Nor was it Flatpak: the Icon field measured the same. Every text field goes
through `DraftTextField` (added in `000e22c2` for an earlier version of this
symptom), which already shows the typed character before the projection
catches up.

## What a keystroke actually cost

Rendering the real `ConfigApp` in jsdom with the real wasm and timing each
keystroke with a React `Profiler`: two whole-editor commits per character,
~15ms each, of which the activities board (every card, dnd-kit) was ~8ms and
the open drawer ~4ms. Neither commit could draw anything new: the forms read
`view`, which only changes after the debounce.

- The first was `apply` bumping `version`, which changed the context value, so
  every `useConfigDoc` consumer re-rendered. `ConfigShell` is one, so the whole
  tree did.
- The second was an effect keyed on `version` that read `doc.text()` into
  state.

## A crash on the way

Driving Chromium with Playwright and firing keystrokes without waiting for each
to be handled, the editor threw React's "Maximum update depth exceeded"
(minified #185 in production) and blanked: at ~50-60 queued keys in a
production build, and in a development build (`lunchbox dev webui`) at 20
characters a second. A plain MUI field (the "Add activity" dialog's Label) took
120 queued keys without complaint.

Instrumenting `scheduleUpdateOnFiber` in the dev build of react-dom showed the
mechanism. React counts a commit as a nested update when it finishes with
sync/default work still pending, and resets only when one finishes clean. Each
keystroke left a Default-lane update behind: the `setText` effect above, and
MUI `InputBase`'s passive `setAdornedStart` effect, which re-runs whenever the
FormControl context changes. The next queued keystroke, being a discrete event,
always ran first, so the count climbed by one per key until it hit 50.

This is not what the reporter saw (their page never blanked), but it has the
same cause.

## What could not be reproduced

No configuration here dropped a character without crashing: Chromium and
Firefox via Playwright, the standalone and device builds, real snap Firefox in
the headless session driven by Marionette with keys typed through `wtype`, and
the same with `GTK_IM_MODULE=ibus`. A GNOME desktop routes Firefox's keys
through the compositor's input method, which the headless session cannot. The
working theory is that Firefox's input-method path drops keys while the page is
busy for tens of milliseconds a key, where a direct key event would queue. It
was not confirmed. The fix makes each key cheap either way.

## The fix

Three commits:

1. `vitest.config.ts` resolves `src/config/wasm/lunchbox_config` to a
   placeholder when the file has not been generated. CI's web test job has no
   Rust toolchain, and `vi.mock` still needs its target to resolve, which is
   why every earlier DOM test mocked `ConfigDocProvider` whole and none could
   test the provider.
2. The provider reads the text in the same update as the version bump, instead
   of in an effect: one render per edit, and no update of its own left queued.
3. What changes on every edit (`text`, `dirty`, `canUndo`, `canRedo`) moved to
   a second context, `useConfigDocLive`, read only by small toolbar components
   (`ReloadButton`, `SaveButton`, `UndoRedo`, `DirtyMark`) and `RawTomlPane`.
   `useConfigDoc` now changes only when `view` or `report` does.
   `availabilityFor` follows `view` too, which agrees with the windows it is
   drawn beside.

`src/config/doc/renders.test.tsx` pins both down with a stand-in document:
an edit renders a live-context reader exactly once, and a form not at all until
the debounce.

## Measured after

- jsdom profile: ~1-5ms of React work per keystroke, from ~30-40ms. The one
  render when the debounce lands (~15-20ms) is unchanged; memoizing
  `EntryCard` would cut it, and was left out as not needed for this.
- Chromium, both builds: 120 queued keys, no crash, nothing lost. Typing at
  0-50ms a key in the dev build, correct.
- The toolbar still follows edits (Undo/Redo enable, the TOML pane shows the
  live text after an undo).

## Tooling notes

- `pkill -f "rsbuild dev"` from the Bash tool kills the tool's own shell (the
  pattern is in its command line); kill `pgrep rsbuild-node` pids instead.
- The rsbuild dev server caches `node_modules`; an instrumented react-dom is
  only picked up after a restart.
- `wtype` drops the first key of each invocation while the virtual keyboard
  binds, and rejects `-d 0`. Lead with a throwaway `-P Shift_L -p Shift_L -s
  400`. `-P`/`-p` take keysym names (`Shift_L`), not modifier names.
- In the editor with an activity drawer open, the toolbar is `aria-hidden`
  (the drawer is modal), so select its buttons by `button[aria-label=…]`.
