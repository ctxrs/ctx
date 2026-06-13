import { describe, expect, it } from "vitest";

import type { ProviderOptions } from "../../api/client";
import { mergeAcpMetaIntoSharedProviderOptions } from "./acpMeta";

describe("mergeAcpMetaIntoSharedProviderOptions", () => {
  it("preserves an existing discovered catalog when the incoming ACP models payload is partial", () => {
    const existing: ProviderOptions = {
      provider_id: "kimi",
      workspace_id: "ws-1",
      supports_load: false,
      auth_required: false,
      probed_at: "2026-03-12T00:00:00.000Z",
      models: {
        models: [{ id: "kimi-k2.5", name: "Kimi K2.5" }],
        current_model_id: "kimi-k2.5",
        meta: {
          source_kind: "subscription",
          catalog_source: "runtime_probe_live",
          refresh_pending: true,
        },
      },
    };

    const merged = mergeAcpMetaIntoSharedProviderOptions(
      existing,
      {
        models: {
          current_model_id: "kimi-k2.5-thinking",
        },
      },
      "kimi",
      "ws-1",
    );

    expect(merged?.models).toMatchObject({
      models: [{ id: "kimi-k2.5", name: "Kimi K2.5" }],
      current_model_id: "kimi-k2.5",
      meta: {
        source_kind: "subscription",
        catalog_source: "session_acp_live",
        refresh_pending: false,
      },
    });
  });
});
