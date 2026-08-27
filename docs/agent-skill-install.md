# Agent Plugin and Skill Install

The ctx plugin, default skill, and supported native command are all named
`ctx`. The plugin bundles the same skill that the native ctx CLI installs into
agent skill directories.

Use the default skill when an agent should search past local agent
sessions or use ctx pro to trace a line, file, commit, or PR back to the session
that produced it.

## Native ctx CLI

The hosted ctx installer runs the managed skill install by default:

```bash
curl -fsSL https://ctx.rs/install | sh
```

Use the native ctx CLI directly after a source or package-manager install, when
installer setup was skipped, or when refreshing the skill after an upgrade:

```bash
ctx integrations install skills
```

By default this opens a small picker in an interactive terminal, with the
universal `~/.agents/skills/ctx` location selected plus detected
agent-specific folders for clients that need them. Safe existing `ctx` or
managed `ctx-agent-history-search` copies in recognized agent folders are also
preselected. In non-interactive runs, ctx maintains those safe existing copies
alongside the universal folder and any detected agent-specific folders that are
needed. Once a picker selection is submitted, or when `--agent` or
`--all-agents` is used, only the selected folders are managed.

Install into explicit global agent skill folders with:

```bash
ctx integrations install skills --agent codex
ctx integrations install skills --agent grok-build
ctx integrations install skills --agent claude-code --agent cursor --agent mimocode
ctx integrations install skills --all-agents
```

Grok Build uses a native skill directory. An explicit global install writes
`ctx` under absolute `GROK_HOME/skills` when `GROK_HOME` is set, or
`~/.grok/skills` otherwise. `GROK_HOME` must be absolute. Project installs use
`.grok/skills/ctx`. Grok Build also scans the universal `.agents/skills`
location, so automatic setup does not create a second native copy.

MiMo Code reads the universal `.agents/skills` location. An explicit global
MiMo install writes to the MiMo config skill directory, honoring
`MIMOCODE_CONFIG_DIR`, absolute `MIMOCODE_HOME/config`, or
`$XDG_CONFIG_HOME/mimocode`.

Use project scope for repository-local skill folders:

```bash
ctx integrations install skills --project
ctx integrations install skills --project --agent claude-code
```

Check installed state with:

```bash
ctx integrations status skills
ctx integrations status skills --agent codex --format json
ctx integrations status skills --agent grok-build
```

`status` reports `current`, `stale`, `modified`, or `missing`. The installer
writes `.ctx-skill.json` beside `SKILL.md` so ctx can distinguish managed copies
from local edits. Without target flags, status uses the same default maintenance
set as install, including safe existing copies in recognized folders.

The 1.0 installer performs a one-way migration from the former
`ctx-agent-history-search` directory. It removes a recognized managed copy and
installs `ctx`; it never leaves both skills active. A locally modified former
skill is preserved unless `--force` is provided.

Installer flags mirror the direct CLI controls:

```bash
curl -fsSL https://ctx.rs/install | sh -s -- --no-skill
curl -fsSL https://ctx.rs/install | sh -s -- --skill-agent codex --skill-agent claude-code
curl -fsSL https://ctx.rs/install | sh -s -- --all-skill-agents
```

`--no-setup` is install-only mode and skips skill setup and history indexing
unless a skill option is passed explicitly.

## Portable Agent Plugin

The self-contained plugin lives at `plugins/ctx`. Its root `plugin.json`
conforms to Agent Plugins 1.0.0. Portable clients discover the default skill at
`skills/ctx/SKILL.md` inside that plugin.

Agent Plugins v1 standardizes skills and optional MCP configuration, but not
slash commands or marketplace distribution. The repository therefore retains
native Codex, Claude Code, and Cursor manifests and catalogs alongside the
portable manifest.

The plugin requires an installed and initialized ctx CLI. Installing the plugin
does not install the CLI or enable the paid ctx pro add-on.

This is a package rename, not an alias. Before installing `ctx`, remove any
installed plugin named `ctx-agent-history-search`; otherwise the client can load
both packages. The managed ctx CLI migrates direct skill-folder installs, but it
cannot remove packages owned by a client's plugin manager.

## Codex and ChatGPT

This repository includes a Codex marketplace catalog at
`.agents/plugins/marketplace.json` and native metadata at
`plugins/ctx/.codex-plugin/plugin.json`.

For an unreleased branch or tag, add the marketplace with an explicit ref:

```bash
codex plugin marketplace add ctxrs/ctx --ref <branch-or-tag>
```

After release on the default branch, the ref can be omitted:

```bash
codex plugin marketplace add ctxrs/ctx
```

If upgrading from the former package, remove it first:

```bash
codex plugin remove ctx-agent-history-search@ctx
```

Then open `/plugins` and install `ctx`.

## Claude Code

This repository includes a Claude Code marketplace catalog at
`.claude-plugin/marketplace.json`.

For local testing from a checkout:

```text
/plugin marketplace add <path-to-ctx-checkout>
/plugin uninstall ctx-agent-history-search@ctx
/plugin install ctx@ctx
```

For GitHub distribution after release:

```text
/plugin marketplace add ctxrs/ctx
/plugin uninstall ctx-agent-history-search@ctx
/plugin install ctx@ctx
```

## Cursor

This repository includes a Cursor plugin manifest at
`plugins/ctx/.cursor-plugin/plugin.json` and a root
`.cursor-plugin/marketplace.json` catalog for submission.

If upgrading, uninstall `ctx-agent-history-search` in Cursor's plugin settings
first. After marketplace acceptance, install `ctx` from Cursor Marketplace or
with `/add-plugin`.

## Direct Skill Folder

For clients that support raw Agent Skills, install or copy:

```text
skills/ctx
```

The plugin copy under `plugins/ctx/skills/ctx` is self-contained so marketplace
installs do not depend on files outside the plugin directory. Keep the
standalone and plugin copies synchronized with:

```bash
scripts/sync-plugin-skills.sh --check
scripts/sync-plugin-skills.sh --write
```

The plugin includes a `/ctx` command for native clients that support bundled
commands. It is a thin entry point that delegates to the `ctx` skill instead of
duplicating its workflow.

## Other Slash Command Entry Points

Many agent clients expose installed skills directly through slash-style
commands. For those clients, installing the ctx skill is the correct
integration. Use the separate slash-command installer only for providers with
a documented command-file location:

```bash
ctx integrations install slash-commands --agent opencode
ctx integrations install slash-commands --agent mimocode
ctx integrations install slash-commands --agent gemini-cli
ctx integrations install slash-commands --agent qwen-code
```

See `ctx docs show slash-command-integrations` for the full provider matrix.
