import { describe, expect, it, vi } from "vitest";

import { createInternalEntry } from "./entryState";
import { adoptLoadedStateRevision, syncSupportLoadsForOpenSession } from "./supportLoads";

describe("syncSupportLoadsForOpenSession", () => {
  it("autoloads state for open sessions even before an authoritative state revision is known", () => {
    const entry = createInternalEntry("session-1", {
      transientSeqStart: -1,
      warmTtlMs: 1_000,
    });
    entry.refCount = 1;
    entry.mode = "active";
    entry.freshness = "replica";

    const ensureState = vi.fn(async () => {});
    const ensureSubagentInvocations = vi.fn(async () => {});
    const resolveRequestedStateRev = vi.fn(() => undefined);

    syncSupportLoadsForOpenSession(entry, {
      resolveRequestedStateRev,
      ensureState,
      ensureSubagentInvocations,
    });

    expect(resolveRequestedStateRev).toHaveBeenCalledWith(entry);
    expect(entry.support.stateAutoLoadKey).toBe("epoch:0");
    expect(entry.support.subagentAutoLoadKey).toBe("epoch:0");
    expect(ensureState).toHaveBeenCalledTimes(1);
    expect(ensureState).toHaveBeenCalledWith(entry);
    expect(ensureSubagentInvocations).toHaveBeenCalledTimes(1);
    expect(ensureSubagentInvocations).toHaveBeenCalledWith(entry);

    syncSupportLoadsForOpenSession(entry, {
      resolveRequestedStateRev,
      ensureState,
      ensureSubagentInvocations,
    });

    expect(ensureState).toHaveBeenCalledTimes(1);
    expect(ensureSubagentInvocations).toHaveBeenCalledTimes(1);
  });

  it("refetches revisionless support once an authoritative revision becomes known", () => {
    const entry = createInternalEntry("session-1", {
      transientSeqStart: -1,
      warmTtlMs: 1_000,
    });
    entry.refCount = 1;
    entry.mode = "active";
    entry.freshness = "authoritative";
    entry.support.stateLoaded = true;
    entry.support.subagentInvocationsLoaded = true;
    entry.support.stateAutoLoadKey = "epoch:0";
    entry.support.subagentAutoLoadKey = "epoch:0";

    const ensureState = vi.fn(async () => {});
    const ensureSubagentInvocations = vi.fn(async () => {});
    const resolveRequestedStateRev = vi.fn(() => 7);

    syncSupportLoadsForOpenSession(entry, {
      resolveRequestedStateRev,
      ensureState,
      ensureSubagentInvocations,
    });

    expect(entry.support.stateAutoLoadKey).toBe("rev:7");
    expect(entry.support.subagentAutoLoadKey).toBe("rev:7");
    expect(ensureState).toHaveBeenCalledTimes(1);
    expect(ensureSubagentInvocations).toHaveBeenCalledTimes(1);
  });
});

describe("adoptLoadedStateRevision", () => {
  it("does not retroactively stamp revisionless support as current", () => {
    expect(adoptLoadedStateRevision(true, undefined, 7)).toBeUndefined();
    expect(adoptLoadedStateRevision(true, 5, 7)).toBe(5);
  });
});
