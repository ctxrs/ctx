import { describe, expect, it } from "vitest";
import { errorMessage } from "./errorMessage";

describe("errorMessage", () => {
  it("returns object message fields when available", () => {
    expect(errorMessage({ message: "boom" })).toBe("boom");
    expect(errorMessage({ error: "bad request" })).toBe("bad request");
  });

  it("always returns a string for stringify edge values", () => {
    expect(errorMessage(undefined)).toBe("undefined");
    expect(typeof errorMessage(() => "x")).toBe("string");
  });
});
