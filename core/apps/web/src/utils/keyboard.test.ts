import { describe, expect, it } from "vitest";
import { shouldSendOnEnter } from "./keyboard";

describe("shouldSendOnEnter", () => {
  it("sends on Enter", () => {
    expect(shouldSendOnEnter({ key: "Enter", shiftKey: false })).toBe(true);
  });

  it("does not send on Shift+Enter", () => {
    expect(shouldSendOnEnter({ key: "Enter", shiftKey: true })).toBe(false);
  });

  it("does not send while composing", () => {
    expect(shouldSendOnEnter({ key: "Enter", shiftKey: false, isComposing: true })).toBe(false);
  });
});
