import { describe, expect, it } from "vitest";
import { formatDuration, formatDurationHuman, parseDurationHuman } from "./duration";

describe("formatDurationHuman", () => {
  it("reads as a budget, not a stopwatch", () => {
    expect(formatDurationHuman(5400)).toBe("1h 30m");
    expect(formatDurationHuman(3600)).toBe("1h");
    expect(formatDurationHuman(1800)).toBe("30m");
  });

  it("has something to say about zero", () => {
    expect(formatDurationHuman(0)).toBe("0 min");
    expect(formatDurationHuman(-5)).toBe("0 min");
  });
});

describe("formatDuration", () => {
  it("counts down like a clock", () => {
    expect(formatDuration(90)).toBe("1:30");
    expect(formatDuration(3661)).toBe("1:01:01");
  });
});

describe("parseDurationHuman", () => {
  it("reads what formatDurationHuman writes", () => {
    expect(parseDurationHuman("1h 30m")).toBe(5400);
    expect(parseDurationHuman("1h")).toBe(3600);
    expect(parseDurationHuman("30m")).toBe(1800);
  });

  it("reads the shapes people actually type", () => {
    expect(parseDurationHuman("90")).toBe(5400); // bare number means minutes
    expect(parseDurationHuman("1:30")).toBe(5400);
    expect(parseDurationHuman("1h30m")).toBe(5400);
    expect(parseDurationHuman("2 hours")).toBe(7200);
    expect(parseDurationHuman("45s")).toBe(45);
  });

  it("returns null rather than snapping a half-typed field to zero", () => {
    expect(parseDurationHuman("")).toBeNull();
    expect(parseDurationHuman("h")).toBeNull();
    expect(parseDurationHuman("abc")).toBeNull();
  });
});
