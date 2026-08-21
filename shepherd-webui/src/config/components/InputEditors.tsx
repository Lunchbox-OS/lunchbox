/**
 * Input compatibility sidecars and hardware requirements.
 *
 * Two orthogonal things that both concern input, and are easy to confuse:
 * `input_compat` launches a translation sidecar while the activity runs, while
 * `requires_input` merely hides the activity when a device is not attached.
 *
 * The one rule worth enforcing in the UI is that the three touch modes are
 * mutually exclusive — the daemon rejects combinations, and a checkbox set that
 * lets you build an invalid one is a checkbox set that wastes your time.
 */
import Alert from "@mui/material/Alert";
import Checkbox from "@mui/material/Checkbox";
import FormControlLabel from "@mui/material/FormControlLabel";
import Stack from "@mui/material/Stack";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import { useFields } from "../doc/useFields";
import type {
  RawInputCompat,
  RawInputCompatOptions,
  RawInputDevice,
} from "../model/config.generated";
import { Section } from "./Section";

const TOUCH_MODES: RawInputCompat[] = ["touch_to_mouse", "tablet_to_touch", "disable_touch"];
const GAMEPAD_MODES: RawInputCompat[] = ["gamepad_productivity", "gamepad_gpd"];

const COMPAT_LABELS: Record<RawInputCompat, string> = {
  touch_to_mouse: "Touch acts as a mouse",
  tablet_to_touch: "Tablet/pen acts as touch",
  disable_touch: "Disable the touchscreen",
  gamepad_productivity: "Gamepad: productivity preset",
  gamepad_gpd: "Gamepad: FPS / GPD preset",
};

const COMPAT_HINTS: Record<RawInputCompat, string> = {
  touch_to_mouse: "For programs that ignore touch events entirely.",
  tablet_to_touch: "The inverse; for programs that only understand touch.",
  disable_touch: "Grabs every touchscreen and discards its events.",
  gamepad_productivity:
    "Sticks drive the mouse and scroll, D-pad is arrows, A is Enter, Start is Escape.",
  gamepad_gpd: "Left stick is WASD, right stick is the mouse, triggers are mouse buttons.",
};

const DEVICE_LABELS: Record<RawInputDevice, string> = {
  mouse: "Mouse or trackpad",
  touch: "Touchscreen",
  keyboard: "Keyboard",
  gamepad: "Gamepad",
};

export function InputCompatEditor({
  basePath,
  compat,
  options,
}: {
  basePath: string;
  compat: RawInputCompat[];
  options: RawInputCompatOptions | null | undefined;
}) {
  const f = useFields(basePath);
  const opts = useFields(`${basePath}.input_compat_options`);

  const selectedTouch = compat.find((c) => TOUCH_MODES.includes(c));
  const gamepadOn = compat.some((c) => GAMEPAD_MODES.includes(c));

  const setCompat = (next: RawInputCompat[]) =>
    next.length === 0 ? f.unsetField("input_compat") : f.setField("input_compat", next);

  const toggleTouch = (mode: RawInputCompat) => {
    // Only one touch mode at a time; picking a second replaces the first.
    const withoutTouch = compat.filter((c) => !TOUCH_MODES.includes(c));
    setCompat(selectedTouch === mode ? withoutTouch : [...withoutTouch, mode]);
  };

  const toggleGamepad = (mode: RawInputCompat) => {
    const withoutGamepad = compat.filter((c) => !GAMEPAD_MODES.includes(c));
    setCompat(compat.includes(mode) ? withoutGamepad : [...withoutGamepad, mode]);
  };

  return (
    <Section
      title="Input compatibility"
      description="Sidecars that translate input while this activity runs."
      present={compat.length > 0}
      onTogglePresent={(on) => (on ? setCompat(["touch_to_mouse"]) : setCompat([]))}
    >
      <Stack spacing={2}>
        <Stack>
          <Typography variant="caption" color="text.secondary">
            Touch — pick at most one
          </Typography>
          {TOUCH_MODES.map((mode) => (
            <FormControlLabel
              key={mode}
              control={
                <Checkbox
                  checked={selectedTouch === mode}
                  onChange={() => toggleTouch(mode)}
                  size="small"
                />
              }
              label={
                <Stack>
                  <Typography variant="body2">{COMPAT_LABELS[mode]}</Typography>
                  <Typography variant="caption" color="text.secondary">
                    {COMPAT_HINTS[mode]}
                  </Typography>
                </Stack>
              }
            />
          ))}
        </Stack>

        <Stack>
          <Typography variant="caption" color="text.secondary">
            Gamepad — stacks with a touch mode
          </Typography>
          {GAMEPAD_MODES.map((mode) => (
            <FormControlLabel
              key={mode}
              control={
                <Checkbox
                  checked={compat.includes(mode)}
                  onChange={() => toggleGamepad(mode)}
                  size="small"
                />
              }
              label={
                <Stack>
                  <Typography variant="body2">{COMPAT_LABELS[mode]}</Typography>
                  <Typography variant="caption" color="text.secondary">
                    {COMPAT_HINTS[mode]}
                  </Typography>
                </Stack>
              }
            />
          ))}
        </Stack>

        {gamepadOn && (
          <Stack direction="row" spacing={2}>
            <TextField
              size="small"
              type="number"
              label="Stick deadzone"
              slotProps={{ htmlInput: { step: 0.05, min: 0, max: 1 } }}
              value={options?.gamepad_deadzone ?? ""}
              onChange={(e) =>
                opts.setField(
                  "gamepad_deadzone",
                  e.target.value === "" ? undefined : Number(e.target.value),
                )
              }
              helperText="0–1"
            />
            <TextField
              size="small"
              type="number"
              label="Mouse speed"
              value={options?.gamepad_mouse_speed ?? ""}
              onChange={(e) =>
                opts.setField(
                  "gamepad_mouse_speed",
                  e.target.value === "" ? undefined : Number(e.target.value),
                )
              }
              helperText="px/s at full deflection"
            />
            <TextField
              size="small"
              type="number"
              label="Scroll speed"
              value={options?.gamepad_scroll_speed ?? ""}
              onChange={(e) =>
                opts.setField(
                  "gamepad_scroll_speed",
                  e.target.value === "" ? undefined : Number(e.target.value),
                )
              }
              helperText="wheel units/s"
            />
          </Stack>
        )}

        {selectedTouch === "disable_touch" && (
          <Alert severity="info">
            The touchscreen is grabbed and its events discarded for the whole session.
          </Alert>
        )}
      </Stack>
    </Section>
  );
}

export function RequiresInputEditor({
  basePath,
  devices,
}: {
  basePath: string;
  devices: RawInputDevice[];
}) {
  const f = useFields(basePath);

  const toggle = (device: RawInputDevice) => {
    const next = devices.includes(device)
      ? devices.filter((d) => d !== device)
      : [...devices, device];
    if (next.length === 0) f.unsetField("requires_input");
    else f.setField("requires_input", next);
  };

  return (
    <Section
      title="Required hardware"
      description="Hide this activity unless every listed device is attached."
      present={devices.length > 0}
      onTogglePresent={(on) =>
        on ? f.setField("requires_input", ["keyboard"]) : f.unsetField("requires_input")
      }
    >
      <Stack>
        {(Object.keys(DEVICE_LABELS) as RawInputDevice[]).map((device) => (
          <FormControlLabel
            key={device}
            control={
              <Checkbox
                size="small"
                checked={devices.includes(device)}
                onChange={() => toggle(device)}
              />
            }
            label={DEVICE_LABELS[device]}
          />
        ))}
      </Stack>
    </Section>
  );
}
