export type EndpointHarnessMatrixEntry = {
  providerId: string;
  menuLabel: string;
  searchTerm: string;
};

// First-pass endpoint harness cohort for OpenRouter endpoint + API key validation.
// Keep this limited to providers with a completed live green write-file baseline
// through the shared OpenRouter flow.
export const OPENROUTER_ENDPOINT_FIRST_PASS_HARNESSES: EndpointHarnessMatrixEntry[] = [
  { providerId: "codex", menuLabel: "Codex", searchTerm: "codex" },
  { providerId: "cline", menuLabel: "Cline", searchTerm: "cline" },
  { providerId: "qwen", menuLabel: "Qwen Code", searchTerm: "qwen" },
  { providerId: "copilot", menuLabel: "Copilot", searchTerm: "copilot" },
  { providerId: "goose", menuLabel: "Goose", searchTerm: "goose" },
  { providerId: "openhands", menuLabel: "OpenHands", searchTerm: "openhands" },
  { providerId: "pi", menuLabel: "PI", searchTerm: "pi" },
];

// Explicitly runnable on the shared OpenRouter flow for focused/manual reruns,
// but still deferred from the default first-pass suite until they have a live
// green write-file baseline.
export const OPENROUTER_ENDPOINT_FOCUSED_DEFERRED_HARNESSES: EndpointHarnessMatrixEntry[] = [
  { providerId: "droid", menuLabel: "Droid", searchTerm: "droid" },
  { providerId: "kimi", menuLabel: "Kimi", searchTerm: "kimi" },
  { providerId: "mistral", menuLabel: "Mistral", searchTerm: "mistral" },
  { providerId: "opencode", menuLabel: "OpenCode", searchTerm: "opencode" },
];

// Explicitly excluded from this suite because they need a dedicated non-OpenRouter lane.
export const OPENROUTER_ENDPOINT_FIRST_PASS_EXCLUDED_PROVIDER_TOKEN_ONLY = [
  "gemini",
  "cursor",
  "claude-crp",
  "cagent",
] as const;

// Tracked but deferred harnesses intentionally excluded from first-pass endpoint e2e
// until they have a completed green live write-file baseline.
export const OPENROUTER_ENDPOINT_FIRST_PASS_EXCLUDED_DEFERRED = [
  "junie",
  "codebuff",
  "charm",
  "aider",
  "kilo",
  // These do not satisfy the shared OpenRouter write-file contract today.
  "amp",
  "auggie",
] as const;
