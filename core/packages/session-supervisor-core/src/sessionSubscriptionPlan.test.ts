import { describe, expect, it } from "vitest";
import { buildSessionSubscriptionPlan } from "./sessionSubscriptionPlan";

describe("sessionSubscriptionPlan", () => {
  it("keeps active task sessions first and appends open and warm sessions without duplicates", () => {
    expect(
      buildSessionSubscriptionPlan({
        openSessionIds: ["session-open", "session-open-2"],
        activeTaskSessionIds: ["session-open-2", "session-active"],
        warmSessionIds: ["session-active", "session-warm"],
        previousSubscribedSessionIds: ["session-open"],
      }),
    ).toEqual({
      openSessionIds: ["session-open", "session-open-2"],
      nextSubscribedSessionIds: ["session-open-2", "session-active", "session-open", "session-warm"],
      addedSessionIds: ["session-open-2", "session-active", "session-warm"],
      removedSessionIds: [],
      changed: true,
    });
  });

  it("reports removals and unchanged plans separately", () => {
    expect(
      buildSessionSubscriptionPlan({
        openSessionIds: ["session-open"],
        activeTaskSessionIds: [],
        warmSessionIds: [],
        previousSubscribedSessionIds: ["session-open", "session-warm"],
      }),
    ).toEqual({
      openSessionIds: ["session-open"],
      nextSubscribedSessionIds: ["session-open"],
      addedSessionIds: [],
      removedSessionIds: ["session-warm"],
      changed: true,
    });

    expect(
      buildSessionSubscriptionPlan({
        openSessionIds: ["session-open"],
        activeTaskSessionIds: ["session-active"],
        warmSessionIds: [],
        previousSubscribedSessionIds: ["session-active", "session-open"],
      }).changed,
    ).toBe(false);
  });
});
