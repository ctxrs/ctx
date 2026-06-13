import { fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi, beforeEach } from "vitest";
import GeometryHarnessPage from "./GeometryHarnessPage";
import { desktopGetViewGeometry } from "../utils/desktop";

type GeometryHarnessWindow = Window & {
  __ctxGeometryHarness?: {
    snapshot: () => Promise<unknown>;
    measureTarget: (targetId: string) => Promise<unknown>;
    resetProbe: () => boolean;
    readProbe: () => {
      lastEvent: {
        type: string;
        targetId: string | null;
        nearestTargetId: string | null;
      } | null;
    };
  };
};

vi.mock("../utils/desktop", () => ({
  desktopGetViewGeometry: vi.fn(async () => ({
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
  })),
}));

function renderPage(route = "/__geometry_harness") {
  return render(
    <MemoryRouter initialEntries={[route]} future={{ v7_startTransition: true, v7_relativeSplatPath: true }}>
      <GeometryHarnessPage />
    </MemoryRouter>,
  );
}

describe("GeometryHarnessPage", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    delete (window as GeometryHarnessWindow).__ctxGeometryHarness;
  });

  it("installs a bridge that measures targets and candidate points", async () => {
    renderPage();
    const harnessWindow = window as GeometryHarnessWindow;

    expect(await screen.findByTestId("geometry-harness-page")).toBeInTheDocument();
    const measurement = await harnessWindow.__ctxGeometryHarness?.measureTarget("center");
    expect(desktopGetViewGeometry).toHaveBeenCalledTimes(1);
    expect(measurement).toMatchObject({
      target: {
        id: "center",
        label: "Center",
      },
    });
    expect(Array.isArray((measurement as { candidatePoints: unknown[] }).candidatePoints)).toBe(true);
    expect((measurement as { candidatePoints: unknown[] }).candidatePoints.length).toBeGreaterThan(4);
  });

  it("records actual click probe events against the nearest target", async () => {
    renderPage("/__geometry_harness?targets=center");
    const harnessWindow = window as GeometryHarnessWindow;
    const button = await screen.findByRole("button", { name: "Center" });
    harnessWindow.__ctxGeometryHarness?.resetProbe();

    fireEvent.mouseDown(button, { clientX: 500, clientY: 320 });
    fireEvent.mouseUp(button, { clientX: 500, clientY: 320 });
    fireEvent.click(button, { clientX: 500, clientY: 320 });

    expect(harnessWindow.__ctxGeometryHarness?.readProbe().lastEvent).toMatchObject({
      type: "click",
      targetId: "center",
      nearestTargetId: "center",
    });
  });
});
