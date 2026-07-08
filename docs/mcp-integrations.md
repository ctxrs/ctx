# MCP Integrations

`ctx integrations install mcp` installs a local MCP server entry named `ctx`
for coding-agent clients that have a stable, file-backed MCP configuration. The
server command is:

```json
{
  "command": "ctx",
  "args": ["mcp", "serve"]
}
```

With no `--agent` flag, the command installs only for supported agents that are
detected on the machine. In `--project` mode, the default is similarly limited
to project config locations that already exist. Use `--agent <name>` (or the
`--provider` alias) for one target, `--all-agents` for every implemented target
in the selected scope, and `--project` for the current workspace when that agent
has a documented project MCP config.

The installer parses JSON, TOML, and YAML configs with structured parsers,
preserves unrelated settings, is idempotent, and refuses to overwrite an
existing `ctx` MCP server whose command or args differ unless `--force` is set.
Invalid configs are reported and left untouched.

## Test Coverage

The Bazel target `//:mcp_integration_e2e` runs hermetic fake-harness tests for
the stable local configuration surfaces. Those tests run the real ctx installer
with temporary `HOME`, `XDG_CONFIG_HOME`, `CTX_DATA_ROOT`, and provider-specific
home variables, then parse the generated provider configs with host-like
readers. They also model project trust or approval gates for Codex, Claude Code,
and Qwen Code, and exercise the real `ctx mcp serve` stdio JSON-RPC path. Live
third-party CLI smoke tests remain optional because they require installed
harness binaries, auth state, or interactive approval.

```bash
ctx integrations install mcp
ctx integrations install mcp --agent codex
ctx integrations install mcp --provider cursor --project
ctx integrations install mcp --all-agents --json
ctx integrations status mcp --json
```

## Support Matrix

This matrix covers the coding-agent harnesses in the public ctx provider
support suite. "Implemented" means ctx can safely write the current local
`ctx mcp serve` config for that target today. "Unknown" means ctx supports
history import for that harness, but no current authoritative MCP config
location/schema was verified during implementation research.

