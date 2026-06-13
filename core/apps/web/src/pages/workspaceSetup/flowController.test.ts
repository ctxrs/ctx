import { describe, expect, it } from "vitest";
import {
  clampStepKey,
  isCurrentFlowRunToken,
  nextFlowRunToken,
  stepKeyOffset,
} from "./flowController";

describe("flowController", () => {
  it("clamps to first step when current key is no longer present", () => {
    expect(
      clampStepKey(
        ["location", "auth-import", "container"],
        "session-titling",
      ),
    ).toBe("location");
  });

  it("moves by relative offset and clamps bounds", () => {
    const keys = ["location", "auth-import", "session-titling", "container"];
    expect(stepKeyOffset(keys, "location", 1)).toBe("auth-import");
    expect(stepKeyOffset(keys, "container", 1)).toBe("container");
    expect(stepKeyOffset(keys, "location", -1)).toBe("location");
  });

  it("marks run token current only on exact run and target match", () => {
    const first = nextFlowRunToken(0, "local");
    const second = nextFlowRunToken(first.runId, "local");
    expect(isCurrentFlowRunToken(first, first)).toBe(true);
    expect(isCurrentFlowRunToken(first, second)).toBe(false);
    expect(isCurrentFlowRunToken({ ...second, targetKey: "ssh:user@host" }, second)).toBe(false);
  });
});
