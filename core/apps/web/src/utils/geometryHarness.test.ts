import { describe, expect, it } from "vitest";
import { buildGeometryHarnessCandidatePoints, normalizeGeometryHarnessRect, type GeometryHarnessMetrics } from "./geometryHarness";

const metrics: GeometryHarnessMetrics = {
  scaleFactor: 2,
  devicePixelRatio: 2,
  webviewPosition: { x: 40, y: 24 },
  webviewSize: { width: 1440, height: 900 },
  windowInnerPosition: { x: 320, y: 180 },
  windowOuterPosition: { x: 300, y: 140 },
  windowInnerSize: { width: 1440, height: 900 },
  windowOuterSize: { width: 1480, height: 944 },
  screenWidth: 1728,
  screenHeight: 1117,
  innerWidth: 1440,
  innerHeight: 900,
  screenX: 300,
  screenY: 977,
  outerWidth: 1480,
  outerHeight: 944,
  availWidth: 1728,
  availHeight: 1080,
};

describe("geometryHarness", () => {
  it("normalizes target rect centers", () => {
    const rect = normalizeGeometryHarnessRect({
      left: 100,
      top: 50,
      width: 120,
      height: 40,
      right: 220,
      bottom: 90,
    });
    expect(rect.centerX).toBe(160);
    expect(rect.centerY).toBe(70);
  });

  it("builds legacy and tauri-origin candidate points", () => {
    const rect = normalizeGeometryHarnessRect({
      left: 100,
      top: 50,
      width: 120,
      height: 40,
      right: 220,
      bottom: 90,
    });
    const candidates = buildGeometryHarnessCandidatePoints(metrics, rect);
    expect(candidates.map((candidate) => candidate.id)).toContain("legacy_screen_metrics");
    expect(candidates.map((candidate) => candidate.id)).toContain("window_inner_plus_webview_logicalized");
    expect(candidates.map((candidate) => candidate.id)).toContain("window_inner_plus_webview_logicalized_top_based");
    expect(candidates.map((candidate) => candidate.id)).toContain("window_outer_raw");

    const logicalized = candidates.find((candidate) => candidate.id === "window_inner_plus_webview_logicalized");
    expect(logicalized).toEqual({
      id: "window_inner_plus_webview_logicalized",
      description: "window.innerPosition() + webview.position() (logicalized, inverted-y)",
      topLeftX: 340,
      topLeftY: 172,
      x: 340,
      y: 945,
    });

    const topBased = candidates.find((candidate) => candidate.id === "window_inner_plus_webview_logicalized_top_based");
    expect(topBased).toEqual({
      id: "window_inner_plus_webview_logicalized_top_based",
      description: "window.innerPosition() + webview.position() (logicalized, top-based y)",
      topLeftX: 340,
      topLeftY: 172,
      x: 340,
      y: 172,
    });

    const legacy = candidates.find((candidate) => candidate.id === "legacy_screen_metrics");
    expect(legacy).toEqual({
      id: "legacy_screen_metrics",
      description: "window.screenX/screenY with outerHeight-innerHeight chrome offset",
      topLeftX: 460,
      topLeftY: 254,
      x: 460,
      y: 863,
    });
  });
});
