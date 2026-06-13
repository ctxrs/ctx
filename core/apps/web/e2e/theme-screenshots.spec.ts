import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";
import {
  buildVisualName,
  captureVisual,
  prepareVisualPage,
  visualViewportLabel,
  type VisualTheme,
} from "./utils/visual";

test.describe.serial("theme screenshots", () => {
  let workspaceId = "";

  test.beforeAll(async ({ request }) => {
    const seed = await seedDummyWorkspace(request, {
      tasks: 0,
      sessionsPerTask: 0,
      turnsPerSession: 0,
    });
    workspaceId = seed.workspaceId;
  });

  for (const theme of ["dark", "light"] as const satisfies VisualTheme[]) {
    test(`${theme} theme`, async ({ page }) => {
      await prepareVisualPage(page, {
        theme,
        viewport: "fullpage",
        route: `/settings?ws=${workspaceId}`,
        ready: page.getByText("Theme", { exact: true }),
      });
      await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
      await captureVisual(
        page,
        buildVisualName(["settings", "theme", theme, visualViewportLabel("fullpage")]),
      );
    });
  }
});
