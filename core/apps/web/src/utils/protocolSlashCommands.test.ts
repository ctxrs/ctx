import { describe, expect, it } from "vitest";
import { deriveProtocolSlashCommands } from "./protocolSlashCommands";

describe("deriveProtocolSlashCommands", () => {
  it("prefers structured protocol commands and appends raw slash names without duplicates", () => {
    const commands = deriveProtocolSlashCommands({
      commands: [
        {
          name: "/compact",
          description: "Summarize conversation to save context",
          argument_hint: "<focus>",
        },
      ],
      slashCommands: ["/compact", "review", "security-review"],
    });

    expect(commands).toEqual([
      {
        name: "compact",
        description: "Summarize conversation to save context",
        argumentHint: "<focus>",
      },
      {
        name: "review",
      },
      {
        name: "security-review",
      },
    ]);
  });

  it("returns an empty list when no protocol metadata exists", () => {
    expect(deriveProtocolSlashCommands({})).toEqual([]);
    expect(deriveProtocolSlashCommands({ commands: null, slashCommands: null })).toEqual([]);
  });

  it("filters redundant and unsupported Claude commands while keeping supported ones", () => {
    const commands = deriveProtocolSlashCommands({
      providerId: "claude-crp",
      commands: [
        { name: "/compact" },
        { name: "/clear" },
        { name: "/mcp" },
        { name: "/mcp__docs__search" },
      ],
      slashCommands: ["/review", "/status", "/desktop"],
    });

    expect(commands).toEqual([
      {
        name: "compact",
        source: {
          kind: "provider",
          providerId: "claude-crp",
          protocol: "CRP",
          label: "Claude Code",
        },
      },
      {
        name: "review",
        source: {
          kind: "provider",
          providerId: "claude-crp",
          protocol: "CRP",
          label: "Claude Code",
        },
      },
    ]);
  });

  it("filters Codex commands down to the subset ctx actually supports", () => {
    const commands = deriveProtocolSlashCommands({
      providerId: "codex",
      commands: [
        { name: "/compact", description: "Summarize conversation" },
        { name: "/status" },
        { name: "/plan" },
        { name: "/prompts:shipit" },
      ],
      slashCommands: ["/review", "/copy", "/prompts:cleanup"],
    });

    expect(commands).toEqual([
      {
        name: "compact",
        description: "Summarize conversation",
        source: {
          kind: "provider",
          providerId: "codex",
          protocol: "ACP",
          label: "Codex",
        },
      },
      {
        name: "review",
        source: {
          kind: "provider",
          providerId: "codex",
          protocol: "ACP",
          label: "Codex",
        },
      },
    ]);
  });

  it("uses the provider id as a safe source label for unknown providers", () => {
    const commands = deriveProtocolSlashCommands({
      providerId: "custom-provider",
      slashCommands: ["/review"],
    });

    expect(commands).toEqual([
      {
        name: "review",
        source: {
          kind: "provider",
          providerId: "custom-provider",
          protocol: undefined,
          label: "custom-provider",
        },
      },
    ]);
  });
});
