import { describe, expect, it } from "vitest";
import { parseBoolishString, providerDetailFlag, readBoolish } from "./boolish";

describe("parseBoolishString", () => {
  it("accepts the shared true vocabulary", () => {
    expect(parseBoolishString("1")).toBe(true);
    expect(parseBoolishString(" true ")).toBe(true);
    expect(parseBoolishString("YES")).toBe(true);
    expect(parseBoolishString("On")).toBe(true);
  });

  it("accepts the shared false vocabulary", () => {
    expect(parseBoolishString("0")).toBe(false);
    expect(parseBoolishString(" false ")).toBe(false);
    expect(parseBoolishString("NO")).toBe(false);
    expect(parseBoolishString("Off")).toBe(false);
  });

  it("rejects invalid values", () => {
    expect(parseBoolishString("")).toBeUndefined();
    expect(parseBoolishString("maybe")).toBeUndefined();
  });
});

describe("readBoolish", () => {
  it("preserves typed booleans", () => {
    expect(readBoolish(true)).toBe(true);
    expect(readBoolish(false)).toBe(false);
  });

  it("decodes boolean-like strings", () => {
    expect(readBoolish(" yes ")).toBe(true);
    expect(readBoolish("off")).toBe(false);
  });
});

describe("providerDetailFlag", () => {
  it("treats only parsed true values as enabled", () => {
    expect(providerDetailFlag({ install_running: "true" }, "install_running")).toBe(true);
    expect(providerDetailFlag({ install_running: "1" }, "install_running")).toBe(true);
    expect(providerDetailFlag({ install_running: "false" }, "install_running")).toBe(false);
    expect(providerDetailFlag({ install_running: "maybe" }, "install_running")).toBe(false);
  });
});
