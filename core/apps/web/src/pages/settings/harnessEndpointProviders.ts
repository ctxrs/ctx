import type { HarnessApiShape } from "../../api/client";

export type HarnessEndpointProviderPreset = {
  id: string;
  label: string;
  base_url: string | null;
  recommended_api_shape: HarnessApiShape;
  logo_src?: string | null;
  invert_in_dark?: boolean;
  invert_in_light?: boolean;
};

type HarnessEndpointProviderPresetBase = Omit<
  HarnessEndpointProviderPreset,
  "logo_src" | "invert_in_dark" | "invert_in_light"
>;

const escapeRegex = (value: string): string => value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

const OPENROUTER_BASE_URL = "https://openrouter.ai/api/v1";
const OPENROUTER_ICON_URL = "https://openrouter.ai/favicon.ico";

const PROVIDER_LOGO_URL_BY_ID: Record<string, string> = {
  ai21: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://ai21.com/&size=256",
  aionlabs: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://www.aionlabs.ai/&size=256",
  alibaba_cloud_intl:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://www.alibabacloud.com/&size=256",
  amazon_bedrock: "https://openrouter.ai/images/icons/Bedrock.svg",
  anthropic: "https://openrouter.ai/images/icons/Anthropic.svg",
  arcee_ai: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://www.arcee.ai/&size=256",
  atlascloud:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://www.atlascloud.ai/&size=256",
  azure: "https://openrouter.ai/images/icons/Azure.svg",
  baseten: "https://openrouter.ai/images/icons/baseten-favicon.svg",
  cerebras: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://www.cerebras.ai/&size=256",
  chutes: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://chutes.ai/&size=256",
  cirrascale:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://www.cirrascale.com&size=256",
  clarifai:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://www.clarifai.com/&size=256",
  cloudflare:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://www.cloudflare.com/&size=256",
  cohere: "https://openrouter.ai/images/icons/Cohere.png",
  crusoe: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://www.crusoe.ai&size=256",
  deepinfra: "https://openrouter.ai/images/icons/DeepInfra.webp",
  deepseek: "https://openrouter.ai/images/icons/DeepSeek.png",
  featherless:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://featherless.ai/&size=256",
  fireworks: "https://openrouter.ai/images/icons/Fireworks.png",
  friendli:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://friendli.ai/&size=256",
  gmicloud: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://gmicloud.ai/&size=256",
  google_ai_studio: "https://openrouter.ai/images/icons/GoogleAIStudio.svg",
  google_vertex: "https://openrouter.ai/images/icons/GoogleVertex.svg",
  groq: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://groq.com/&size=256",
  hyperbolic:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://hyperbolic.xyz/&size=256",
  inception:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://www.inceptionlabs.ai/&size=256",
  inceptron:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://www.inceptron.io&size=256",
  infermatic:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://infermatic.ai/&size=256",
  inflection:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://inflection.ai/&size=256",
  liquid: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://www.liquid.ai/&size=256",
  mancer: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://mancer.tech/&size=256",
  minimax: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://minimaxi.com/&size=256",
  mistral: "https://openrouter.ai/images/icons/Mistral.png",
  moonshot_ai:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://moonshot.ai&size=256",
  morph: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://morphllm.com&size=256",
  nebius_token_factory:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://docs.nebius.com/&size=256",
  nextbit:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://nextbit256.com/&size=256",
  novita_ai: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://novita.ai/&size=256",
  openai: "https://openrouter.ai/images/icons/OpenAI.svg",
  openinference:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://openinference.xyz/&size=256",
  parasail:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://www.parasail.io/&size=256",
  perplexity: "https://openrouter.ai/images/icons/Perplexity.svg",
  phala: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://phala.network/&size=256",
  relace: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://relace.ai&size=256",
  sambanova:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://sambanova.ai/&size=256",
  switchpoint:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://switchpoint.dev/&size=256",
  together:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://www.together.ai/&size=256",
  venice: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://venice.ai/&size=256",
  weights_biases:
    "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://wandb.ai/home&size=256",
  xai: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://x.ai/&size=256",
  xiaomi: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://www.mi.com/&size=256",
  z_ai: "https://t0.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://z.ai/model-api&size=256",
  openrouter: OPENROUTER_ICON_URL,
};

