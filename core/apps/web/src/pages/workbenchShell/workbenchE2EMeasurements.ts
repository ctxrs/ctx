import { desktopGetViewGeometry, type DesktopViewGeometry } from "../../utils/desktop";

export type WorkbenchE2ERect = {
  left: number;
  top: number;
  width: number;
  height: number;
};

export type WorkbenchE2EElementMeasurement = {
  selector?: string | null;
  label?: string | null;
  text: string;
  rect: WorkbenchE2ERect;
};

export type WorkbenchE2EMetrics = DesktopViewGeometry & {
  screenX: number;
  screenY: number;
  outerWidth: number;
  outerHeight: number;
  availWidth: number;
  availHeight: number;
  pathname: string;
};

export type WorkbenchE2EMeasureTargetsResult = {
  metrics: WorkbenchE2EMetrics;
  elements: Record<string, WorkbenchE2EElementMeasurement | null>;
};

export type WorkbenchE2EMeasuredTargetResult = {
  metrics: WorkbenchE2EMetrics;
  rect: WorkbenchE2ERect;
  selector?: string | null;
  label?: string | null;
  text: string;
};

function rectPayload(element: Element): WorkbenchE2ERect {
  const rect = element.getBoundingClientRect();
  return {
    left: rect.left,
    top: rect.top,
    width: rect.width,
    height: rect.height,
  };
}

async function collectMetrics(): Promise<WorkbenchE2EMetrics> {
  const geometry = await desktopGetViewGeometry();
  return {
    ...geometry,
    screenX: window.screenX,
    screenY: window.screenY,
    outerWidth: window.outerWidth,
    outerHeight: window.outerHeight,
    availWidth: window.screen.availWidth,
    availHeight: window.screen.availHeight,
    pathname: window.location.pathname,
  };
}

export async function measureWorkbenchTargets(
  selectors: Record<string, string>,
): Promise<WorkbenchE2EMeasureTargetsResult> {
  const metrics = await collectMetrics();
  const elements = Object.fromEntries(
    Object.entries(selectors).map(([key, selector]) => {
      const element = document.querySelector(selector);
      if (!(element instanceof HTMLElement)) {
        return [key, null];
      }
      return [
        key,
        {
          selector,
          rect: rectPayload(element),
          text: (element.textContent || "").trim(),
        } satisfies WorkbenchE2EElementMeasurement,
      ];
    }),
  );
  return {
    metrics,
    elements,
  };
}

export async function measureWorkbenchHarnessOption(
  label: string,
): Promise<WorkbenchE2EMeasuredTargetResult | null> {
  const target = Array.from(document.querySelectorAll(".wb-harness-row-main")).find((element) =>
    element.textContent?.includes(label));
  if (!(target instanceof HTMLElement)) {
    return null;
  }
  target.scrollIntoView({ block: "center", inline: "nearest" });
  return {
    metrics: await collectMetrics(),
    rect: rectPayload(target),
    label,
    text: target.textContent?.trim() ?? "",
  };
}

export async function measureWorkbenchDiffFile(
  targetPath: string,
): Promise<WorkbenchE2EMeasuredTargetResult | null> {
  const diffFile = Array.from(document.querySelectorAll(".cursor-diff-file")).find((element) => {
    const filePathElement = element.querySelector(".cursor-diff-file-path");
    return filePathElement?.textContent?.trim() === targetPath;
  });
  if (!(diffFile instanceof HTMLElement)) {
    return null;
  }
  const chevron = diffFile.querySelector(".cursor-diff-chevron");
  if (!(chevron instanceof HTMLElement)) {
    return null;
  }
  const filePathElement = diffFile.querySelector(".cursor-diff-file-path");
  const text = filePathElement instanceof HTMLElement ? filePathElement.textContent?.trim() ?? "" : diffFile.textContent?.trim() ?? "";
  const target = chevron;
  target.scrollIntoView({ block: "center", inline: "nearest" });
  return {
    metrics: await collectMetrics(),
    rect: rectPayload(target),
    label: targetPath,
    text,
  };
}
