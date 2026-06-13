import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../../utils/desktop", () => ({
  desktopGetViewGeometry: vi.fn(async () => ({
    windowInnerPosition: { x: 0, y: 66 },
    windowOuterPosition: { x: 0, y: 33 },
    webviewPosition: { x: 0, y: 0 },
    scaleFactor: 2,
  })),
}));

import { measureWorkbenchDiffFile } from "./workbenchE2EMeasurements";

describe("measureWorkbenchDiffFile", () => {
  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("targets the chevron button instead of the file header center", async () => {
    document.body.innerHTML = `
      <div class="cursor-diff-file">
        <div class="cursor-diff-file-header">
          <button type="button" class="cursor-diff-chevron" aria-label="Expand file diff"></button>
          <span class="cursor-diff-file-path" title="src/hn_enhancer.js">src/hn_enhancer.js</span>
        </div>
      </div>
    `;

    const chevron = document.querySelector(".cursor-diff-chevron");
    const path = document.querySelector(".cursor-diff-file-path");
    const header = document.querySelector(".cursor-diff-file-header");
    if (!(chevron instanceof HTMLElement) || !(path instanceof HTMLElement) || !(header instanceof HTMLElement)) {
      throw new Error("diff file fixture did not render");
    }

    const scrollIntoView = vi.fn();
    Object.defineProperty(chevron, "scrollIntoView", {
      value: scrollIntoView,
      configurable: true,
    });
    chevron.getBoundingClientRect = () =>
      ({
        left: 24,
        top: 180,
        width: 18,
        height: 18,
      }) as DOMRect;
    header.getBoundingClientRect = () =>
      ({
        left: 12,
        top: 168,
        width: 320,
        height: 42,
      }) as DOMRect;

    const result = await measureWorkbenchDiffFile("src/hn_enhancer.js");
    expect(result).not.toBeNull();
    expect(result?.rect).toEqual({
      left: 24,
      top: 180,
      width: 18,
      height: 18,
    });
    expect(result?.text).toBe("src/hn_enhancer.js");
    expect(scrollIntoView).toHaveBeenCalledWith({ block: "center", inline: "nearest" });
  });
});