const withProviderLogo = (preset: HarnessEndpointProviderPresetBase): HarnessEndpointProviderPreset => ({
  ...preset,
  logo_src: PROVIDER_LOGO_URL_BY_ID[preset.id] ?? null,
  invert_in_dark: preset.id === "openai",
});

const HARNESS_ENDPOINT_PROVIDER_PRESET_BASES = [
  { id: "ai21", label: "AI21", base_url: "https://api.ai21.com/studio/v1", recommended_api_shape: "openai_responses" },
  { id: "aionlabs", label: "AionLabs", base_url: "https://api.aionlabs.ai/v1", recommended_api_shape: "openai_responses" },
  {
    id: "alibaba_cloud_intl",
    label: "Alibaba Cloud Int.",
    base_url: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
    recommended_api_shape: "openai_responses",
  },
  {
    id: "amazon_bedrock",
    label: "Amazon Bedrock",
    base_url: "not_used",
    recommended_api_shape: "openai_responses",
  },
  { id: "anthropic", label: "Anthropic", base_url: "https://api.anthropic.com/v1", recommended_api_shape: "anthropic_messages" },
  { id: "arcee_ai", label: "Arcee AI", base_url: "https://api.clarifai.com/v2/ext/openai/v1", recommended_api_shape: "openai_responses" },
  { id: "atlascloud", label: "AtlasCloud", base_url: "https://api.atlascloud.ai/v1", recommended_api_shape: "openai_responses" },
  {
    id: "azure",
    label: "Azure",
    base_url: "https://openrouter-east-us-2.openai.azure.com/openai",
    recommended_api_shape: "openai_responses",
  },
  { id: "baseten", label: "Baseten", base_url: "https://inference.baseten.co/v1", recommended_api_shape: "openai_responses" },
  { id: "cerebras", label: "Cerebras", base_url: "https://api.cerebras.ai/v1", recommended_api_shape: "openai_responses" },
  { id: "chutes", label: "Chutes", base_url: "https://llm.chutes.ai/v1", recommended_api_shape: "openai_responses" },
  { id: "cirrascale", label: "Cirrascale", base_url: "https://ai2endpoints.cirrascale.ai/api", recommended_api_shape: "openai_responses" },
  { id: "clarifai", label: "Clarifai", base_url: "https://api.clarifai.com/v2/ext/openai/v1", recommended_api_shape: "openai_responses" },
  {
    id: "cloudflare",
    label: "Cloudflare",
    base_url: "https://api.cloudflare.com/client/v4/accounts/{accountId}/ai/v1",
    recommended_api_shape: "openai_responses",
  },
  { id: "cohere", label: "Cohere", base_url: "https://api.cohere.com/compatibility/v1", recommended_api_shape: "openai_responses" },
  { id: "crusoe", label: "Crusoe", base_url: "https://api.crusoe.ai/v1", recommended_api_shape: "openai_responses" },
  { id: "deepinfra", label: "DeepInfra", base_url: "https://api.deepinfra.com/v1/openai", recommended_api_shape: "openai_responses" },
  { id: "deepseek", label: "DeepSeek", base_url: "https://api.deepseek.com/beta", recommended_api_shape: "openai_responses" },
  { id: "featherless", label: "Featherless", base_url: "https://api.featherless.ai/v1", recommended_api_shape: "openai_responses" },
  { id: "fireworks", label: "Fireworks", base_url: "https://api.fireworks.ai/inference/v1", recommended_api_shape: "openai_responses" },
  { id: "friendli", label: "Friendli", base_url: "https://api.friendli.ai/serverless/v1", recommended_api_shape: "openai_responses" },
  { id: "gmicloud", label: "GMICloud", base_url: "https://api.gmi-serving.com/v1", recommended_api_shape: "openai_responses" },
  {
    id: "google_ai_studio",
    label: "Google AI Studio",
    base_url: "https://generativelanguage.googleapis.com/v1beta",
    recommended_api_shape: "openai_responses",
  },
  {
    id: "google_vertex",
    label: "Google Vertex",
    base_url: "not_used",
    recommended_api_shape: "openai_responses",
  },
  { id: "groq", label: "Groq", base_url: "https://api.groq.com/openai/v1", recommended_api_shape: "openai_responses" },
  { id: "hyperbolic", label: "Hyperbolic", base_url: "https://api.hyperbolic.xyz/v1", recommended_api_shape: "openai_responses" },
  { id: "inception", label: "Inception", base_url: "https://api.inceptionlabs.ai/v1", recommended_api_shape: "openai_responses" },
  { id: "inceptron", label: "Inceptron", base_url: "https://api.inceptron.io/v1", recommended_api_shape: "openai_responses" },
  { id: "infermatic", label: "Infermatic", base_url: "https://api.totalgpt.ai/v1", recommended_api_shape: "openai_responses" },
  { id: "inflection", label: "Inflection", base_url: "https://api.inflection.ai/v1", recommended_api_shape: "openai_responses" },
  { id: "liquid", label: "Liquid", base_url: "https://router.liquid.ai/v1", recommended_api_shape: "openai_responses" },
  { id: "mancer", label: "Mancer", base_url: "https://neuro.mancer.tech/oai/v1", recommended_api_shape: "openai_responses" },
  { id: "minimax", label: "MiniMax", base_url: "https://api.minimaxi.chat/v1", recommended_api_shape: "openai_responses" },
  { id: "mistral", label: "Mistral", base_url: "https://api.mistral.ai/v1", recommended_api_shape: "openai_responses" },
  { id: "moonshot_ai", label: "Moonshot AI", base_url: "https://api.moonshot.ai/v1", recommended_api_shape: "openai_responses" },
  { id: "morph", label: "Morph", base_url: "https://api.morphllm.com/v1", recommended_api_shape: "openai_responses" },
  { id: "nebius_token_factory", label: "Nebius Token Factory", base_url: "https://api.studio.nebius.ai/v1", recommended_api_shape: "openai_responses" },
  { id: "nextbit", label: "NextBit", base_url: "https://api.nextbit256.com/v1", recommended_api_shape: "openai_responses" },
  { id: "novita_ai", label: "NovitaAI", base_url: "https://api.novita.ai/v3/openai", recommended_api_shape: "openai_responses" },
  { id: "openai", label: "OpenAI", base_url: "https://api.openai.com/v1", recommended_api_shape: "openai_responses" },
  { id: "openinference", label: "OpenInference", base_url: "https://openinference.ngrok.io/v1", recommended_api_shape: "openai_responses" },
  { id: "parasail", label: "Parasail", base_url: "https://api.parasail.io/v1", recommended_api_shape: "openai_responses" },
  { id: "perplexity", label: "Perplexity", base_url: "https://api.perplexity.ai", recommended_api_shape: "openai_responses" },
  { id: "phala", label: "Phala", base_url: "https://api.redpill.ai/v1", recommended_api_shape: "openai_responses" },
  { id: "relace", label: "Relace", base_url: "https://instantapply.endpoint.relace.run/v1/apply", recommended_api_shape: "openai_responses" },
  { id: "sambanova", label: "SambaNova", base_url: "https://api.sambanova.ai/v1", recommended_api_shape: "openai_responses" },
  { id: "switchpoint", label: "Switchpoint", base_url: "https://www.switchpoint.dev/v1", recommended_api_shape: "openai_responses" },
  { id: "together", label: "Together", base_url: "https://api.together.xyz/v1", recommended_api_shape: "openai_responses" },
  { id: "venice", label: "Venice", base_url: "https://api.venice.ai/api/v1", recommended_api_shape: "openai_responses" },
  { id: "weights_biases", label: "Weights & Biases", base_url: "https://api.inference.wandb.ai/v1", recommended_api_shape: "openai_responses" },
  { id: "xai", label: "xAI", base_url: "https://api.x.ai/v1", recommended_api_shape: "openai_responses" },
  { id: "xiaomi", label: "Xiaomi", base_url: "https://api.xiaomimimo.com/v1", recommended_api_shape: "openai_responses" },
  { id: "z_ai", label: "Z.ai", base_url: "https://api.z.ai/api/paas/v4", recommended_api_shape: "openai_responses" },
  { id: "openrouter", label: "OpenRouter", base_url: OPENROUTER_BASE_URL, recommended_api_shape: "openai_responses" },
  { id: "other", label: "Other", base_url: null, recommended_api_shape: "openai_responses" },
] satisfies HarnessEndpointProviderPresetBase[];

