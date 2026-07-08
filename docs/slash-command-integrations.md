# Slash Command Integrations

This page records the ctx slash-command decision for the supported agent
harnesses listed in [Provider Support](provider-support.md).

Current rule: prefer the bundled `ctx-agent-history-search` skill when the
provider exposes skills as command-like invocations. Write a separate
`/ctx-history` command file only when the provider has a current documented
file location, file format, invocation syntax, and argument behavior that ctx
can manage idempotently without editing broad user config.

The implemented command is a thin entry point. It tells the agent to use local
`ctx` search, inspect cited sessions or events, and answer with ctx citations.
It does not install hooks.

## Implemented Writers

`ctx integrations install slash-commands` currently writes these separate
entry points:

| Agent | Global path | Project path | Format | Invocation | Detection |
| --- | --- | --- | --- | --- | --- |
| OpenCode | `$XDG_CONFIG_HOME/opencode/commands/ctx-history.md` | `.opencode/commands/ctx-history.md` | Markdown with YAML frontmatter | `/ctx-history <query>` | `$XDG_CONFIG_HOME/opencode` exists |
| Gemini CLI | `~/.gemini/commands/ctx-history.toml` | `.gemini/commands/ctx-history.toml` | TOML custom command | `/ctx-history <query>` | `~/.gemini` exists |
| Qwen Code | `~/.qwen/commands/ctx-history.md` | `.qwen/commands/ctx-history.md` | Markdown with YAML frontmatter and `{{args}}` | `/ctx-history <query>` | `~/.qwen` exists |
| Windsurf | `~/.codeium/windsurf/global_workflows/ctx-history.md` | `.windsurf/workflows/ctx-history.md` | Markdown workflow | `/ctx-history` | `~/.codeium/windsurf` exists |

Each writer stores `.ctx-slash-commands.json` beside the generated command. A
reinstall refreshes stale ctx-owned files, leaves locally modified files alone,
and requires `--force` to replace a modified file. A future remove operation
should only delete the exact managed command file when the current hash matches
the metadata or the user explicitly requests a forced remove.

Skill-only providers are intentionally not written through this command. Use
`ctx skill install` for the providers already supported by the skill installer.

## Automated Coverage

The Bazel target `//:slash_command_e2e` runs hermetic fake-harness tests for the
implemented file writers. Those tests execute the real ctx installer with
temporary home and config directories, discover the generated files from the
documented provider paths, parse the provider command formats, and substitute a
multi-word query through the provider argument token where the provider supports
one. Windsurf is covered as a workflow-readiness parser because its invocation
and reload path is UI/manual.

Optional live-harness smoke tests should stay outside the default Bazel gate.
They require installed third-party CLIs or desktop UI state and, for some
providers, interactive command reload or approval.

## Support Matrix

Source links were checked on July 7, 2026.

