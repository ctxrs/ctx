import { describe, expect, it } from "vitest";
import { DISABLE_BROWSER_TEXT_ASSISTS } from "./browserTextInputBehavior";

describe("DISABLE_BROWSER_TEXT_ASSISTS", () => {
  it("opts prompt and search fields out of browser autocomplete and correction", () => {
    expect(DISABLE_BROWSER_TEXT_ASSISTS).toEqual({
      autoComplete: "off",
      autoCorrect: "off",
      autoCapitalize: "none",
      spellCheck: false,
    });
  });
});