const ALL_HARNESS_ENDPOINT_PROVIDER_PRESETS: HarnessEndpointProviderPreset[] =
  HARNESS_ENDPOINT_PROVIDER_PRESET_BASES.map(withProviderLogo);

// TEMP(v1): exclude providers that require provider-specific auth/context fields
// we have not implemented in the modal yet. Re-enable these soon in v2 when
// dedicated forms/validation are added.
const V1_TEMPORARILY_EXCLUDED_ENDPOINT_PROVIDER_IDS = new Set([
  "amazon_bedrock",
  "azure",
  "cloudflare",
  "google_vertex",
  "openinference",
]);

// Presets are intentionally launch-focused: known direct endpoints where stable,
// otherwise OpenRouter-compatible fallback so users can still route via one key.
export const HARNESS_ENDPOINT_PROVIDER_PRESETS: HarnessEndpointProviderPreset[] =
  ALL_HARNESS_ENDPOINT_PROVIDER_PRESETS
    .filter((preset) => !V1_TEMPORARILY_EXCLUDED_ENDPOINT_PROVIDER_IDS.has(preset.id))
    .sort((left, right) => {
      if (left.id === "other") return 1;
      if (right.id === "other") return -1;
      return left.label.localeCompare(right.label, undefined, { sensitivity: "base" });
    });

