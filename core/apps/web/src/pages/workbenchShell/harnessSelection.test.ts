import { describe, expect, it } from "vitest";
import type { ProviderOptions, ProviderStatus } from "../../api/client";
import {
  collectSelectableHarnessProviderIds,
  getHarnessMruStorageKey,
  resolveDefaultHarnessProviderId,
  resolveDraftHarnessReplacement,
  resolveInitialHarnessSelection,
  shouldFinalizeInitialHarnessSelection,
} from "./harnessSelection";

const provider = (
  provider_id: string,
  opts?: Partial<ProviderStatus>,
): ProviderStatus => ({
  provider_id,
  installed: true,
  health: "ok",
  diagnostics: [],
  usability: {
    usable: true,
    status: "ready",
    blocking_provider_ids: [],
    recommended_action: "none",
  },
  ...opts,
});

const baseOptions = (providerId: string): ProviderOptions => ({
  provider_id: providerId,
  workspace_id: "ws-test",
  supports_load: false,
  auth_required: false,
  probed_at: new Date().toISOString(),
});

describe("harnessSelection", () => {
  it("builds workspace-scoped MRU keys", () => {
    expect(getHarnessMruStorageKey("ws-123")).toBe("wb.harnessMru.ws-123");
  });

  it("collects only installed, healthy, visible providers", () => {
    const providersById: Record<string, ProviderStatus> = {
      codex: provider("codex"),
      hidden: provider("hidden", { details: { ui_hidden: "true" } }),
      missing: provider("missing", {
        installed: false,
        usability: {
          usable: false,
          status: "installable",
          blocking_provider_ids: [],
          recommended_action: "install",
        },
      }),
      unhealthy: provider("unhealthy", {
        health: "error",
        usability: {
          usable: false,
          status: "blocked",
          blocking_provider_ids: [],
          recommended_action: "none",
        },
      }),
    };
    expect(collectSelectableHarnessProviderIds(providersById)).toEqual(["codex"]);
  });

  it("resolves the preferred default harness provider from installed providers", () => {
    expect(resolveDefaultHarnessProviderId([
      provider("cursor"),
      provider("gemini"),
      provider("claude-crp"),
    ])).toBe("claude-crp");
  });

  it("does not prefer deferred non-writer harnesses over supported defaults", () => {
    expect(resolveDefaultHarnessProviderId([
      provider("goose"),
      provider("kimi"),
    ])).toBe("kimi");
  });

  it("falls back to the first installed provider or codex when no preferred provider exists", () => {
    expect(resolveDefaultHarnessProviderId([
      provider("cursor"),
      provider("fake"),
    ])).toBe("cursor");
    expect(resolveDefaultHarnessProviderId([])).toBe("codex");
  });

  it("replaces a placeholder codex draft when codex is unavailable", () => {
    expect(resolveDraftHarnessReplacement({
      draftHarness: { providerId: "codex", modelId: "" },
      providersById: {
        "claude-crp": provider("claude-crp"),
      },
      defaultProviderId: "claude-crp",
    })).toEqual({ providerId: "claude-crp", modelId: "" });
  });

  it("keeps explicit or still-valid drafts unchanged during replacement checks", () => {
    const explicitDraft = { providerId: "codex", modelId: "gpt-5/xhigh" };
    expect(resolveDraftHarnessReplacement({
      draftHarness: explicitDraft,
      providersById: {
        "claude-crp": provider("claude-crp"),
      },
      defaultProviderId: "claude-crp",
    })).toEqual(explicitDraft);

    const placeholderDraft = { providerId: "codex", modelId: "" };
    expect(resolveDraftHarnessReplacement({
      draftHarness: placeholderDraft,
      providersById: {
        codex: provider("codex"),
        "claude-crp": provider("claude-crp"),
      },
      defaultProviderId: "codex",
    })).toEqual(placeholderDraft);
  });

  it("prefers MRU when that provider has active auth", () => {
    const providerOptions: Record<string, ProviderOptions | undefined> = {
      codex: { ...baseOptions("codex"), has_active_auth: true },
      cursor: { ...baseOptions("cursor"), has_active_auth: true },
    };
    const selected = resolveInitialHarnessSelection({
      providerIds: ["codex", "cursor"],
      providerOptions,
      mruProviderId: "cursor",
    });
    expect(selected).toBe("cursor");
  });

  it("falls back to single authed provider when MRU is invalid", () => {
    const providerOptions: Record<string, ProviderOptions | undefined> = {
      codex: { ...baseOptions("codex"), has_active_auth: true },
      cursor: baseOptions("cursor"),
    };
    const selected = resolveInitialHarnessSelection({
      providerIds: ["codex", "cursor"],
      providerOptions,
      mruProviderId: "cursor",
    });
    expect(selected).toBe("codex");
  });

  it("does not auto-select fake when a real installed harness exists", () => {
    const selected = resolveInitialHarnessSelection({
      providerIds: ["fake", "cline"],
      providerOptions: {
        fake: { ...baseOptions("fake"), has_active_auth: true },
        cline: baseOptions("cline"),
      },
      mruProviderId: "fake",
    });
    expect(selected).toBeNull();
  });

  it("returns null when none or multiple candidates are authed", () => {
    const noneAuthed = resolveInitialHarnessSelection({
      providerIds: ["codex", "cursor"],
      providerOptions: {
        codex: baseOptions("codex"),
        cursor: baseOptions("cursor"),
      },
      mruProviderId: null,
    });
    expect(noneAuthed).toBeNull();

    const multipleAuthed = resolveInitialHarnessSelection({
      providerIds: ["codex", "cursor"],
      providerOptions: {
        codex: { ...baseOptions("codex"), has_active_auth: true },
        cursor: { ...baseOptions("cursor"), has_active_auth: true },
      },
      mruProviderId: null,
    });
    expect(multipleAuthed).toBeNull();
  });

  it("returns null when manual demo selection disables autoselect", () => {
    const selected = resolveInitialHarnessSelection({
      providerIds: ["codex"],
      providerOptions: {
        codex: { ...baseOptions("codex"), has_active_auth: true },
      },
      mruProviderId: "codex",
      disableAutoselect: true,
    });
    expect(selected).toBeNull();
  });

  it("finalizes initial resolver only after a provider is actually selected", () => {
    expect(shouldFinalizeInitialHarnessSelection(null)).toBe(false);
    expect(shouldFinalizeInitialHarnessSelection("codex")).toBe(true);
  });
});
