import { describe, expect, it } from "vitest";
import { add, isAdult } from "./calc";

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

describe("calc", () => {
  it("adds", () => {
    expect(add(2, 3)).toBe(5);
  });

  it("adds again", () => {
    expect(add(2, 3)).toBe(5);
  });

  it("isAdult", () => {
    expect(isAdult(30)).toBe(true);
  });

  it("slow", async () => {
    await sleep(600);
    expect(add(1, 1)).toBe(2);
  });
});
