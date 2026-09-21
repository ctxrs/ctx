import manifest from "../contracts/providers-v1.json";

export type ProviderClassification =
  | "current"
  | "historical"
  | "unrecognized"
  | "neutral"
  | "invalid";

const PROVIDER_ID_PATTERN = /^[a-z][a-z0-9_]{0,63}$/u;
const entries = readManifest(manifest);

export const CURRENT_PROVIDER_IDS = Object.freeze(
  entries.filter((entry) => entry.status === "current").map((entry) => entry.id),
);
export const HISTORICAL_PROVIDER_IDS = Object.freeze(
  entries.filter((entry) => entry.status === "retired").map((entry) => entry.id),
);
export const CURRENT_PROVIDERS: ReadonlySet<string> = new Set(CURRENT_PROVIDER_IDS);
export const HISTORICAL_PROVIDERS: ReadonlySet<string> = new Set(HISTORICAL_PROVIDER_IDS);
export const PROVIDERS: ReadonlySet<string> = new Set([
  ...CURRENT_PROVIDER_IDS,
  ...HISTORICAL_PROVIDER_IDS,
]);

export function isProviderId(value: string): boolean {
  return PROVIDER_ID_PATTERN.test(value);
}

export function normalizeProviderId(value: string): string {
  return PROVIDERS.has(value) ? value : "unknown";
}

export function classifyProvider(value: unknown): ProviderClassification {
  if (value === undefined || value === null) return "neutral";
  if (typeof value !== "string" || !isProviderId(value)) return "invalid";
  if (CURRENT_PROVIDERS.has(value)) return "current";
  if (HISTORICAL_PROVIDERS.has(value)) return "historical";
  return "unrecognized";
}

type ProviderEntry = Readonly<{ id: string; status: "current" | "retired" }>;

function readManifest(value: unknown): readonly ProviderEntry[] {
  if (!isRecord(value) || value.schema_version !== 1 || value.vocabulary !== "ctx-telemetry-provider") {
    throw new Error("invalid_provider_manifest");
  }
  if (!Array.isArray(value.providers) || value.providers.length === 0) {
    throw new Error("invalid_provider_manifest");
  }
  const seen = new Set<string>();
  const parsed = value.providers.map((entry): ProviderEntry => {
    if (
      !isRecord(entry)
      || typeof entry.id !== "string"
      || !isProviderId(entry.id)
      || (entry.status !== "current" && entry.status !== "retired")
      || seen.has(entry.id)
    ) {
      throw new Error("invalid_provider_manifest");
    }
    seen.add(entry.id);
    return { id: entry.id, status: entry.status };
  });
  if (!parsed.some((entry) => entry.id === "unknown" && entry.status === "current")) {
    throw new Error("invalid_provider_manifest");
  }
  return parsed;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
