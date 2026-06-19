import type { SlashCommandDescriptor } from "../state/useComposerAutocomplete";

const CLAUDE_REDUNDANT_COMMANDS = new Set<string>([
  "allowed-tools",
  "clear",
  "config",
  "continue",
  "diff",
  "exit",
  "login",
  "logout",
  "model",
  "new",
  "permissions",
  "quit",
  "rename",
  "reset",
  "resume",
  "sandbox",
  "settings",
  "status",
  "tasks",
]);

const CLAUDE_UNSUPPORTED_COMMANDS = new Set<string>([
  "add-dir",
  "agents",
  "android",
  "app",
  "checkpoint",
  "chrome",
  "copy",
  "desktop",
  "export",
  "extra-usage",
  "fork",
  "hooks",
  "ide",
  "install-github-app",
  "install-slack-app",
  "ios",
  "keybindings",
  "mcp",
  "mobile",
  "passes",
  "plugin",
  "privacy-settings",
  "rc",
  "reload-plugins",
  "remote-control",
  "remote-env",
  "rewind",
  "statusline",
  "stickers",
  "terminal-setup",
  "theme",
  "upgrade",
  "vim",
]);

const CODEX_REDUNDANT_COMMANDS = new Set<string>([
  "approvals",
  "clear",
  "copy",
  "diff",
  "exit",
  "mention",
  "model",
  "new",
  "permissions",
  "quit",
  "resume",
  "status",
]);

const CODEX_UNSUPPORTED_COMMANDS = new Set<string>([
  "agent",
  "apps",
  "clean",
  "collab",
  "debug-config",
  "debug-m-drop",
  "debug-m-update",
  "experimental",
  "feedback",
  "fork",
  "init",
  "logout",
  "mcp",
  "personality",
  "plan",
  "ps",
  "realtime",
  "rename",
  "rollout",
  "sandbox-add-read-dir",
  "setup-default-sandbox",
  "skills",
  "statusline",
  "test-approval",
  "theme",
]);

const asRecord = (value: unknown): Record<string, unknown> | null => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  return value as Record<string, unknown>;
};

const readString = (value: unknown): string | undefined => {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed ? trimmed : undefined;
};

const providerSourceLabel = (providerId: string): string => {
  if (providerId === "claude-crp") return "Claude Code";
  if (providerId === "codex") return "Codex";
  return providerId;
};

const normalizeSlashCommandName = (value: unknown): string | undefined => {
  const raw = readString(value);
  if (!raw) return undefined;
  const normalized = raw.replace(/^\/+/, "").trim();
  return normalized || undefined;
};

const isCtxSupportedClaudeProtocolCommand = (name: string): boolean => {
  const normalized = normalizeSlashCommandName(name);
  if (!normalized) return false;
  const key = normalized.toLowerCase();
  if (key.startsWith("mcp__")) return false;
  return !CLAUDE_REDUNDANT_COMMANDS.has(key) && !CLAUDE_UNSUPPORTED_COMMANDS.has(key);
};

const isCtxSupportedCodexProtocolCommand = (name: string): boolean => {
  const normalized = normalizeSlashCommandName(name);
  if (!normalized) return false;
  const key = normalized.toLowerCase();
  if (key.startsWith("prompts:")) return false;
  return !CODEX_REDUNDANT_COMMANDS.has(key) && !CODEX_UNSUPPORTED_COMMANDS.has(key);
};

const readCommandDescriptor = (value: unknown): SlashCommandDescriptor | null => {
  const record = asRecord(value);
  if (!record) return null;
  const name = normalizeSlashCommandName(record.name);
  if (!name) return null;
  return {
    name,
    description: readString(record.description),
    argumentHint: readString(record.argumentHint) ?? readString(record.argument_hint),
  };
};

export function deriveProtocolSlashCommands(meta: {
  providerId?: string;
  commands?: unknown;
  slashCommands?: unknown;
}): SlashCommandDescriptor[] {
  const out: SlashCommandDescriptor[] = [];
  const seen = new Set<string>();
  const providerId = readString(meta.providerId);
  const filterClaude = providerId === "claude-crp";
  const filterCodex = providerId === "codex";
  const providerSource = providerId
    ? {
        kind: "provider" as const,
        providerId,
        protocol: providerId === "claude-crp" ? "CRP" : providerId === "codex" ? "ACP" : undefined,
        label: providerSourceLabel(providerId),
      }
    : undefined;

  const add = (command: SlashCommandDescriptor | null) => {
    if (!command) return;
    const key = command.name.trim().toLowerCase();
    if (!key || seen.has(key)) return;
    if (filterClaude && !isCtxSupportedClaudeProtocolCommand(key)) return;
    if (filterCodex && !isCtxSupportedCodexProtocolCommand(key)) return;
    seen.add(key);
    out.push(providerSource ? { ...command, source: command.source ?? providerSource } : command);
  };

  if (Array.isArray(meta.commands)) {
    for (const entry of meta.commands) {
      add(readCommandDescriptor(entry));
    }
  }

  if (Array.isArray(meta.slashCommands)) {
    for (const entry of meta.slashCommands) {
      const name = normalizeSlashCommandName(entry);
      if (!name) continue;
      add({ name });
    }
  }

  return out;
}
