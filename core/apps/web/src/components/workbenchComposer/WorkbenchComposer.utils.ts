import { type ProviderOptions } from "../../api/client";
import type { SessionViewVerbosity } from "../../state/uiStateStore";
import type { WorkbenchModeId } from "./WorkbenchComposer.types";
import type { ContextWindowInfo } from "./WorkbenchComposer.types";
import type { buildModelCatalog } from "../../utils/modelEffort";
import { composeModelId, parseModelId } from "../../utils/modelEffort";
import {
  hasFailedProviderModelProbe,
  hasProviderModels,
  isEndpointProviderSourceSelected,
  isFinalProviderModelCatalog,
  SUBSCRIPTION_MODEL_DISCOVERY_PROVIDER_IDS,
} from "../../utils/providerModelCatalog";

export const MENU_DESCRIPTIONS = {
  harness: `Agent harnesses are the low-level wrappers around models that provide the basic plumbing to allow the model to interact with the workspace. This normally includes features like filesystem access, shell access, configurations to set up MCP servers, and more. Despite similiarities between them, different harnesses will have varying tools, capabilities, and performance - even if used with the same underlying models. From here, you can install agent harnesses you haven't used before and switch which harness powers your next task.`,
  model: `You can switch between different models here. Model selection offers a tradeoff between cost, latency, and intelligence - but it also offers an opportunity to leverage the differences in their weights for collaboration. Even if two different models score similarly on popular coding benchmarks, they might have different "habits" - or biases. This means that if you are working on a pernicious bug fix, you might want multiple different models to both look at the problem from a different angle.`,
  effort: `Some models have a "thinking effort" or "reasoning effort" setting, while others do not. The effort level simply corresponds to how many tokens a model spends on thinking while solving a problem. Models that offer high or extra high can sometimes be very powerful, at the expense of latency and cost. However, you can also experience an unintended negative consequence from extra high thinking: if the model is emitting lots of thinking tokens that don't add much value, this will cause the context window to fill up faster (not just from thinking tokens alone, but also from more excessive tool calls like reading files). Performance on coding tasks declines as context increases beyond the minimum context needed to solve the problem, so effort level is a key lever in tuning your agent for optimal performance.`,
  mode: `Modes are basically just prompts, sometimes combined with access limitations. For example, the review mode is nothing more than prompting the agent to tell it to review the code and putting it in a read-only access level. That sounds fairly simple, but there is a hidden benefit: developers who build agent harnesses and models in conjunction will often train their custom model to use their bespoke harness, including its different modes. So in a way, this prompt can be more than just a regular prompt. It is a special prompt than has been trained on via reinforcement learning to achieve certain outcomes. For example, OpenAI trained their codex model to use their codex harness in review mode, so as to output only high value review comments with priority details. If you give the exact same prompt to a model that has not undergone the same RL, it will emit much less useful review comments. We recommend using RPIR (Research, Plan, Implement, Review) pattern for most changes except for small and easy ones.`,
  verbosity: `Verbosity controls how much activity is shown during a turn. Terse hides tools and thoughts, default shows summaries and thoughts, and verbose will eventually expand full tool details.`,
} as const;

export function clamp(n: number, min: number, max: number) {
  return Math.max(min, Math.min(max, n));
}

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

const selectedEndpointModelOverride = (opts?: ProviderOptions): string => {
  const source = opts?.source;
  if (!source || source.selected_source_kind !== "endpoint") return "";
  const endpointId = String(source.selected_endpoint_id ?? "").trim();
  if (!endpointId) return "";
  const endpoint = source.endpoints.find((candidate) => candidate.id === endpointId);
  return String(endpoint?.model_override ?? "").trim();
};

export function attachmentDisplayName(name?: string | null) {
  const n = String(name ?? "").trim();
  if (!n) return "image";
  return n.split(/[\\/]/).pop() || "image";
}