const PRESET_BY_ID = new Map(ALL_HARNESS_ENDPOINT_PROVIDER_PRESETS.map((preset) => [preset.id, preset]));
const OTHER_PRESET: HarnessEndpointProviderPreset = {
  id: "other",
  label: "Other",
  base_url: null,
  recommended_api_shape: "openai_responses",
};

export const getHarnessEndpointProviderPreset = (id: string): HarnessEndpointProviderPreset =>
  PRESET_BY_ID.get(id) ?? OTHER_PRESET;

export const defaultEndpointProviderPresetForHarness = (harnessProviderId: string): string => {
  if (harnessProviderId === "codex") return "openai";
  if (harnessProviderId === "claude-crp") return "anthropic";
  if (harnessProviderId === "gemini") return "google_ai_studio";
  if (harnessProviderId === "kimi") return "openrouter";
  if (harnessProviderId === "cursor") return "other";
  return "openrouter";
};

export const defaultShapeForHarnessProvider = (harnessProviderId: string): HarnessApiShape =>
  harnessProviderId === "claude-crp" ? "anthropic_messages" : "openai_responses";

export const supportsOptionalBaseUrlForHarness = (harnessProviderId: string): boolean =>
  harnessProviderId === "cody";

export const normalizeOptionalBaseUrl = (rawBaseUrl: string): string | null => {
  const trimmed = rawBaseUrl.trim();
  return trimmed.length > 0 ? trimmed : null;
};

export const nextDefaultEndpointName = (
  endpointProviderId: string,
  existingNames: string[],
): string => {
  const preset = getHarnessEndpointProviderPreset(endpointProviderId);
  const base = preset.label.trim() || "Endpoint";
  const matcher = new RegExp(`^${escapeRegex(base)}\\s+(\\d+)$`, "i");
  const usedNumbers = new Set<number>();

  for (const existingName of existingNames) {
    const match = existingName.trim().match(matcher);
    if (!match) continue;
    const parsed = Number.parseInt(match[1] ?? "", 10);
    if (Number.isFinite(parsed) && parsed > 0) {
      usedNumbers.add(parsed);
    }
  }

  let next = 1;
  while (usedNumbers.has(next)) next += 1;
  return `${base} ${next}`;
};

export const nextTokenEndpointName = (
  providerId: string,
  existingNames: string[],
): string => {
  const base = `${providerId} token`;
  const used = new Set(
    existingNames
      .map((value) => value.trim().toLowerCase())
      .filter((value) => value.length > 0),
  );
  if (!used.has(base.toLowerCase())) {
    return base;
  }
  let next = 2;
  while (used.has(`${base} ${next}`.toLowerCase())) {
    next += 1;
  }
  return `${base} ${next}`;
};
