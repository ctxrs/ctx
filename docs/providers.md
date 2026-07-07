# Providers

ctx imports existing agent history through conservative provider adapters. Each adapter makes a narrow, testable claim about the local source format it reads.

## Supported Local Imports

The public CLI supports these local-history harnesses:

Codex, Pi, Claude, OpenCode, Kilo Code, Kiro CLI, Crush, Goose, Lingma, Qoder, Warp, CodeBuddy, Trae, OpenClaw, Hermes Agent, NanoClaw, AstrBot, Shelley, Continue, OpenHands, Antigravity, Gemini, Tabnine, Cursor, Windsurf, Zed, Copilot CLI, Factory AI Droid, Qwen Code, Kimi Code CLI, Auggie, Junie, Firebender, ForgeCode, Deep Agents, Mistral Vibe, Mux, Rovo Dev, Cline, Roo Code.

Use `ctx sources` for the truth on the current machine:

```bash
ctx sources
ctx sources --json
ctx sources --all
```

Default `ctx sources` output keeps the common missing-location list compact. Use `--all` to inspect every supported provider location. The supported CLI provider names include:

```text
codex, claude, cursor, pi, opencode, github-copilot, copilot-cli, antigravity, gemini, kilo, kiro-cli, crush, goose, tabnine, windsurf, zed, factory-ai-droid, qwen-code, kimi-code-cli, auggie, junie, firebender, forgecode, deepagents, mistral-vibe, mux, rovodev, openclaw, hermes, nanoclaw, astrbot, shelley, continue, openhands, cline, roo, lingma, qoder, warp, codebuddy, trae
```

Aliases are accepted for common naming differences, for example `claude-code`, `gemini-cli`, `github-copilot`, `droid`, `augment`, `qoder-cn`, `trae-cn`, and `roo-code`.

Custom history is separate: `ctx import --format ctx-history-jsonl-v1 --path <file>` reads an explicit JSONL interchange file from any exporter, and history-source plugins can stream the same format from local adapter commands.

## Import Rules

Provider imports should be bounded, read-only, and tied to a documented source
format. Do not document a provider as locally importable until the CLI can
discover or parse that provider's real local history and the provider support
matrix marks the shipped path as Supported. Contributor-facing content and
fixture expectations are defined in
[`provider-import-policy.md`](provider-import-policy.md).