export function labelForMode(mode: WorkbenchModeId): string {
  if (mode === "default") return "Default";
  if (mode === "research") return "Research";
  if (mode === "plan") return "Plan";
  return "Review";
}

export function labelForVerbosity(level: SessionViewVerbosity): string {
  if (level === "terse") return "Terse";
  if (level === "verbose") return "Verbose";
  return "Default";
}

export function formatTokenCount(value: number): string {
  if (!Number.isFinite(value)) return "0";
  if (value >= 1_000_000) {
    const scaled = value / 1_000_000;
    const fixed = scaled >= 10 ? scaled.toFixed(0) : scaled.toFixed(1);
    return `${fixed.replace(/\.0$/, "")}m`;
  }
  if (value >= 1_000) {
    const scaled = value / 1_000;
    const fixed = scaled >= 100 ? scaled.toFixed(0) : scaled.toFixed(1);
    return `${fixed.replace(/\.0$/, "")}k`;
  }
  return `${Math.round(value)}`;
}

export function formatUsedTokenCount(value: number): string {
  if (!Number.isFinite(value)) return "0";
  if (value >= 1_000_000) {
    const scaled = value / 1_000_000;
    const fixed = scaled >= 10 ? scaled.toFixed(0) : scaled.toFixed(1);
    return `${fixed.replace(/\.0$/, "")}m`;
  }
  if (value >= 1_000) {
    const rounded = Math.round(value / 1_000);
    return `${rounded}k`;
  }
  return `${Math.round(value)}`;
}

export type ContextWindowDisplay = {
  percent?: number;
  usedLabel?: string;
  windowLabel?: string;
  title: string;
  summary: string;
};

export function describeContextWindow(
  contextWindow?: ContextWindowInfo | null,
) : ContextWindowDisplay | null {
  if (!contextWindow?.windowTokens) {
    return null;
  }
  let usedTokens = contextWindow.usedTokens;
  if (usedTokens == null && contextWindow.remainingTokens != null) {
    usedTokens = contextWindow.windowTokens - contextWindow.remainingTokens;
  }
  if (usedTokens == null && contextWindow.remainingFraction != null) {
    usedTokens = Math.round(contextWindow.windowTokens * (1 - contextWindow.remainingFraction));
  }
  if (usedTokens == null) {
    return null;
  }

  const windowTokens = Math.max(1, Math.round(contextWindow.windowTokens));
  const clampedUsed = Math.max(0, Math.min(windowTokens, Math.round(usedTokens)));
  const percent = Math.max(0, Math.min(100, Math.round((clampedUsed / windowTokens) * 100)));
  const usedLabel = formatUsedTokenCount(clampedUsed);
  const windowLabel = formatTokenCount(windowTokens);
  const summary = `${percent}% · ${usedLabel}/${windowLabel}`;

  return {
    percent,
    usedLabel,
    windowLabel,
    title: `Context Window: ${summary}`,
    summary,
  };
}

export function buildModelsFromCatalogPayload(raw?: unknown): Array<{ id: string; name?: string }> {
  if (!raw) return [];
  const rec = asRecord(raw);
  const list = rec.availableModels ?? rec.available_models ?? rec.models ?? raw;
  if (!Array.isArray(list)) return [];
  return list
    .map((m) => {
      const model = asRecord(m);
      return {
        id: String(model.modelId ?? model.model_id ?? model.id ?? model.name ?? "").trim(),
        name: typeof model.name === "string" ? model.name : undefined,
      };
    })
    .filter((m) => m.id.length > 0);
}

export function buildModelsFromProviderOptions(opts?: ProviderOptions): Array<{ id: string; name?: string }> {
  return buildModelsFromCatalogPayload(opts?.models);
}

function codexSlugDisplayName(modelId: string): string | null {
  const parsed = parseModelId(modelId);
  const base = parsed.base || parsed.full;
  if (!/^gpt-\d/i.test(base)) return null;
  return base.toLowerCase();
}

