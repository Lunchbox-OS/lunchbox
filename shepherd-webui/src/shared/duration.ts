/**
 * Duration formatting and parsing, shared by the management UI and the config
 * editor.
 *
 * Lives outside `src/api/` because `src/config/` must not import the API layer
 * — the standalone config-editor bundle has no daemon to talk to, and pulling
 * `api/types.ts` in would drag axios and the wire types along with it.
 */

/** `3660` -> `"1:01:00"`, `90` -> `"1:30"`. For live countdowns. */
export function formatDuration(secs: number): string {
  if (secs <= 0) return "0:00";
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = Math.floor(secs % 60);
  if (h > 0)
    return `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
  return `${m}:${String(s).padStart(2, "0")}`;
}

/** `5400` -> `"1h 30m"`. For budgets, where seconds are noise. */
export function formatDurationHuman(secs: number): string {
  if (secs <= 0) return "0 min";
  const h = Math.floor(secs / 3600);
  const m = Math.round((secs % 3600) / 60);
  if (h > 0 && m > 0) return `${h}h ${m}m`;
  if (h > 0) return `${h}h`;
  return `${m}m`;
}

/**
 * Parse what {@link formatDurationHuman} prints, plus the shapes people
 * actually type: `90`, `90m`, `1h30`, `1h 30m`, `1:30`, `2h`.
 *
 * Returns null when nothing numeric is found, so a half-typed field can be
 * left alone rather than snapping to zero on every keystroke.
 */
export function parseDurationHuman(input: string): number | null {
  const text = input.trim().toLowerCase();
  if (!text) return null;

  // `1:30` / `1:30:00` — colon-separated, largest unit first.
  if (/^\d+(:\d{1,2}){1,2}$/.test(text)) {
    const parts = text.split(":").map(Number);
    if (parts.length === 2) return parts[0] * 3600 + parts[1] * 60;
    return parts[0] * 3600 + parts[1] * 60 + parts[2];
  }

  // `1h 30m`, `90m`, `2h`, `45s`.
  const unit = /(\d+(?:\.\d+)?)\s*(h|hr|hrs|hour|hours|m|min|mins|minute|minutes|s|sec|secs|second|seconds)/g;
  let total = 0;
  let matched = false;
  for (const m of text.matchAll(unit)) {
    matched = true;
    const value = Number(m[1]);
    const u = m[2];
    if (u.startsWith("h")) total += value * 3600;
    else if (u.startsWith("m")) total += value * 60;
    else total += value;
  }
  if (matched) return Math.round(total);

  // A bare number is minutes — the unit people mean when they say "30".
  if (/^\d+(\.\d+)?$/.test(text)) return Math.round(Number(text) * 60);

  return null;
}
