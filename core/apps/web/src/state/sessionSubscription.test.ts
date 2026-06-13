import { describe, expect, it } from "vitest";

import {
  normalizeSessionSubscriptionCursors,
  sameSessionSubscriptionCursors,
} from "./sessionSubscription";

describe("sessionSubscription", () => {
  it("prefers explicit reset over stale resume cursors", () => {
    expect(
      normalizeSessionSubscriptionCursors([
        { sessionId: "session-1", replay: { kind: "resume", afterSeq: 7 } },
        { sessionId: "session-1", replay: { kind: "reset" } },
      ]),
    ).toEqual([{ sessionId: "session-1", intent: "replay", replay: { kind: "reset" } }]);
  });

  it("keeps the largest resume cursor when merging duplicates", () => {
    expect(
      normalizeSessionSubscriptionCursors([
        { sessionId: "session-1", replay: { kind: "resume", afterSeq: 4 } },
        { sessionId: "session-1", replay: { kind: "resume", afterSeq: 9 } },
      ]),
    ).toEqual([
      { sessionId: "session-1", intent: "replay", replay: { kind: "resume", afterSeq: 9 } },
    ]);
  });

  it("keeps the largest projection rev when resume cursors tie on seq", () => {
    expect(
      normalizeSessionSubscriptionCursors([
        { sessionId: "session-1", replay: { kind: "resume", afterSeq: 9, afterProjectionRev: 11 } },
        { sessionId: "session-1", replay: { kind: "resume", afterSeq: 9, afterProjectionRev: 14 } },
      ]),
    ).toEqual([
      { sessionId: "session-1", intent: "replay", replay: { kind: "resume", afterSeq: 9, afterProjectionRev: 14 } },
    ]);
  });

  it("treats auto, reset, and resume as distinct replay intents", () => {
    expect(
      sameSessionSubscriptionCursors(
        [{ sessionId: "session-1", replay: { kind: "auto" } }],
        [{ sessionId: "session-1", replay: { kind: "reset" } }],
      ),
    ).toBe(false);
    expect(
      sameSessionSubscriptionCursors(
        [{ sessionId: "session-1", replay: { kind: "resume", afterSeq: 3 } }],
        [{ sessionId: "session-1", replay: { kind: "resume", afterSeq: 4 } }],
      ),
    ).toBe(false);
    expect(
      sameSessionSubscriptionCursors(
        [{ sessionId: "session-1", replay: { kind: "resume", afterSeq: 4, afterProjectionRev: 5 } }],
        [{ sessionId: "session-1", replay: { kind: "resume", afterSeq: 4, afterProjectionRev: 6 } }],
      ),
    ).toBe(false);
  });
});
