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

export const HUD_ORIENTATIONS: ReadonlyArray<{
  value: RawHudOrientation;
  label: string;
}> = [
  { value: "top", label: "Top" },
  { value: "bottom", label: "Bottom" },
  { value: "left", label: "Left (vertical)" },
];

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