export function normalizeModelDisplayNamesForProvider(
  providerId: string,
  models: Array<{ id: string; name?: string }>,
): Array<{ id: string; name?: string }> {
  if (providerId.trim().toLowerCase() !== "codex") return models;
  return models.map((model) => {
    const displayName = codexSlugDisplayName(model.id);
    if (!displayName || model.name === displayName) return model;
    return { ...model, name: displayName };
  });
}

export function buildModelsForProvider(
  providerId: string,
  opts?: ProviderOptions,
): Array<{ id: string; name?: string }> {
  return normalizeModelDisplayNamesForProvider(providerId, buildModelsFromProviderOptions(opts));
}

export function shouldShowLoadingProviderModels(providerId: string, opts?: ProviderOptions): boolean {
  if (!opts) return true;
  if (hasProviderModels(opts)) return !isFinalProviderModelCatalog(opts);
  if (isEndpointProviderSourceSelected(opts)) return false;
  if (!SUBSCRIPTION_MODEL_DISCOVERY_PROVIDER_IDS.has(providerId)) return false;
  if (hasFailedProviderModelProbe(opts)) return false;
  return opts.has_active_auth === true;
}

export function pickDefaultEffort(efforts: string[]): string | null {
  if (efforts.includes("medium")) return "medium";
  return efforts[0] ?? null;
}

export function deriveFullModelIdForBase(
  catalog: ReturnType<typeof buildModelCatalog>,
  base: string,
  preferredEffort: string | null,
): string {
  const efforts = catalog.effortsByBase[base] ?? [];
  if (efforts.length === 0) return base;
  const eff = preferredEffort && efforts.includes(preferredEffort) ? preferredEffort : pickDefaultEffort(efforts);
  if (!eff) return base;
  const mapped = catalog.fullIdByBaseEffort[base]?.[eff];
  return mapped ?? composeModelId(base, eff);
}

export function modelIdFromProviderOptions(opts?: ProviderOptions): string | null {
  const raw = asRecord(opts?.models);
  const preferred = opts?.preferred_model_id;
  if (typeof preferred === "string" && preferred.trim().length > 0) {
    const preferredId = preferred.trim();
    const current = raw.currentModelId ?? raw.current_model_id;
    if (typeof current === "string" && current.trim() === preferredId) {
      return preferredId;
    }
    const list = raw.availableModels ?? raw.available_models ?? raw.models ?? [];
    if (Array.isArray(list)) {
      const preferredAvailable = list.some((entry) => {
        const model = asRecord(entry);
        const id = model.modelId ?? model.model_id ?? model.id ?? model.name;
        return typeof id === "string" && id.trim() === preferredId;
      });
      if (preferredAvailable) return preferredId;
    }
  }
  const current = raw.currentModelId ?? raw.current_model_id;
  if (typeof current === "string" && current.trim().length > 0) return current.trim();
  const sourceOverride = selectedEndpointModelOverride(opts);
  if (sourceOverride) return sourceOverride;
  if (Object.keys(raw).length === 0) return null;
  const list = raw.availableModels ?? raw.available_models ?? raw.models ?? [];
  if (!Array.isArray(list) || list.length === 0) return null;
  const first = asRecord(list[0]);
  const id = first.modelId ?? first.model_id ?? first.id ?? first.name;
  return typeof id === "string" && id.trim().length > 0 ? id.trim() : null;
}

export function nextAutoSeededModelId(
  currentModelId: string,
  nextResolvedModelId: string | null,
  previousAutoSeededModelId: string | null,
): string | null {
  const current = currentModelId.trim();
  const next = nextResolvedModelId?.trim() ?? "";
  const previousAuto = previousAutoSeededModelId?.trim() ?? "";
  if (!next) return null;
  if (!current) return next;
  if (previousAuto && current === previousAuto && current !== next) {
    return next;
  }
  return null;
}
