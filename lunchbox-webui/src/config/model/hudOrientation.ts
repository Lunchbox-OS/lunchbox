/**
 * The screen edges the HUD can occupy, and how to name them to a person.
 *
 * Two places offer this choice — the device-wide `[service.hud] orientation`
 * and an activity's own `hud_orientation` — and they must offer the same edges
 * with the same wording, so the list lives here rather than in either.
 *
 * The values are `RawHudOrientation` from `config.generated.ts`, so a new edge
 * added to the Rust schema fails the type check here until it is given a label
 * rather than silently going missing from both menus.
 */
import type { RawHudOrientation } from "./config.generated";
import { LOAD_TIME_DEFAULTS } from "./field-defaults.generated";

export const HUD_ORIENTATIONS: ReadonlyArray<{
  value: RawHudOrientation;
  label: string;
}> = [
  { value: "top", label: "Top" },
  { value: "bottom", label: "Bottom" },
  { value: "left", label: "Left (vertical)" },
];

/**
 * The edge a device uses when nothing chooses one, and its human name.
 *
 * `[service.hud] orientation` is an `Option` the daemon resolves in
 * `Policy::from_raw` via `HudOrientation::default()`, so `schemars` reports no
 * default for it and the value comes from `LoadTimeDefaults` instead. Both
 * menus offer an empty "unset" row that has to say which edge that means, and
 * saying "Top" in prose would be a mirror of the Rust with nothing to keep it
 * honest — the more so because this is one of the few defaults a person might
 * plausibly want changed later.
 */
export const DEFAULT_HUD_ORIENTATION: RawHudOrientation =
  LOAD_TIME_DEFAULTS.hud_orientation;

export const DEFAULT_HUD_ORIENTATION_LABEL: string =
  HUD_ORIENTATIONS.find((o) => o.value === DEFAULT_HUD_ORIENTATION)?.label ??
  DEFAULT_HUD_ORIENTATION;

/**
 * What the vertical HUD actually is, for the help text under either menu.
 *
 * Worth stating wherever the choice is offered: "left" reads like a small
 * placement tweak, and it is in fact a different layout — the bar rotated a
 * quarter turn, with the activity name read bottom-to-top and the wall clock
 * as an analog face.
 */
export const VERTICAL_HUD_DESCRIPTION =
  "Left is the vertical HUD: the same bar rotated a quarter turn, with the " +
  "end-session button at the top, the sliders vertical, the activity name " +
  "read bottom-to-top and the wall clock as an analog face.";
