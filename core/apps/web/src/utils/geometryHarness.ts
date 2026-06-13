import type { DesktopViewGeometry } from "./desktop";

export type GeometryHarnessRect = {
  left: number;
  top: number;
  width: number;
  height: number;
  right: number;
  bottom: number;
  centerX: number;
  centerY: number;
};

export type GeometryHarnessBrowserMetrics = {
  screenX: number;
  screenY: number;
  outerWidth: number;
  outerHeight: number;
  availWidth: number;
  availHeight: number;
};

export type GeometryHarnessMetrics = DesktopViewGeometry & GeometryHarnessBrowserMetrics;

export type GeometryHarnessCandidatePoint = {
  id: string;
  description: string;
  topLeftX: number;
  topLeftY: number;
  x: number;
  y: number;
};

export type GeometryHarnessTargetMeasurement = {
  id: string;
  label: string;
  rect: GeometryHarnessRect;
};

export function normalizeGeometryHarnessRect(
  rect: Pick<DOMRectReadOnly, "left" | "top" | "width" | "height" | "right" | "bottom">,
): GeometryHarnessRect {
  const left = Number(rect.left);
  const top = Number(rect.top);
  const width = Number(rect.width);
  const height = Number(rect.height);
  return {
    left,
    top,
    width,
    height,
    right: Number(rect.right),
    bottom: Number(rect.bottom),
    centerX: Number((left + width / 2).toFixed(2)),
    centerY: Number((top + height / 2).toFixed(2)),
  };
}

function quantize(value: number): number {
  return Number(value.toFixed(2));
}

function logicalScale(metrics: GeometryHarnessMetrics): number {
  if (Number.isFinite(metrics.scaleFactor) && metrics.scaleFactor > 0) {
    return metrics.scaleFactor;
  }
  if (Number.isFinite(metrics.devicePixelRatio) && metrics.devicePixelRatio > 0) {
    return metrics.devicePixelRatio;
  }
  return 1;
}

function toLogicalPoint(
  point: { x: number; y: number },
  scaling: "raw" | "logicalized",
  metrics: GeometryHarnessMetrics,
): { x: number; y: number } {
  if (scaling === "raw") {
    return { x: quantize(point.x), y: quantize(point.y) };
  }
  const scale = logicalScale(metrics);
  return {
    x: quantize(point.x / scale),
    y: quantize(point.y / scale),
  };
}

function toQuartzPoint(topLeftX: number, topLeftY: number, screenHeight: number): { x: number; y: number } {
  return {
    x: quantize(topLeftX),
    y: quantize(screenHeight - topLeftY),
  };
}

function toTopBasedPoint(topLeftX: number, topLeftY: number): { x: number; y: number } {
  return {
    x: quantize(topLeftX),
    y: quantize(topLeftY),
  };
}

export function buildGeometryHarnessCandidatePoints(
  metrics: GeometryHarnessMetrics,
  rect: GeometryHarnessRect,
): GeometryHarnessCandidatePoint[] {
  const center = {
    x: rect.centerX,
    y: rect.centerY,
  };
  const scaleModes: Array<"raw" | "logicalized"> = ["raw", "logicalized"];
  const baseOrigins = [
    {
      id: "webview",
      description: "webview.position()",
      point: metrics.webviewPosition,
    },
    {
      id: "window_inner",
      description: "window.innerPosition()",
      point: metrics.windowInnerPosition,
    },
    {
      id: "window_outer",
      description: "window.outerPosition()",
      point: metrics.windowOuterPosition,
    },
    {
      id: "window_inner_plus_webview",
      description: "window.innerPosition() + webview.position()",
      point: {
        x: metrics.windowInnerPosition.x + metrics.webviewPosition.x,
        y: metrics.windowInnerPosition.y + metrics.webviewPosition.y,
      },
    },
    {
      id: "window_outer_plus_webview",
      description: "window.outerPosition() + webview.position()",
      point: {
        x: metrics.windowOuterPosition.x + metrics.webviewPosition.x,
        y: metrics.windowOuterPosition.y + metrics.webviewPosition.y,
      },
    },
  ];

  const candidates: GeometryHarnessCandidatePoint[] = scaleModes.flatMap((scaling) =>
    baseOrigins.flatMap((origin) => {
      const logicalOrigin = toLogicalPoint(origin.point, scaling, metrics);
      const topLeftX = quantize(logicalOrigin.x + center.x);
      const topLeftY = quantize(logicalOrigin.y + center.y);
      const quartzPoint = toQuartzPoint(topLeftX, topLeftY, metrics.screenHeight);
      const topBasedPoint = toTopBasedPoint(topLeftX, topLeftY);
      return [
        {
          id: `${origin.id}_${scaling}`,
          description: `${origin.description} (${scaling}, inverted-y)`,
          topLeftX,
          topLeftY,
          x: quartzPoint.x,
          y: quartzPoint.y,
        },
        {
          id: `${origin.id}_${scaling}_top_based`,
          description: `${origin.description} (${scaling}, top-based y)`,
          topLeftX,
          topLeftY,
          x: topBasedPoint.x,
          y: topBasedPoint.y,
        },
      ];
    }),
  );

  const chromeTop = Math.max(0, Number(metrics.outerHeight) - Number(metrics.innerHeight));
  const legacyX = quantize(Number(metrics.screenX) + center.x);
  const legacyY = quantize(Number(metrics.screenY) - chromeTop - center.y);
  candidates.unshift({
    id: "legacy_screen_metrics",
    description: "window.screenX/screenY with outerHeight-innerHeight chrome offset",
    topLeftX: legacyX,
    topLeftY: quantize(metrics.screenHeight - legacyY),
    x: legacyX,
    y: legacyY,
  });
  candidates.splice(1, 0, {
    id: "legacy_screen_metrics_top_based",
    description: "window.screenX plus top-based client y",
    topLeftX: quantize(Number(metrics.screenX) + center.x),
    topLeftY: quantize(center.y),
    x: quantize(Number(metrics.screenX) + center.x),
    y: quantize(center.y),
  });

  return candidates;
}
