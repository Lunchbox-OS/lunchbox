import { describe, expect, it } from "vitest";
import type { RawConfig } from "./config.generated";
import { GROUP_PREFIX, tokenSources } from "./tokenSources";

/**
 * Two categories with a member each, plus one ungrouped activity.
 *
 * `school` deliberately has two members so "excludes its members" is testing
 * more than "excludes the one member".
 */
const CONFIG: RawConfig = {
  config_version: 1,
  groups: [
    { id: "games", label: "Games" },
    { id: "school", label: "Schoolwork" },
  ],
  entries: [
    { id: "celeste", label: "Celeste", group: "games", kind: { type: "process", command: "/x" } },
    { id: "tuxmath", label: "Tux Math", group: "school", kind: { type: "process", command: "/x" } },
    { id: "typing", label: "Typing", group: "school", kind: { type: "process", command: "/x" } },
    { id: "loose", label: "Loose", kind: { type: "process", command: "/x" } },
  ],
};

const valuesFor = (subject: Parameters<typeof tokenSources>[1]) =>
  tokenSources(CONFIG, subject).map((o) => o.value);

describe("token sources for an activity's gate", () => {
  const subject = { kind: "entry", id: "celeste" } as const;

  it("does not offer the activity itself", () => {
    expect(valuesFor(subject)).not.toContain("celeste");
  });

  it("does not offer the category it belongs to", () => {
    // Its own time counts toward that category's total, so this would let it
    // unlock itself — `validate_tokens` rejects it.
    expect(valuesFor(subject)).not.toContain(`${GROUP_PREFIX}games`);
  });

  it("offers other categories and every other activity", () => {
    expect(valuesFor(subject)).toEqual([
      `${GROUP_PREFIX}school`,
      "tuxmath",
      "typing",
      "loose",
    ]);
  });

  it("offers every category to an activity with no category of its own", () => {
    expect(valuesFor({ kind: "entry", id: "loose" })).toEqual([
      `${GROUP_PREFIX}games`,
      `${GROUP_PREFIX}school`,
      "celeste",
      "tuxmath",
      "typing",
    ]);
  });
});

describe("token sources for a category's gate", () => {
  const subject = { kind: "group", id: "school" } as const;

  it("does not offer the category itself", () => {
    expect(valuesFor(subject)).not.toContain(`${GROUP_PREFIX}school`);
  });

  it("does not offer any of its members", () => {
    const values = valuesFor(subject);
    expect(values).not.toContain("tuxmath");
    expect(values).not.toContain("typing");
  });

  it("offers other categories and non-members", () => {
    expect(valuesFor(subject)).toEqual([`${GROUP_PREFIX}games`, "celeste", "loose"]);
  });
});

describe("ordering", () => {
  it("puts categories first, which is what MUI's groupBy requires", () => {
    // It groups consecutive runs rather than sorting, so an interleaved list
    // would render several headings of the same name.
    const kinds = tokenSources(CONFIG, { kind: "entry", id: "loose" }).map((o) => o.kind);
    expect(kinds.indexOf("entry")).toBeGreaterThan(kinds.lastIndexOf("group"));
  });

  it("labels categories by their own label, since the heading says what they are", () => {
    const games = tokenSources(CONFIG, { kind: "entry", id: "loose" }).find(
      (o) => o.value === `${GROUP_PREFIX}games`,
    );
    expect(games?.label).toBe("Games");
  });
});
