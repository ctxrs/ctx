import type { APIRequestContext } from "playwright/test";
import { asArray, asRecord, readString } from "../../src/testing/providerRuntime";

export type GeminiEndpointAuthType = "gemini_api_key" | "vertex_ai";

type EnsureGeminiEndpointSelectedOptions = {
  apiKey?: string;
  serviceAccountJson?: string;
  projectId?: string;
  location?: string;
  authType: GeminiEndpointAuthType;
  endpointName: string;
  modelId: string;
  requestTimeoutMs: number;
};

export const GEMINI_DEFAULT_MODEL_ID = "gemini-2.0-flash-lite";

export const selectedGeminiEndpointForConfig = (
  config: Record<string, unknown>,
): Record<string, unknown> | null => {
  const selectedEndpointId = readString(config.selected_endpoint_id);
  if (!selectedEndpointId) return null;
  return asArray(config.endpoints)
    .map((entry) => asRecord(entry))
    .find((entry) => readString(entry.id) === selectedEndpointId) ?? null;
};

export async function readGeminiHarnessConfig(
  request: APIRequestContext,
  requestTimeoutMs: number,
): Promise<Record<string, unknown>> {
  const response = await request.get("/api/providers/gemini/harness_config", {
    timeout: requestTimeoutMs,
  });
  if (!response.ok()) {
    throw new Error(`gemini harness config read failed (${response.status()})`);
  }
  return asRecord(await response.json());
}

const endpointMatches = (
  endpoint: Record<string, unknown> | null,
  opts: EnsureGeminiEndpointSelectedOptions,
): boolean => {
  if (!endpoint) return false;
  return readString(endpoint.name) === opts.endpointName
    && readString(endpoint.auth_type) === opts.authType
    && readString(endpoint.model_override) === opts.modelId;
};

export async function ensureGeminiEndpointSelected(
  request: APIRequestContext,
  opts: EnsureGeminiEndpointSelectedOptions,
): Promise<Record<string, unknown>> {
  let config = await readGeminiHarnessConfig(request, opts.requestTimeoutMs);
  let selectedEndpoint = selectedGeminiEndpointForConfig(config);
  if (readString(config.selected_source_kind) === "endpoint" && endpointMatches(selectedEndpoint, opts)) {
    return config;
  }

  const existingEndpoint =
    asArray(config.endpoints)
      .map((entry) => asRecord(entry))
      .find((entry) => readString(entry.name) === opts.endpointName)
    ?? selectedEndpoint;
  const endpointId = readString(existingEndpoint?.id) || null;

  const upsertResponse = await request.post("/api/providers/gemini/harness_config/endpoints", {
    data: {
      endpoint_id: endpointId,
      name: opts.endpointName,
      auth_type: opts.authType,
      api_key: opts.authType === "vertex_ai" ? null : (opts.apiKey ?? null),
      service_account_json: opts.authType === "vertex_ai" ? (opts.serviceAccountJson ?? null) : null,
      project_id: opts.authType === "vertex_ai" ? (opts.projectId ?? null) : null,
      location: opts.authType === "vertex_ai" ? (opts.location ?? null) : null,
      model_override: opts.modelId,
    },
    timeout: opts.requestTimeoutMs,
  });
  if (!upsertResponse.ok()) {
    const body = await upsertResponse.text().catch(() => "");
    throw new Error(`gemini endpoint upsert failed (${upsertResponse.status()}): ${body}`);
  }
  config = asRecord(await upsertResponse.json());
  selectedEndpoint =
    asArray(config.endpoints)
      .map((entry) => asRecord(entry))
      .find((entry) => endpointMatches(entry, opts))
    ?? selectedGeminiEndpointForConfig(config);
  const selectedEndpointId = readString(selectedEndpoint?.id);
  if (!selectedEndpointId) {
    throw new Error("gemini endpoint upsert returned no endpoint id");
  }

  const selectResponse = await request.post("/api/providers/gemini/harness_config/select", {
    data: {
      source_kind: "endpoint",
      endpoint_id: selectedEndpointId,
    },
    timeout: opts.requestTimeoutMs,
  });
  if (!selectResponse.ok()) {
    const body = await selectResponse.text().catch(() => "");
    throw new Error(`gemini endpoint select failed (${selectResponse.status()}): ${body}`);
  }
  return asRecord(await selectResponse.json());
}
