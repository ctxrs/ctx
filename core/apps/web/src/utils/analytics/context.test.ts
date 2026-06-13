import { describe, expect, it } from "vitest";
import { buildEventEnvelope } from "./context";

describe("buildEventEnvelope", () => {
  it("emits the required baseline envelope fields", () => {
    const envelope = buildEventEnvelope(1, { provider_id: "codex" });

    expect(envelope.event_version).toBe(1);
    expect(typeof envelope.occurred_at).toBe("string");
    expect(typeof envelope.app_version).toBe("string");
    expect(typeof envelope.os).toBe("string");
    expect(typeof envelope.arch).toBe("string");
    expect(typeof envelope.surface).toBe("string");
    expect(typeof envelope.analytics_environment).toBe("string");
    expect(envelope.traffic_class).toBe("user");
    expect(envelope.provider_id).toBe("codex");
  });
});
