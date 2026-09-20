import { describe, expect, it } from "vitest";

import { KEEP_ALIVE_CAP, insertMru, pruneKept, renderOrder, sameIds } from "./keepAliveSet";

describe("insertMru", () => {
  it("puts a new id at the front", () => {
    expect(insertMru(["b", "c"], "a")).toEqual(["a", "b", "c"]);
  });

  it("promotes an id that is already kept instead of duplicating it", () => {
    expect(insertMru(["b", "c", "a"], "a")).toEqual(["a", "b", "c"]);
  });

  it("evicts the least recently visited id past the cap", () => {
    const full = ["s8", "s7", "s6", "s5", "s4", "s3", "s2", "s1"];
    expect(full).toHaveLength(KEEP_ALIVE_CAP);
    expect(insertMru(full, "s9")).toEqual(["s9", "s8", "s7", "s6", "s5", "s4", "s3", "s2"]);
  });

  it("honours a caller-supplied cap", () => {
    expect(insertMru(["b", "c"], "a", 2)).toEqual(["a", "b"]);
  });
});

describe("pruneKept", () => {
  it("drops ids that are no longer eligible", () => {
    expect(pruneKept(["a", "b", "c"], new Set(["a", "c"]), "a")).toEqual(["a", "c"]);
  });

  it("keeps the visible id even when it is not eligible", () => {
    // A trashed session stays readable while it is the one on screen; it
    // leaves the set on the next switch, when it is no longer `keep`.
    expect(pruneKept(["a", "b"], new Set(["b"]), "a")).toEqual(["a", "b"]);
  });

  it("drops everything when nothing is eligible and nothing is visible", () => {
    expect(pruneKept(["a", "b"], new Set(), null)).toEqual([]);
  });
});

describe("renderOrder", () => {
  it("is stable under promotion so a switch does not move DOM nodes", () => {
    expect(renderOrder(["b", "a", "c"])).toEqual(renderOrder(["a", "c", "b"]));
    expect(renderOrder(["b", "a", "c"])).toEqual(["a", "b", "c"]);
  });

  it("does not mutate its input", () => {
    const kept = ["b", "a"];
    renderOrder(kept);
    expect(kept).toEqual(["b", "a"]);
  });
});

describe("sameIds", () => {
  it("compares element-wise", () => {
    expect(sameIds(["a", "b"], ["a", "b"])).toBe(true);
    expect(sameIds(["a", "b"], ["b", "a"])).toBe(false);
    expect(sameIds(["a"], ["a", "b"])).toBe(false);
  });
});
