import { describe, expect, it } from "vitest";
import {
  ALL_DAYS,
  WEEKDAYS,
  WEEKENDS,
  bandsOnDay,
  describeDays,
  formatDays,
  formatTime,
  mergeSpans,
  mergeableGroups,
  parseDays,
  parseTime,
  parseWindow,
  snap,
  toggleDay,
} from "./windows";
import fixture from "../../../../crates/lunchbox-config-wasm/tests/time_formats.json";

describe("day masks", () => {
  it("reads the presets the daemon accepts", () => {
    expect(parseDays("all")).toBe(ALL_DAYS);
    expect(parseDays("every")).toBe(ALL_DAYS);
    expect(parseDays("daily")).toBe(ALL_DAYS);
    expect(parseDays("weekdays")).toBe(WEEKDAYS);
    expect(parseDays("weekends")).toBe(WEEKENDS);
  });

  it("reads both short and long day names", () => {
    expect(parseDays(["mon", "wed", "fri"])).toBe(0b10101);
    expect(parseDays(["monday", "wednesday", "friday"])).toBe(0b10101);
  });

  it("is case-insensitive, like parse_days", () => {
    expect(parseDays("WEEKDAYS")).toBe(WEEKDAYS);
    expect(parseDays(["MON"])).toBe(1);
  });

  it("returns null for anything the daemon would reject", () => {
    expect(parseDays("fortnightly")).toBeNull();
    expect(parseDays(["funday"])).toBeNull();
  });
});

describe("formatDays preserves what the user wrote", () => {
  it("leaves a preset alone when it still matches", () => {
    expect(formatDays(ALL_DAYS, "daily")).toBe("daily");
    expect(formatDays(ALL_DAYS, "every")).toBe("every");
    expect(formatDays(WEEKDAYS, "weekdays")).toBe("weekdays");
  });

  it("leaves an explicit list alone when it still matches", () => {
    expect(formatDays(0b10101, ["mon", "wed", "fri"])).toEqual(["mon", "wed", "fri"]);
    expect(formatDays(0b1, ["monday"])).toEqual(["monday"]);
  });

  it("keeps long day names long when the set changes", () => {
    expect(formatDays(0b11, ["monday"])).toEqual(["monday", "tuesday"]);
  });

  it("keeps an explicit list explicit rather than collapsing to a preset", () => {
    // Someone who spelled out the days gets days back, not "weekdays".
    expect(formatDays(WEEKDAYS, ["mon", "tue", "wed", "thu"])).toEqual([
      "mon",
      "tue",
      "wed",
      "thu",
      "fri",
    ]);
  });

  it("uses a preset for a fresh window", () => {
    expect(formatDays(ALL_DAYS)).toBe("all");
    expect(formatDays(WEEKDAYS)).toBe("weekdays");
    expect(formatDays(WEEKENDS)).toBe("weekends");
    expect(formatDays(0b101)).toEqual(["mon", "wed"]);
  });
});

describe("times", () => {
  it("round-trips HH:MM", () => {
    expect(parseTime("16:30")).toBe(990);
    expect(formatTime(990)).toBe("16:30");
    expect(formatTime(0)).toBe("00:00");
  });

  it("rejects what parse_time rejects", () => {
    expect(parseTime("24:00")).toBeNull();
    expect(parseTime("12:60")).toBeNull();
    expect(parseTime("nope")).toBeNull();
  });

  // Whatever the daemon runs on, the grid has to draw — including the parts of
  // `parse_time` that are accidents of `u8::from_str`.
  it("takes the loose spellings the daemon takes", () => {
    expect(parseTime("16:5")).toBe(965);
    expect(parseTime("016:030")).toBe(990);
    expect(parseTime("+6:30")).toBe(390);
  });

  // And the reverse: a value the daemon flags must not draw a band as if it
  // were fine.
  it("rejects surrounding space, which the daemon does not allow", () => {
    expect(parseTime(" 16:30")).toBeNull();
    expect(parseTime("16:30 ")).toBeNull();
  });

  it("snaps to the grid step", () => {
    expect(snap(967, 15)).toBe(960);
    expect(snap(968, 15)).toBe(975);
  });
});

