// @vitest-environment jsdom

import { describe, expect, it } from "vitest";
import { sanitizeTaskListStyle } from "./WorkbenchPage.taskList";

describe("sanitizeTaskListStyle", () => {
  it("replaces non-finite list spacing values before they reach the DOM", () => {
    expect(
      sanitizeTaskListStyle({
        boxSizing: "border-box",
        marginTop: Number.NaN,
        paddingTop: Number.NaN,
        paddingBottom: Number.NaN,
      }),
    ).toEqual({
      boxSizing: "border-box",
      marginTop: 0,
      paddingTop: 0,
      paddingBottom: 0,
    });
  });

  it("preserves finite numeric and string spacing values", () => {
    expect(
      sanitizeTaskListStyle({
        marginTop: 4,
        paddingTop: "6px",
        paddingBottom: 8,
      }),
    ).toEqual({
      marginTop: 4,
      paddingTop: "6px",
      paddingBottom: 8,
    });
  });
});
