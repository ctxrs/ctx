import { useEffect, useMemo } from "react";
import { useSearchParams } from "react-router-dom";
import { buildGeometryHarnessCandidatePoints, normalizeGeometryHarnessRect, type GeometryHarnessCandidatePoint, type GeometryHarnessMetrics, type GeometryHarnessRect, type GeometryHarnessTargetMeasurement } from "../utils/geometryHarness";
import { desktopGetViewGeometry } from "../utils/desktop";

type GeometryHarnessTargetSpec = {
  id: string;
  label: string;
  left: string;
  top: string;
};

type GeometryHarnessProbeEvent = {
  type: "mousedown" | "mouseup" | "click";
  clientX: number;
  clientY: number;
  targetId: string | null;
  nearestTargetId: string | null;
  targetTag: string;
  targetText: string;
  ts: number;
};

type GeometryHarnessWindow = Window & {
  __ctxGeometryHarness?: {
    snapshot: () => Promise<{
      metrics: GeometryHarnessMetrics;
      targets: GeometryHarnessTargetMeasurement[];
      probe: GeometryHarnessProbeState;
    }>;
    measureTarget: (targetId: string) => Promise<{
      metrics: GeometryHarnessMetrics;
      target: GeometryHarnessTargetMeasurement;
      candidatePoints: GeometryHarnessCandidatePoint[];
    }>;
    resetProbe: () => boolean;
    readProbe: () => GeometryHarnessProbeState;
  };
};

type GeometryHarnessProbeState = {
  events: GeometryHarnessProbeEvent[];
  lastEvent: GeometryHarnessProbeEvent | null;
};

const TARGET_SPECS: GeometryHarnessTargetSpec[] = [
  { id: "top-left", label: "Top Left", left: "10%", top: "10%" },
  { id: "top-center", label: "Top Center", left: "50%", top: "10%" },
  { id: "top-right", label: "Top Right", left: "90%", top: "10%" },
  { id: "middle-left", label: "Middle Left", left: "10%", top: "50%" },
  { id: "center", label: "Center", left: "50%", top: "50%" },
  { id: "middle-right", label: "Middle Right", left: "90%", top: "50%" },
  { id: "bottom-left", label: "Bottom Left", left: "10%", top: "90%" },
  { id: "bottom-center", label: "Bottom Center", left: "50%", top: "90%" },
  { id: "bottom-right", label: "Bottom Right", left: "90%", top: "90%" },
];

function findTargetElement(targetId: string): HTMLButtonElement {
  const target = document.querySelector(`[data-geometry-target-id="${targetId}"]`);
  if (!(target instanceof HTMLButtonElement)) {
    throw new Error(`geometry harness target not found: ${targetId}`);
  }
  return target;
}

function measureTarget(targetId: string): GeometryHarnessTargetMeasurement {
  const element = findTargetElement(targetId);
  const rect = normalizeGeometryHarnessRect(element.getBoundingClientRect());
  return {
    id: targetId,
    label: element.textContent?.trim() ?? targetId,
    rect,
  };
}

function listTargets(): GeometryHarnessTargetMeasurement[] {
  return Array.from(document.querySelectorAll("[data-geometry-target-id]")).flatMap((node) => {
    if (!(node instanceof HTMLButtonElement)) {
      return [];
    }
    return [{
      id: String(node.dataset.geometryTargetId || "").trim(),
      label: node.textContent?.trim() ?? "",
      rect: normalizeGeometryHarnessRect(node.getBoundingClientRect()),
    }];
  });
}

async function collectHarnessMetrics(): Promise<GeometryHarnessMetrics> {
  const desktopMetrics = await desktopGetViewGeometry();
  return {
    ...desktopMetrics,
    screenX: window.screenX,
    screenY: window.screenY,
    outerWidth: window.outerWidth,
    outerHeight: window.outerHeight,
    availWidth: window.screen.availWidth,
    availHeight: window.screen.availHeight,
  };
}

function distanceToRectCenter(rect: GeometryHarnessRect, clientX: number, clientY: number): number {
  return Math.hypot(rect.centerX - clientX, rect.centerY - clientY);
}

function nearestTargetIdForPoint(clientX: number, clientY: number): string | null {
  const targets = listTargets();
  let best: { id: string; distance: number } | null = null;
  for (const target of targets) {
    const distance = distanceToRectCenter(target.rect, clientX, clientY);
    if (!best || distance < best.distance) {
      best = { id: target.id, distance };
    }
  }
  return best?.id ?? null;
}