| Harness | Support | Global/user path | Project path | Format | Invocation | Safe strategy and detection | Sources | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Codex | Skill-only | `~/.agents/skills`, `~/.codex/skills`, `/etc/codex/skills` | `.agents/skills` | `SKILL.md` | `/skills` picker or `$skill` reference | Covered by `ctx skill install --agent codex`; do not write deprecated `~/.codex/prompts` | [Codex skills](https://developers.openai.com/codex/skills), [Codex custom prompts](https://developers.openai.com/codex/custom-prompts) | Custom prompts are documented as deprecated in favor of skills. |
| Claude Code | Skill-only | `~/.claude/skills` | `.claude/skills` | `SKILL.md` | `/<skill-name>` | Covered by `ctx skill install --agent claude-code` | [Claude skills](https://code.claude.com/docs/en/skills), [Claude slash commands](https://code.claude.com/docs/en/agent-sdk/slash-commands) | Claude says custom commands have merged into skills; legacy `.claude/commands` still works but is not preferred for ctx. |
| Cursor | Skill-only | `~/.cursor/skills`, `~/.agents/skills` | `.cursor/skills`, `.agents/skills` | `SKILL.md` | `/<skill-name>` | Covered by `ctx skill install --agent cursor`; no `.cursor/commands` writer until official layout docs are stable | [Cursor skills](https://cursor.com/docs/skills.md), [Cursor CLI slash commands](https://cursor.com/docs/cli/reference/slash-commands.md) | Cursor plugins can expose commands; the raw command-file surface is treated as migration input, not the ctx install target. |
| OpenCode | Yes, separate | `$XDG_CONFIG_HOME/opencode/commands` | `.opencode/commands` | Markdown with YAML frontmatter, `$ARGUMENTS`, `$1` | `/ctx-history <query>` | Implemented with metadata hash and detection by config dir | [OpenCode commands](https://opencode.ai/docs/commands/) | Separate command file is clearly documented and safe to manage. |
| Gemini CLI | Yes, separate | `~/.gemini/commands` | `.gemini/commands` | TOML command, `{{args}}` | `/ctx-history <query>` | Implemented with metadata hash and detection by `~/.gemini` | [Gemini custom commands](https://geminicli.com/docs/cli/custom-commands/), [Gemini commands reference](https://geminicli.com/docs/reference/commands/) | Gemini still documents TOML custom commands and `/commands reload`. |
| Qwen Code | Yes, separate | `~/.qwen/commands` | `.qwen/commands` | Markdown with YAML frontmatter, `{{args}}` | `/ctx-history <query>` | Implemented with metadata hash and detection by `~/.qwen` | [Qwen commands](https://qwenlm.github.io/qwen-code-docs/en/users/features/commands/) | Qwen documents Markdown commands as recommended and TOML as deprecated. |
| Windsurf | Yes, separate workflow | `~/.codeium/windsurf/global_workflows` | `.windsurf/workflows` | Markdown workflow | `/ctx-history` | Implemented with metadata hash and detection by Windsurf config dir | [Windsurf workflows](https://docs.devin.ai/desktop/cascade/workflows), [Windsurf skills](https://docs.devin.ai/desktop/cascade/skills) | Workflows are manual; skills are a separate progressive-disclosure mechanism. |
| GitHub Copilot CLI | Skill-only | `~/.copilot/skills`, `~/.agents/skills` | `.github/skills`, `.claude/skills`, `.agents/skills` | `SKILL.md` | `/<skill-name>`, `/skills list` | Covered by `ctx skill install --agent github-copilot` | [Copilot CLI skills](https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-skills), [Copilot CLI custom agents](https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/create-custom-agents-for-cli) | No separate prompt-command file writer found. |
| Pi | Skill-only | `~/.pi/agent/skills` | `.pi/skills` | Skill directory | `/skill:<name> <args>` | Covered by `ctx skill install --agent pi`; invocation is not `/ctx-history` | [Pi skills](https://pi.dev/docs/latest/skills), [Pi usage](https://pi.dev/docs/latest/usage) | Pi skill commands use `/skill:` rather than a direct command alias. |
| Goose | Manual-only | `~/.config/goose/config.yaml` | Not documented as a file drop-in | YAML `slash_commands` mapping to recipes | `/<name>` | Do not edit YAML yet; a safe writer needs YAML round-tripping and recipe ownership | [Goose custom slash commands](https://goose-docs.ai/docs/guides/context-engineering/slash-commands/) | Goose commands are shortcuts to recipes and accept at most one parameter. |
| Continue | Manual-only | Continue assistant config | Continue assistant config | YAML prompt entries with `invokable: true` | `/<prompt-name>` | Do not edit YAML yet; a safe writer needs config discovery and round-tripping | [Continue CLI](https://docs.continue.dev/guides/cli), [Continue config reference](https://docs.continue.dev/reference) | Continue prompts are invokable slash prompts, but the command is a config entry, not a standalone file in a stable command dir. |
| Kiro CLI | Skill-only | `~/.kiro/skills` | `.kiro/skills` | `SKILL.md` | `/<skill-name>` | No separate writer; install as a skill when ctx adds a Kiro skill target | [Kiro CLI skills](https://kiro.dev/docs/cli/skills/), [Kiro slash commands](https://kiro.dev/docs/cli/reference/slash-commands/) | Kiro documents skills as direct slash commands and supports `$ARGUMENTS`. |
| Zed | Skill-only | `~/.agents/skills` | `.agents/skills` | `SKILL.md` | `/` skill picker or `@skill` | No separate writer; use skills | [Zed skills](https://zed.dev/docs/ai/skills), [Zed external agents](https://zed.dev/docs/ai/external-agents) | Zed loads skills from `.agents` and notes external agents may have their own systems. |
| Factory AI Droid | Skill-only | `~/.factory/skills` | `.factory/skills` | `SKILL.md` or `skill.mdx` | `/<skill-name>` | No command writer; docs say commands are superseded by skills | [Factory skills](https://docs.factory.ai/cli/configuration/skills), [Factory custom commands](https://docs.factory.ai/cli/configuration/custom-slash-commands) | Legacy `.factory/commands` still works, but new ctx installs should be skills. |
| Mistral Vibe | Skill-only | `~/.vibe/skills`, `~/.agents/skills` | `.vibe/skills`, `.agents/skills` | `SKILL.md` with `user-invocable: true` | `/<skill-name>` | No separate writer; use skills | [Mistral Vibe CLI](https://docs.mistral.ai/vibe/code/cli/work-with-cli), [mistral-vibe README](https://github.com/mistralai/mistral-vibe) | Custom slash commands are defined through skills. |
| Kimi Code CLI | Skill-only | Kimi, Claude, Codex, and generic skill dirs | `.kimi/skills`, `.claude/skills`, `.codex/skills`, `.agents/skills` | `SKILL.md` or flat `.md` skill | `/skill:<name> <args>` | No separate writer; use skills | [Kimi skills](https://moonshotai.github.io/kimi-cli/en/customization/skills.html), [Kimi slash commands](https://moonshotai.github.io/kimi-cli/en/reference/slash-commands.html) | Kimi uses `/skill:` for explicit skill loading. |
| Cline | Skill-only | `~/.cline/skills` | `.cline/skills`, `.clinerules/skills`, `.claude/skills` | `SKILL.md` | `/<skill-name>` | No separate writer; use skills | [Cline skills](https://docs.cline.bot/customization/skills) | Cline skills are explicitly invokable from slash suggestions. |
| Roo Code | Yes, deferred | `~/.roo/commands` | `.roo/commands` | Markdown with optional frontmatter | `/<command-name>` | No ctx writer yet; command paths are clear but argument substitution for a portable `/ctx-history <query>` wrapper is not documented | [Roo slash commands](https://roocodeinc.github.io/Roo-Code/features/slash-commands/), [Roo skills](https://roocodeinc.github.io/Roo-Code/features/skills/) | Roo also supports skills, but its docs distinguish skills from slash commands. |
| Kilo Code | Yes, deferred | `~/.config/kilo/commands` | `.kilo/commands` | Markdown with optional frontmatter | `/<command-name>` | No ctx writer yet; command path is clear but argument interpolation is not documented | [Kilo workflows](https://kilo.ai/docs/customize/workflows), [Kilo CLI](https://kilo.ai/docs/code-with-ai/platforms/cli) | Kilo calls workflows slash commands. |
| Auggie | Yes, deferred | `~/.augment/commands` | `.augment/commands` | Markdown with optional frontmatter | `/<command-name> [arguments]` | No ctx writer yet; Auggie also supports skills and the alias decision should be made with skill install support | [Auggie custom commands](https://docs.augmentcode.com/cli/custom-commands), [Auggie skills](https://docs.augmentcode.com/cli/skills) | Commands and skills are separate but share the slash surface. |
| Junie | Yes, manual/deferred | `~/.junie/commands` | `.junie/commands` | Markdown with YAML frontmatter | `/<command> name=value` | No ctx writer; Junie requires named arguments declared in the prompt | [Junie custom slash commands](https://junie.jetbrains.com/docs/custom-slash-commands.html) | The generic `/ctx-history <query>` shape does not map cleanly to Junie's named-argument command model. |
| Tabnine CLI | Unknown for file writer | Settings UI, not a documented file writer | Settings UI | UI-defined custom command | `/custom-command` | Do not write; no stable command file contract found | [Tabnine CLI commands](https://docs.tabnine.com/main/getting-started/tabnine-cli/features/commands), [Tabnine chat custom commands](https://docs.tabnine.com/main/getting-started/tabnine-chat/interact), [ctx provider list](provider-support.md) | Tabnine Chat supports user-defined quick actions in UI; CLI docs found only built-in commands. |
| Warp | Unknown for file writer | Warp Drive saved prompts | Warp Drive saved prompts | Saved prompt | Slash menu | Do not write; no local file contract found | [Warp slash commands](https://docs.warp.dev/agent-platform/capabilities/slash-commands/), [ctx provider list](provider-support.md) | Slash commands expose built-ins and saved prompts, not a documented command file path. |
| Rovo Dev | Unknown for custom writer | n/a | n/a | Built-in commands | `/help` and built-ins | Do not write | [Rovo Dev commands](https://support.atlassian.com/rovo/docs/rovo-dev-cli-commands/), [ctx provider list](provider-support.md) | Current public docs list built-in commands, not custom command files. |
| Antigravity | Skill-only | `~/.gemini/antigravity/skills`, `~/.gemini/antigravity-cli/skills` | `.agents/skills` | `SKILL.md` | Skill invocation | Covered by `ctx skill install --agent antigravity` or `--agent antigravity-cli` | [ctx skill installer](agent-skill-install.md), [Gemini skills](https://geminicli.com/docs/cli/skills/) | Treats Antigravity as a Gemini-family skill target; no separate slash docs found. |
| Qoder | Unknown | n/a | n/a | n/a | n/a | Do not write | [Qoder skills](https://docs.qoder.com/extensions/skills), [ctx provider list](provider-support.md) | Public docs found skills UI, not a stable slash-command file path. |
| Lingma | Unknown | n/a | n/a | n/a | n/a | Do not write | [ctx provider list](provider-support.md) | No authoritative command or skill file contract found during this research pass. |
| CodeBuddy | Unknown | n/a | n/a | n/a | n/a | Do not write | [ctx provider list](provider-support.md) | No authoritative command or skill file contract found during this research pass. |
| Trae | Unknown | n/a | n/a | n/a | n/a | Do not write | [ctx provider list](provider-support.md) | No authoritative command or skill file contract found during this research pass. |
| OpenClaw | Unknown | n/a | n/a | n/a | n/a | Do not write | [ctx provider list](provider-support.md) | No standalone slash-command file contract found. |
| Hermes Agent | Unknown | n/a | n/a | n/a | n/a | Do not write | [ctx provider list](provider-support.md) | No authoritative command or skill file contract found during this research pass. |
| NanoClaw | Unknown | n/a | n/a | n/a | n/a | Do not write | [ctx provider list](provider-support.md) | No authoritative command or skill file contract found during this research pass. |
| AstrBot | Unknown | n/a | n/a | n/a | n/a | Do not write | [ctx provider list](provider-support.md) | Chat-bot style slash commands are outside this local coding-agent installer until a file contract is documented. |
| Shelley | Unknown | n/a | n/a | n/a | n/a | Do not write | [ctx provider list](provider-support.md) | No authoritative command or skill file contract found during this research pass. |
| OpenHands | Unknown | n/a | n/a | n/a | n/a | Do not write | [OpenHands command reference](https://docs.openhands.dev/openhands/usage/cli/command-reference), [ctx provider list](provider-support.md) | Public docs found CLI commands and hooks, not a custom slash-command file contract. |
| Crush | Unknown | n/a | n/a | n/a | n/a | Do not write | [Crush repository](https://github.com/charmbracelet/crush), [ctx provider list](provider-support.md) | No authoritative command or skill file contract found during this research pass. |
| Firebender | Unknown | n/a | n/a | n/a | n/a | Do not write | [ctx provider list](provider-support.md) | No authoritative command or skill file contract found during this research pass. |
| ForgeCode | Unknown | n/a | n/a | n/a | n/a | Do not write | [ctx provider list](provider-support.md) | No authoritative command or skill file contract found during this research pass. |
| Deep Agents | Unknown | n/a | n/a | n/a | n/a | Do not write | [ctx provider list](provider-support.md) | No authoritative command or skill file contract found during this research pass. |
| Mux | Unknown | n/a | n/a | n/a | n/a | Do not write | [Mux AI agent guide](https://www.mux.com/docs/core/ai-agents), [ctx provider list](provider-support.md) | Mux docs are API guidance for agents, not a coding-agent command harness. |
