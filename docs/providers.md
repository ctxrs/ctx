# Providers

ctx imports existing agent history through conservative provider adapters. Each adapter makes a narrow, testable claim about the local source format it reads.

## Supported Local Imports

The public CLI supports these local-history harnesses:

Codex, Pi, Claude, OpenCode, Kilo Code, Kiro CLI, Crush, Goose, Lingma, Qoder, Warp, CodeBuddy, Trae, OpenClaw, Hermes Agent, NanoClaw, AstrBot, Shelley, Continue, OpenHands, Antigravity, Gemini, Tabnine, Cursor, Windsurf, Zed, Copilot CLI, Factory AI Droid, Qwen Code, Kimi Code CLI, Auggie, Junie, Firebender, ForgeCode, Deep Agents, Mistral Vibe, Mux, Rovo Dev, Cline, Roo Code, MiMo Code.

Use `ctx sources` for the truth on the current machine:

```bash
ctx sources
ctx sources --format json
ctx sources --all
```

Default `ctx sources` output keeps the common missing-location list compact. Use `--all` to inspect every supported provider location. The supported CLI provider names include:

```text
codex, claude, cursor, pi, opencode, github-copilot, copilot-cli, antigravity, gemini, kilo, kiro-cli, crush, goose, tabnine, windsurf, zed, factory-ai-droid, qwen-code, kimi-code-cli, auggie, junie, firebender, forgecode, deepagents, mistral-vibe, mux, rovodev, openclaw, hermes, nanoclaw, astrbot, shelley, continue, openhands, cline, roo, lingma, qoder, warp, codebuddy, trae, mimocode
```

Aliases are accepted for common naming differences, for example `claude-code`, `gemini-cli`, `github-copilot`, `droid`, `augment`, `qoder-cn`, `trae-cn`, and `roo-code`.

Custom history is separate: `ctx import --input-format ctx-history-jsonl-v1 --path <file>` reads an explicit JSONL interchange file from any exporter, and history-source plugins can stream the same format from local adapter commands.

## Location Selection

For each provider and product surface, ctx applies the provider's current
precedence and checks only the winning official root. A replacement environment
or persistent-config value replaces the lower-priority default; it is not added
beside it. Multiple roots are emitted only for current coexisting stores such as
installed clients, persisted profiles, or configured agents. See
[`provider-support-matrix.json`](provider-support-matrix.json) for every row.

One-shot flags, API constructor paths, old launch directories, container host
mounts, copies, and unreconstructible selectors are not automatic. Import one
with `ctx import --provider <provider> --path <path>`. That path bypasses
discovery precedence for the invocation, but not format checks, read bounds,
no-link checks, or read-only handling, and it is not remembered as a default.

Detected unsupported formats and sources marked `import_support: explicit` are
excluded from setup, `--all`, daemon refresh, and search refresh. Removing an
old automatic probe does not delete indexed history; a still-supported
compatible path can be selected explicitly.

Interactive discovery captures a fresh allowlisted environment and current
working directory. A long-lived daemon uses the named environment/CWD snapshot
from its launch, so restart it after changing provider root variables. A
coordinator that evaluates project-scoped providers across multiple worktrees
must call the injected discovery context once per already observed or explicitly
authorized worktree locator, then apply the normal bounded de-duplication. It
must not use provider roots as repository identity, infer worktrees from those
roots, or crawl for repositories; logical repository, checkout, and worktree
identity remain a separate activity-derived concern.

## Import Rules

Provider imports should be bounded, read-only, and tied to a documented source
format. Do not document a provider as locally importable until the CLI can
discover or parse that provider's real local history and the provider support
matrix marks the shipped path as Supported. Contributor-facing content and
fixture expectations are defined in
[`provider-import-policy.md`](provider-import-policy.md).
