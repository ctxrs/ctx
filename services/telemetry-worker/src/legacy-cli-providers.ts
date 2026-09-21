export const V001_PROVIDERS = [
  "codex", "claude", "pi", "opencode", "antigravity", "gemini", "cursor", "copilot_cli",
  "factory_ai_droid",
] as const;
export const V016_PROVIDER_ADDITIONS = [
  "openclaw", "hermes", "nanoclaw", "astrbot", "custom",
] as const;
export const V020_PROVIDER_ADDITIONS = [
  "kilo", "kiro_cli", "tabnine", "windsurf", "zed", "qwen_code", "kimi_code_cli", "auggie",
  "junie", "firebender", "forgecode", "deepagents", "mistral_vibe", "mux", "rovodev",
  "shelley", "continue", "openhands", "cline", "roo_code", "crush", "goose", "lingma",
  "qoder", "warp", "codebuddy", "trae",
] as const;

export function providersForMinor(minor: number): ReadonlySet<string> {
  return new Set([
    ...V001_PROVIDERS,
    ...(minor >= 16 ? V016_PROVIDER_ADDITIONS : []),
    ...(minor >= 20 ? V020_PROVIDER_ADDITIONS : []),
    ...(minor >= 24 ? ["mimocode"] : []),
  ]);
}
