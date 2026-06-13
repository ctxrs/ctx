type EndpointRecord = Record<string, unknown>;

type ResolveEndpointModelOverrideTargetOpts = {
  providerId: string;
  endpoints: EndpointRecord[];
  selectedEndpointId: string;
  modelOverride: string;
  endpointBaseUrl?: string;
  endpointApiKey?: string;
};

type ResolvedEndpointModelOverrideTarget = {
  selectedEndpoint: EndpointRecord;
  upsertPayload: Record<string, unknown> | null;
};

const readString = (value: unknown): string => (typeof value === "string" ? value : "");

const firstText = (...values: unknown[]): string => {
  for (const value of values) {
    const text = readString(value).trim();
    if (text) return text;
  }
  return "";
};

export const openRouterEndpointName = (providerId: string): string => `${providerId}-openrouter`;

const buildSelectedEndpointPayload = (opts: {
  endpoint: EndpointRecord;
  modelOverride: string;
}): Record<string, unknown> | null => {
  const { endpoint, modelOverride } = opts;
  const endpointId = firstText(endpoint.id);
  const name = firstText(endpoint.name);
  if (!endpointId || !name) {
    return null;
  }

  const payload: Record<string, unknown> = {
    endpoint_id: endpointId,
    name,
    model_override: modelOverride,
  };
  const baseUrl = firstText(endpoint.base_url);
  if (baseUrl) payload.base_url = baseUrl;
  const apiShape = firstText(endpoint.api_shape);
  if (apiShape) payload.api_shape = apiShape;
  const authType = firstText(endpoint.auth_type);
  if (authType) payload.auth_type = authType;
  return payload;
};

const buildOpenRouterEndpointPayload = (opts: {
  providerId: string;
  endpoint: EndpointRecord;
  modelOverride: string;
  endpointBaseUrl: string;
  endpointApiKey: string;
}): Record<string, unknown> => {
  const { providerId, endpoint, modelOverride, endpointBaseUrl, endpointApiKey } = opts;
  const payload: Record<string, unknown> = {
    name: openRouterEndpointName(providerId),
    base_url: endpointBaseUrl,
    auth_type: "api_key",
    api_key: endpointApiKey,
    model_override: modelOverride,
  };

  const endpointId = firstText(endpoint.id);
  if (endpointId) {
    payload.endpoint_id = endpointId;
  }
  const apiShape = firstText(endpoint.api_shape);
  if (apiShape) {
    payload.api_shape = apiShape;
  }
  return payload;
};

export const resolveEndpointModelOverrideTarget = (
  opts: ResolveEndpointModelOverrideTargetOpts,
): ResolvedEndpointModelOverrideTarget => {
  const {
    providerId,
    endpoints,
    selectedEndpointId,
    modelOverride,
    endpointBaseUrl,
    endpointApiKey,
  } = opts;
  const targetModel = modelOverride.trim();

  if (endpointBaseUrl && endpointApiKey) {
    const selectedEndpoint =
      endpoints.find((entry) => firstText(entry.name) === openRouterEndpointName(providerId)) ??
      endpoints.find((entry) => firstText(entry.base_url) === endpointBaseUrl) ??
      {};

    const needsUpsert =
      Object.keys(selectedEndpoint).length === 0
      || firstText(selectedEndpoint.name) !== openRouterEndpointName(providerId)
      || firstText(selectedEndpoint.base_url) !== endpointBaseUrl
      || firstText(selectedEndpoint.auth_type) !== "api_key"
      || firstText(selectedEndpoint.model_override) !== targetModel;

    return {
      selectedEndpoint,
      upsertPayload: needsUpsert
        ? buildOpenRouterEndpointPayload({
          providerId,
          endpoint: selectedEndpoint,
          modelOverride: targetModel,
          endpointBaseUrl,
          endpointApiKey,
        })
        : null,
    };
  }

  const selectedEndpoint =
    endpoints.find((entry) => firstText(entry.id) === selectedEndpointId) ??
    (endpoints.length === 1 ? (endpoints[0] ?? {}) : {});
  const needsUpsert =
    Object.keys(selectedEndpoint).length > 0
    && firstText(selectedEndpoint.model_override) !== targetModel;

  return {
    selectedEndpoint,
    upsertPayload: needsUpsert
      ? buildSelectedEndpointPayload({
        endpoint: selectedEndpoint,
        modelOverride: targetModel,
      })
      : null,
  };
};