export default function GeometryHarnessPage() {
  const [params] = useSearchParams();
  const visibleTargets = useMemo(() => {
    const requested = String(params.get("targets") ?? "")
      .split(",")
      .map((value) => value.trim())
      .filter(Boolean);
    if (requested.length === 0) {
      return TARGET_SPECS;
    }
    const allowed = new Set(requested);
    return TARGET_SPECS.filter((target) => allowed.has(target.id));
  }, [params]);

  useEffect(() => {
    const probeState: GeometryHarnessProbeState = {
      events: [],
      lastEvent: null,
    };

    const recordEvent = (event: MouseEvent, type: GeometryHarnessProbeEvent["type"]) => {
      const target =
        event.target instanceof Element
          ? event.target.closest("[data-geometry-target-id]")
          : null;
      const targetId =
        target instanceof HTMLElement
          ? String(target.dataset.geometryTargetId || "").trim() || null
          : null;
      const probeEvent: GeometryHarnessProbeEvent = {
        type,
        clientX: Number(event.clientX.toFixed(2)),
        clientY: Number(event.clientY.toFixed(2)),
        targetId,
        nearestTargetId: nearestTargetIdForPoint(event.clientX, event.clientY),
        targetTag: event.target instanceof HTMLElement ? event.target.tagName : "",
        targetText: event.target instanceof HTMLElement ? (event.target.textContent?.trim() ?? "").slice(0, 120) : "",
        ts: Date.now(),
      };
      probeState.events.push(probeEvent);
      if (probeState.events.length > 24) {
        probeState.events.splice(0, probeState.events.length - 24);
      }
      probeState.lastEvent = probeEvent;
    };

    const onMouseDown = (event: MouseEvent) => recordEvent(event, "mousedown");
    const onMouseUp = (event: MouseEvent) => recordEvent(event, "mouseup");
    const onClick = (event: MouseEvent) => recordEvent(event, "click");

    document.addEventListener("mousedown", onMouseDown, true);
    document.addEventListener("mouseup", onMouseUp, true);
    document.addEventListener("click", onClick, true);

    const win = window as GeometryHarnessWindow;
    win.__ctxGeometryHarness = {
      snapshot: async () => ({
        metrics: await collectHarnessMetrics(),
        targets: listTargets(),
        probe: {
          events: [...probeState.events],
          lastEvent: probeState.lastEvent ? { ...probeState.lastEvent } : null,
        },
      }),
      measureTarget: async (targetId: string) => {
        const target = measureTarget(targetId);
        const metrics = await collectHarnessMetrics();
        return {
          metrics,
          target,
          candidatePoints: buildGeometryHarnessCandidatePoints(metrics, target.rect),
        };
      },
      resetProbe: () => {
        probeState.events = [];
        probeState.lastEvent = null;
        return true;
      },
      readProbe: () => ({
        events: [...probeState.events],
        lastEvent: probeState.lastEvent ? { ...probeState.lastEvent } : null,
      }),
    };

    return () => {
      document.removeEventListener("mousedown", onMouseDown, true);
      document.removeEventListener("mouseup", onMouseUp, true);
      document.removeEventListener("click", onClick, true);
      delete win.__ctxGeometryHarness;
    };
  }, []);

  return (
    <div
      data-testid="geometry-harness-page"
      style={{
        position: "relative",
        width: "100vw",
        height: "100vh",
        overflow: "hidden",
        background:
          "radial-gradient(circle at 20% 20%, rgba(255,255,255,0.09), transparent 25%), linear-gradient(160deg, #0f172a 0%, #111827 48%, #030712 100%)",
      }}
    >
      <div
        style={{
          position: "absolute",
          inset: 0,
          backgroundImage:
            "linear-gradient(rgba(255,255,255,0.04) 1px, transparent 1px), linear-gradient(90deg, rgba(255,255,255,0.04) 1px, transparent 1px)",
          backgroundSize: "80px 80px",
          opacity: 0.5,
          pointerEvents: "none",
        }}
      />
      <div
        aria-hidden="true"
        style={{
          position: "absolute",
          left: 24,
          top: 24,
          display: "flex",
          flexDirection: "column",
          gap: 6,
          color: "#e5e7eb",
          fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
          fontSize: 12,
          pointerEvents: "none",
        }}
      >
        <div>Geometry Harness</div>
        <div>Real mouse target proving only.</div>
      </div>
      {visibleTargets.map((target) => (
        <button
          key={target.id}
          type="button"
          data-geometry-target-id={target.id}
          style={{
            position: "absolute",
            left: target.left,
            top: target.top,
            transform: "translate(-50%, -50%)",
            width: 144,
            height: 52,
            borderRadius: 14,
            border: "1px solid rgba(255,255,255,0.22)",
            background: "rgba(15, 23, 42, 0.84)",
            color: "#f8fafc",
            fontFamily: "\"SF Pro Text\", ui-sans-serif, system-ui, sans-serif",
            fontSize: 16,
            fontWeight: 650,
            letterSpacing: "0.01em",
            boxShadow: "0 18px 44px rgba(0,0,0,0.35)",
          }}
        >
          {target.label}
        </button>
      ))}
    </div>
  );
}
