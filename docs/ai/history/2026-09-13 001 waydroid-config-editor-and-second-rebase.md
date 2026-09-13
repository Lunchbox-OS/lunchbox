# Wiring `[service.waydroid]` into the config editor, and a second rebase (#2)

<https://git.armeafamily.com/albert/shepherd-launcher/pulls/75>

## Prompts

> hm let's just fix the web configuration UI first
> [`npm run check:coverage` output: 7 unreachable schema fields]

> drop lock_down from the schema entirely, then rebase on the current main

## How the gap got through

`shepherd-webui` has a CI check, `npm run check:coverage`
(`scripts/check-schema-coverage.mjs`, `ci.yml`), that fails when a schema field
is unreachable from the editor. The [2026-09-12 rebase](./2026-09-12%20001%20android-branch-rebase-onto-main.md)
verified `tsc` and `npm test` but not this, so all seven fields of
`[service.waydroid]` — a table that predates main having a config editor at all
— reached the operator instead of the build. Worth knowing for next time:
**`tsc` + `vitest` is not the webui's full check set.** It is
`typecheck`, `test`, `check:coverage` and `check:boundary`.

## Why the UI could not just be wired

`lock_mode` was `Option<String>`. Codegen therefore emitted
`lock_mode?: string | null`, which gives a picker nothing to enumerate, and the
three valid slugs already existed in three places: the doc comment, the
`LockMode::parse` match, and a hand-rolled check in `validate_config` whose
error string listed them a fourth time.

Hand-writing the options in TSX would have been a fifth copy, and exactly the
drift `RawSponsorBlockCategory`'s doc comment says the schema uses enums to
prevent. So `lock_mode` became `RawWaydroidLockMode`, shaped like
`RawHudOrientation`:

- the editor builds the menu from the generated union, so a mode added to the
  schema and not to the menu is a build error;
- serde refuses a typo when the file is *parsed*, naming the alternatives, which
  **deleted** the hand-rolled validator rather than adding a mirror of it.

The TOML spelling is unchanged. Case-insensitivity is gone — `lock_mode =
"LockTask"` no longer parses — which matches every other enum in the schema and
costs nothing on a version that has never shipped.

The four defaults `WaydroidConfig::from_raw` resolves joined `LoadTimeDefaults`,
so the form's placeholders and its "(the default)" rows come from the daemon
rather than from numbers typed into a form. `multi_window` and
`suspend_when_idle` needed named constants for that, which is the arrangement
that module's parse test exists to hold together.

## Three UI decisions

- **`preboot` is a three-state menu, not a switch.** Unset means "decide from
  whether any Android activity exists", which is neither always nor never and is
  what most devices should be on. A switch would have had to fold it into one of
  the other two.
- **Picking `locktask` warns, there and then, that it needs the DPC.** A device
  set to it without a provisioned device owner does not launch Android at all,
  and the config editor is where that gets chosen.
- **`lock_down` got no control** — and then, on the follow-up ask, no schema
  field either. It was the deprecated bool `lock_mode` replaced *within this
  branch*, so nothing released carries it. Removing it took the two-input
  resolution rule and the coverage-check exemption with it.

  One caveat recorded because it is invisible: `RawWaydroidConfig` does not
  `deny_unknown_fields` (almost nothing in the schema does), so a file that
  still sets `lock_down` parses and is **ignored** rather than refused. A device
  that had `lock_down = false` moves from "off" to the statusbar default —
  reachable only for someone who ran this branch pre-merge, and locking down
  harder than asked is the safe direction for that miss.

## Seeing it, cheaply

The editor has a **standalone** build target (`npm run build:standalone`,
`SHEPHERD_UI_TARGET=standalone`) that carries no API layer and talks to no
daemon. Served from `python3 -m http.server` and driven through Firefox's
Marionette in the headless session, it renders the real editor with
`config.example.toml` loaded in about a minute — no `web_assets.rs` rebuild, no
login, no daemon. That is a much cheaper loop than the embedded SPA route the
`headless-dev` skill describes, and it is the right one for any config-editor
change.

Two notes on it: `WebDriver:TakeScreenshot` beats `dev shot` here, because the
launcher's first-boot setup card is a compositor overlay that sits over the
browser window; and Marionette allows only one session at a time, so a scenario
has to be one script (a second connection re-navigates and resets the page).

## The second rebase

`origin/main` had moved 3 commits (#191, multiple companion bonds). 82 commits
replayed with **one conflict**: main added an `admins` field to the
`DefaultManagementService` literal in `make_svc_full`, inside the same
`tests/dispatch.rs` hunk where this branch changed the tail to return the
`MockHost`. Both belong; the field joins the literal and the branch's
`(svc, host)` still closes it.

The generated wire outputs needed no regeneration: main's new RPCs and this
branch's textually merged to exactly what `rpc-codegen` produces, which
`codegen_outputs_match_checked_in` then confirmed rather than us assuming it.

## Verification

`cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D
warnings`; `cargo test --workspace --all-targets` (**71 binaries, 1422 tests, 0
failures**); `./scripts/shepherd version check`; `config validate
config.example.toml`; `check-arch-neutral.sh`; `check-workflows.sh`; CI's
shellcheck invocation; in `shepherd-webui` `typecheck`, `check:coverage` (152
fields reachable, 2 exempt), `check:boundary` and 128 vitest cases across 16
files; `:app:testDebugUnitTest` in `companion-android`.

End-to-end in the headless session: the pre-boot cover went up and came down
(`startup_busy` true → false), "Waydroid pre-boot complete; session warm", the
grid rendered, and the Android entry stayed gated on `NotReady { kind: Android }`
— correct here, since boot-completion is read through the privileged helper,
which this box does not have installed.
