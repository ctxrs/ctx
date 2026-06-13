import { describe, expect, it } from "vitest";
import {
  readWorkbenchNavigationTarget,
  stripWorkbenchNavigationTarget,
} from "./workbenchNavigationQuery";

describe("workbenchNavigationQuery", () => {
  it("reads task and session targets from the search string", () => {
    expect(readWorkbenchNavigationTarget("?task=task-1&session=session-1")).toEqual({
      taskId: "task-1",
      sessionId: "session-1",
    });
  });

  it("returns null when no task target exists", () => {
    expect(readWorkbenchNavigationTarget("?debug=1")).toBeNull();
  });

  it("strips only navigation params and keeps unrelated search params", () => {
    expect(stripWorkbenchNavigationTarget("?task=task-1&session=session-1&debug=1")).toBe("debug=1");
  });
});
