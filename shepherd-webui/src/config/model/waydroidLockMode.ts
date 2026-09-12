/**
 * How an Android activity is locked into its app, and how to name each mode to
 * a person.
 *
 * The values are `RawWaydroidLockMode` from `config.generated.ts`, so a mode
 * added to the Rust schema fails the type check here until it is given a label
 * rather than silently going missing from the menu.
 */
import type { RawWaydroidLockMode } from "./config.generated";
import { LOAD_TIME_DEFAULTS } from "./field-defaults.generated";

export const WAYDROID_LOCK_MODES: ReadonlyArray<{
  value: RawWaydroidLockMode;
  label: string;
  description: string;
}> = [
  {
    value: "statusbar",
    label: "Status bar",
    description:
      "Disables the notification shade, quick settings and the nav-bar " +
      "home/recents buttons — the routes out of the app that reach Android " +
      "Settings. Each app keeps its own window.",
  },
  {
    value: "locktask",
    label: "Lock Task (strict)",
    description:
      "Pins the app using Android's own Lock Task Mode, which blocks home, " +
      "recents and app-switching at the framework level. Android is presented " +
      "as one full-screen surface rather than per-app windows.",
  },
  {
    value: "off",
    label: "Off",
    description:
      "No lock-in: the child can reach the Android home screen and Settings " +
      "from inside the activity.",
  },
];

/**
 * The mode a device uses when nothing chooses one, and its human name.
 *
 * `lock_mode` is an `Option` the daemon resolves in `Policy::from_raw`, so
 * `schemars` reports no default for it and the value comes from
 * `LoadTimeDefaults` instead — the same arrangement as the HUD edge, and for
 * the same reason: the menu's "unset" row has to say which mode that means,
 * and saying "Status bar" in prose would be a mirror of the Rust with nothing
 * keeping it honest.
 */
export const DEFAULT_WAYDROID_LOCK_MODE =
  LOAD_TIME_DEFAULTS.waydroid_lock_mode as RawWaydroidLockMode;

export const DEFAULT_WAYDROID_LOCK_MODE_LABEL: string =
  WAYDROID_LOCK_MODES.find((m) => m.value === DEFAULT_WAYDROID_LOCK_MODE)
    ?.label ?? DEFAULT_WAYDROID_LOCK_MODE;