| Harness | MCP support | User/global config | Project config | Format | Safest add/update/remove strategy | Detection signal | Source | Notes/risks |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Codex | Yes, implemented | `~/.codex/config.toml`, or `CODEX_HOME/config.toml` | `.codex/config.toml` | TOML `[mcp_servers.ctx]`, `command`, `args` | Parse TOML and upsert only `mcp_servers.ctx`; use `--force` for conflicting `ctx`; remove by deleting that table | `CODEX_HOME`, `~/.codex`, or `/etc/codex` | [OpenAI Codex config](https://developers.openai.com/codex/config-reference), [ctx providers](provider-support.md) | Project config is loaded only where Codex trusts the project. |
| Pi | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | ctx history source under Pi session defaults | [ctx providers](provider-support.md) | ctx can import history; MCP config support was not verified from official docs. |
| Claude Code | Yes, implemented | `~/.claude.json`, or `CLAUDE_CONFIG_DIR/.claude.json` | `.mcp.json` | JSON `mcpServers.ctx`, `type: "stdio"`, `command`, `args` | Prefer `claude mcp add/remove` when interactive; ctx merges only the `ctx` server key | `CLAUDE_CONFIG_DIR`, `~/.claude`, or `~/.claude.json` | [Claude Code MCP](https://code.claude.com/docs/en/mcp-quickstart), [ctx providers](provider-support.md) | User/local/project scopes can coexist; approval may still be required in Claude Code. |
| OpenCode | Yes, implemented | `~/.config/opencode/opencode.json` | `opencode.json` | JSON/JSONC-style config with `mcp.ctx`, local server uses `type: "local"` and command argv array | Parse JSON and upsert `mcp.ctx`; remove by deleting that key | `~/.config/opencode` | [OpenCode config](https://opencode.ai/docs/config/), [OpenCode CLI](https://opencode.ai/docs/cli/), [ctx providers](provider-support.md) | ctx currently writes strict JSON; JSONC comments in an existing file are reported as invalid. |
| Kilo Code | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | ctx Kilo history source defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Kiro | Yes, implemented | `~/.kiro/settings/mcp.json` | `.kiro/settings/mcp.json` | JSON `mcpServers.ctx`, `command`, `args` | Parse JSON and upsert only `mcpServers.ctx`; Kiro UI can also open user/workspace config | `~/.kiro` | [Kiro MCP config](https://kiro.dev/docs/mcp/configuration/), [ctx providers](provider-support.md) | Workspace config takes precedence over user config. |
| Crush | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | ctx Crush SQLite source defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Goose | Yes, implemented | `~/.config/goose/config.yaml` | Not documented | YAML `extensions.ctx`, `type: stdio`, `cmd`, `args` | Prefer `goose configure` when interactive; ctx parses YAML and upserts only `extensions.ctx` | `~/.config/goose` | [Goose config files](https://goose-docs.ai/docs/guides/config-files/), [Goose extensions](https://goose-docs.ai/docs/getting-started/using-extensions/), [ctx providers](provider-support.md) | Goose extensions are MCP servers; project-scoped MCP config was not verified. |
| Lingma | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | ctx Lingma SQLite source defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Qoder | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | ctx Qoder transcript source defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Warp | Yes, implemented | `~/.warp/.mcp.json` | `.warp/.mcp.json` | JSON `mcpServers.ctx`, `command`, `args` | Prefer Warp UI or `/agent-add-mcp`; ctx parses JSON and upserts only `mcpServers.ctx` | `~/.warp` | [Warp MCP](https://docs.warp.dev/agent-platform/capabilities/mcp/), [ctx providers](provider-support.md) | Warp requires approval around file-based config edits; project servers do not auto-spawn. |
| CodeBuddy | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | ctx CodeBuddy history source defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Trae | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | ctx Trae state source defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| OpenClaw | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | `OPENCLAW_STATE_DIR` or ctx OpenClaw defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Hermes Agent | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | `HERMES_HOME` or ctx Hermes defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| NanoClaw | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | ctx NanoClaw explicit import paths | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| AstrBot | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | `ASTRBOT_ROOT` or ctx AstrBot defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Shelley | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | `SHELLEY_DB` or ctx Shelley defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Continue | Yes, implemented | `~/.continue/config.yaml` | `.continue/mcpServers/ctx.yaml` | YAML `mcpServers` sequence with `name`, `type`, `command`, `args` | Parse YAML and upsert the server named `ctx`; standalone project block includes required metadata | Existing `~/.continue/config.yaml` | [Continue MCP](https://docs.continue.dev/customize/deep-dives/mcp), [Continue config](https://docs.continue.dev/customize/deep-dives/configuration), [ctx providers](provider-support.md) | MCP is available in agent mode; default install avoids creating a new global YAML config unless Continue already uses one. |
| OpenHands | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | ctx OpenHands file-event source defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Antigravity | Unknown | Unknown | Unknown | Unknown | Do not infer Gemini config; do not write MCP config | `~/.gemini/antigravity-*` history defaults | [ctx providers](provider-support.md) | Antigravity uses Gemini-family paths for ctx history, but official MCP config docs were not verified. |
| Gemini CLI | Yes, implemented | `~/.gemini/settings.json` | `.gemini/settings.json` | JSON `mcpServers.ctx`, `command`, `args` | Prefer `gemini mcp add/remove` when available; ctx parses JSON and upserts only `mcpServers.ctx` | `~/.gemini` | [Gemini CLI MCP](https://github.com/google-gemini/gemini-cli/blob/main/docs/tools/mcp-server.md), [ctx providers](provider-support.md) | Gemini supports additional MCP trust and tool allowlist settings that ctx leaves untouched. |
| Tabnine | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | ctx Tabnine chat recording defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Cursor | Yes, implemented | `~/.cursor/mcp.json` | `.cursor/mcp.json` | JSON `mcpServers.ctx`, `type: "stdio"`, `command`, `args` | Parse JSON and upsert only `mcpServers.ctx`; remove by deleting that key | `~/.cursor` | [Cursor MCP](https://cursor.com/docs/mcp.md), [Cursor CLI MCP](https://cursor.com/docs/cli/mcp.md), [ctx providers](provider-support.md) | Cursor CLI uses the same MCP config as Cursor. |
| Windsurf | Yes, implemented | `~/.codeium/mcp_config.json` | Not documented | JSON `mcpServers.ctx`, `command`, `args` | Prefer Windsurf settings UI; ctx parses JSON and upserts only `mcpServers.ctx` | `~/.codeium` | [Windsurf/Devin MCP](https://docs.devin.ai/windsurf/plugins/cascade/mcp), [ctx providers](provider-support.md) | Admin allowlists or registries can block non-approved MCP servers. |
| Zed | Yes, implemented | `~/.config/zed/settings.json` | `.zed/settings.json` | JSON `context_servers.ctx`, `command`, `args` | Prefer Zed action/UI when interactive; ctx parses JSON and upserts only `context_servers.ctx` | `~/.config/zed` | [Zed MCP](https://zed.dev/docs/ai/mcp), [ctx providers](provider-support.md) | Zed uses `context_servers`, not `mcpServers`. |
| Copilot CLI | Yes, implemented | `~/.copilot/mcp-config.json`, or `COPILOT_HOME/mcp-config.json` | Not documented for CLI | JSON `mcpServers.ctx`, `type: "local"`, `command`, `args`, `tools` | Prefer `copilot mcp add/remove`; ctx parses JSON and upserts only `mcpServers.ctx` | `COPILOT_HOME` or `~/.copilot` | [Copilot CLI MCP](https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers), [Copilot CLI overview](https://docs.github.com/en/copilot/how-tos/copilot-cli/use-copilot-cli/overview), [ctx providers](provider-support.md) | GitHub also has separate IDE and cloud-agent MCP config surfaces; ctx targets Copilot CLI only. |
| Factory AI Droid | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | ctx Factory AI Droid session defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Qwen Code | Yes, implemented | `~/.qwen/settings.json` | `.qwen/settings.json` | JSON `mcpServers.ctx`, `command`, `args` | Prefer `qwen mcp add/remove` when available; ctx parses JSON and upserts only `mcpServers.ctx` | `~/.qwen` | [Qwen Code MCP](https://qwenlm.github.io/qwen-code-docs/en/users/features/mcp/), [ctx providers](provider-support.md) | Qwen Code is Gemini-derived but uses its own settings path. |
| Kimi Code CLI | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | ctx Kimi Code CLI wire JSONL defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Auggie | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | ctx Auggie session defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Junie | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | ctx Junie session event defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Firebender | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | ctx Firebender chat history defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| ForgeCode | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | `FORGE_CONFIG` or ctx ForgeCode defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Deep Agents | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | ctx Deep Agents session defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Mistral Vibe | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | `VIBE_HOME` or ctx Mistral Vibe defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Mux | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | ctx Mux session defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Rovo Dev | Unknown | Unknown | Unknown | Unknown | Do not write MCP config | ctx Rovo Dev session defaults | [ctx providers](provider-support.md) | No current official MCP config source verified. |
| Cline | Yes, implemented | `~/.cline/mcp.json` | Not documented for CLI | JSON `mcpServers.ctx`, `command`, `args`, `disabled`, `autoApprove` | Prefer `cline mcp` wizard; ctx parses JSON and upserts only `mcpServers.ctx` | `~/.cline` | [Cline MCP](https://docs.cline.bot/mcp/mcp-overview), [ctx providers](provider-support.md) | Cline IDE extensions can use a separate settings JSON opened from the extension UI. |
| Roo Code | Project-only implemented | Global `mcp_settings.json` opened by UI, exact path varies by host | `.roo/mcp.json` | JSON `mcpServers.ctx`, `command`, `args` | Prefer Roo UI; ctx supports `--project` and refuses global writes | `.roo/mcp.json` or `~/.roo` | [Roo Code MCP](https://roocodeinc.github.io/Roo-Code/features/mcp/using-mcp-in-roo/), [ctx providers](provider-support.md) | Project MCP config is shareable and should be reviewed before commit. |

## Manual Advanced Snippets

These snippets show the `ctx` server entry in the major config shapes. They are
for manual review, recovery, or clients that ctx does not write automatically.

Codex TOML:

```toml
[mcp_servers.ctx]
command = "ctx"
args = ["mcp", "serve"]
```

Most JSON clients:

```json
{
  "mcpServers": {
    "ctx": {
      "command": "ctx",
      "args": ["mcp", "serve"]
    }
  }
}
```

Claude Code and Cursor stdio JSON:

```json
{
  "mcpServers": {
    "ctx": {
      "type": "stdio",
      "command": "ctx",
      "args": ["mcp", "serve"]
    }
  }
}
```

OpenCode:

```json
{
  "mcp": {
    "ctx": {
      "type": "local",
      "command": ["ctx", "mcp", "serve"],
      "enabled": true
    }
  }
}
```

Goose:

```yaml
extensions:
  ctx:
    enabled: true
    name: ctx
    display_name: ctx
    type: stdio
    cmd: ctx
    args:
      - mcp
      - serve
    timeout: 300
```

Continue:

```yaml
name: ctx MCP
version: 0.0.1
schema: v1
mcpServers:
  - name: ctx
    type: stdio
    command: ctx
    args:
      - mcp
      - serve
```

Zed:

```json
{
  "context_servers": {
    "ctx": {
      "command": "ctx",
      "args": ["mcp", "serve"]
    }
  }
}
```

Claude Desktop, which is not a ctx coding-agent history provider, uses the
same `mcpServers` JSON shape in its desktop app config. See the official MCP
Claude Desktop local-server guide for current platform paths.