describe("bands on the grid", () => {
  const win = (days: string | string[], start: string, end: string) =>
    parseWindow({ days, start, end }, 0);

  it("draws nothing on a day outside the mask", () => {
    expect(bandsOnDay(win("weekdays", "16:00", "18:00"), 5)).toEqual([]);
  });

  it("draws one band for an ordinary window", () => {
    expect(bandsOnDay(win("weekdays", "16:00", "18:00"), 0)).toEqual([
      { start: 960, end: 1080 },
    ]);
  });

  it("draws nothing for a zero-width window, because end is exclusive", () => {
    expect(bandsOnDay(win("all", "09:00", "09:00"), 0)).toEqual([]);
  });

  it("draws two bands on the same day for a window that wraps midnight", () => {
    // The engine tests the day mask against the instant's weekday, so a
    // Friday 22:00-02:00 window never reaches Saturday.
    const w = win(["fri"], "22:00", "02:00");
    expect(w.wraps).toBe(true);
    expect(bandsOnDay(w, 4)).toEqual([
      { start: 1320, end: 1440 },
      { start: 0, end: 120 },
    ]);
    expect(bandsOnDay(w, 5)).toEqual([]);
  });

  it("draws nothing for a window that did not parse", () => {
    expect(bandsOnDay(win("weekdays", "nope", "18:00"), 0)).toEqual([]);
  });
});

describe("span merging", () => {
  it("coalesces overlapping spans", () => {
    expect(
      mergeSpans([
        { start: 540, end: 720 },
        { start: 600, end: 780 },
      ]),
    ).toEqual([{ start: 540, end: 780 }]);
  });

  it("coalesces touching spans", () => {
    expect(
      mergeSpans([
        { start: 0, end: 60 },
        { start: 60, end: 120 },
      ]),
    ).toEqual([{ start: 0, end: 120 }]);
  });

  it("leaves disjoint spans alone", () => {
    const spans = [
      { start: 0, end: 60 },
      { start: 120, end: 180 },
    ];
    expect(mergeSpans(spans)).toEqual(spans);
  });
});

describe("mergeable windows", () => {
  it("groups windows that share a start and end", () => {
    const windows = [
      parseWindow({ days: ["mon"], start: "16:00", end: "18:00" }, 0),
      parseWindow({ days: ["tue"], start: "16:00", end: "18:00" }, 1),
      parseWindow({ days: ["wed"], start: "09:00", end: "10:00" }, 2),
    ];
    expect(mergeableGroups(windows)).toEqual([[0, 1]]);
  });

  it("finds nothing to merge when the times differ", () => {
    const windows = [
      parseWindow({ days: ["mon"], start: "16:00", end: "18:00" }, 0),
      parseWindow({ days: ["tue"], start: "16:00", end: "19:00" }, 1),
    ];
    expect(mergeableGroups(windows)).toEqual([]);
  });
});

describe("labels", () => {
  it("names the presets", () => {
    expect(describeDays(ALL_DAYS)).toBe("Every day");
    expect(describeDays(WEEKDAYS)).toBe("Weekdays");
    expect(describeDays(WEEKENDS)).toBe("Weekends");
  });

  it("lists explicit days", () => {
    expect(describeDays(0b101)).toBe("Mon, Wed");
  });

  it("says so when a mask covers nothing", () => {
    expect(describeDays(0)).toBe("No days");
  });
});

describe("toggleDay", () => {
  it("adds and removes a day", () => {
    expect(toggleDay(0, 2)).toBe(0b100);
    expect(toggleDay(0b100, 2)).toBe(0);
  });
});

/**
 * The editor's half of the time and day format contract.
 *
 * `parseDays` and `parseTime` above re-implement `lunchbox_config`'s, because
 * the grid parses on every drag frame and cannot round-trip through wasm to do
 * it. Nothing connected the two, and they had drifted — see the fixture's own
 * header for what that cost. `crates/lunchbox-config-wasm/tests/time_formats.rs`
 * asserts the daemon's side of this same file.
 */
describe("the format contract with the daemon", () => {
  // Every assertion below is a loop, so an empty or unreadable fixture would
  // pass all of them without checking anything.
  it("actually has cases to check", () => {
    expect(fixture.times.length).toBeGreaterThan(0);
    expect(fixture.day_presets.length).toBeGreaterThan(0);
    expect(fixture.day_lists.length).toBeGreaterThan(0);
  });

  it("parses every time the fixture lists the way the daemon does", () => {
    for (const c of fixture.times) {
      expect(parseTime(c.input), `parseTime(${JSON.stringify(c.input)})`).toBe(
        c.minutes,
      );
    }
  });

  it("parses every day preset the way the daemon does", () => {
    for (const c of fixture.day_presets) {
      expect(parseDays(c.input), `parseDays(${JSON.stringify(c.input)})`).toBe(
        c.mask,
      );
    }
  });

  it("parses every day list the way the daemon does", () => {
    for (const c of fixture.day_lists) {
      expect(parseDays(c.input), `parseDays(${JSON.stringify(c.input)})`).toBe(
        c.mask,
      );
    }
  });
});
