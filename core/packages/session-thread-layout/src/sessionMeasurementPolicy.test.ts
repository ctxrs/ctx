import { describe, expect, it } from "vitest";
import {
  shouldUseExactRenderedPlainTextMeasurement,
  shouldUseRenderedAssistantRowMeasurement,
  shouldUseRenderedMarkdownMeasurement,
} from "./sessionMeasurementPolicy";

describe("sessionMeasurementPolicy", () => {
  it("routes complex inline-code markdown to rendered measurement", () => {
    const markdown = [
      "- Follow `sessionThreadDomMeasurement.tsx` with `ctx-main #42` before shipping.",
      "",
      "The exact commit should stay aligned with the green integration run.",
    ].join("\n");

    expect(shouldUseRenderedMarkdownMeasurement(markdown)).toBe(true);
  });

  it("routes multiline long-token markdown to rendered measurement", () => {
    const markdown = [
      "short lead",
      "",
      "core/apps/web/src/pages/sessionThread/sessionThreadDomMeasurement.tsx/core/apps/web/src/pages/sessionThread/sessionThreadDomMeasurement.tsx",
    ].join("\n");

    expect(shouldUseRenderedMarkdownMeasurement(markdown)).toBe(true);
  });

  it("keeps ordinary short markdown on the deterministic path", () => {
    expect(shouldUseRenderedMarkdownMeasurement("short prose with `code`")).toBe(false);
  });

  it("routes long multiline plain text to rendered measurement", () => {
    const text = [
      "This line is short.",
      "core/apps/web/src/pages/sessionThread/sessionThreadDomMeasurement.tsx keeps wrapping across the full browser width now.",
      "The next line is also long enough to stay in the risky corpus.",
    ].join("\n");

    expect(shouldUseExactRenderedPlainTextMeasurement(text)).toBe(true);
  });

  it("keeps ordinary plain text on the deterministic path", () => {
    expect(shouldUseExactRenderedPlainTextMeasurement("short plain text")).toBe(false);
  });

  it("routes long inline-code assistant rows to rendered measurement", () => {
    const content =
      "The fix is on `origin/main` now at `9771951d0`, and the integration lane picked it up immediately as `ctx-main #42`. I’m waiting for that exact commit to go green before I continue.";

    expect(shouldUseRenderedAssistantRowMeasurement(content)).toBe(true);
    expect(shouldUseRenderedAssistantRowMeasurement("short answer")).toBe(false);
  });
});
