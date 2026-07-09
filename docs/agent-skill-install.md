# Agent Skill Install

The ctx agent skill is named `ctx-agent-history-search`.

The name follows the public product language: ctx is local agent history search,
not a model memory or graph database. Use the skill when an agent should query
past local coding-agent sessions before starting work.

## Native ctx CLI

The hosted ctx installer runs the native skill install by default:

```bash
curl -fsSL https://ctx.rs/install | sh
```

Use the native ctx CLI directly when you installed ctx from source or a package
manager, when you skipped installer setup, or when you want to refresh the skill
after upgrading:

```bash
ctx integrations install skills
```

By default this opens a small picker when run in an interactive terminal, with
the universal `~/.agents/skills/ctx-agent-history-search` location selected
plus detected agent-specific folders for tools that need them. In
non-interactive runs, ctx installs to the universal folder and also writes
detected agent-specific folders, such as Claude Code, only when ctx sees
evidence that the agent is installed. Re-run the same command whenever you
upgrade ctx or want to refresh the installed skill instructions.

Install into specific agent skill folders when an agent does not read the
universal `.agents/skills` location:

```bash
ctx integrations install skills --agent codex
ctx integrations install skills --agent claude-code --agent cursor --agent mimocode
ctx integrations install skills --all-agents
```

Use project scope when you want a repository-local skill folder:

```bash
ctx integrations install skills --project
ctx integrations install skills --project --agent claude-code
```

Check installed state with:

```bash
ctx integrations status skills
ctx integrations status skills --agent codex --json
```

`status` reports `current`, `stale`, `modified`, or `missing` for the bundled
`ctx-agent-history-search` skill. The installer writes a small
`.ctx-skill.json` metadata file beside `SKILL.md` so ctx can tell stale bundled
copies from local edits.

`ctx integrations install skills` refreshes stale bundled copies automatically,
but it does not overwrite locally modified skill files unless you pass
`--force`.

Installer flags mirror the direct CLI controls:

```bash
curl -fsSL https://ctx.rs/install | sh -s -- --no-skill
curl -fsSL https://ctx.rs/install | sh -s -- --skill-agent codex --skill-agent claude-code
curl -fsSL https://ctx.rs/install | sh -s -- --all-skill-agents
```

`--no-setup` is install-only mode and skips both skill setup and history
indexing unless you pass a skill option explicitly.

## Codex

This repository includes a Codex marketplace catalog at
`.agents/plugins/marketplace.json` and a plugin at
`plugins/ctx-agent-history-search`.

For an unreleased branch or tag, add the marketplace with an explicit ref:

```bash
codex plugin marketplace add ctxrs/ctx --ref ctx/search-sdlc-maturity
```

After the branch is released on the default branch, the ref can be omitted:

```bash
codex plugin marketplace add ctxrs/ctx
```

Then open `/plugins` and install `ctx-agent-history-search`.

## Claude Code

This repository includes a Claude Code marketplace catalog at
`.claude-plugin/marketplace.json`.

For local testing from a checkout:

```text
/plugin marketplace add <path-to-ctx-checkout>
/plugin install ctx-agent-history-search@ctx
```

For GitHub distribution after release:

```text
/plugin marketplace add ctxrs/ctx
/plugin install ctx-agent-history-search@ctx
```

## Cursor

This repository includes a Cursor plugin manifest at
`plugins/ctx-agent-history-search/.cursor-plugin/plugin.json` and a root
`.cursor-plugin/marketplace.json` catalog for submission.

After marketplace acceptance, install it from Cursor Marketplace or with
`/add-plugin` using the name `ctx-agent-history-search`.

## Direct Skill Folder

For agents that support raw Agent Skills, install or copy:

```text
skills/ctx-agent-history-search
```

The plugin copy under `plugins/ctx-agent-history-search/skills/` is intentionally
self-contained so marketplace installs do not depend on files outside the
plugin directory. Keep the standalone and plugin copies in sync with:

```bash
scripts/sync-plugin-skills.sh --check
scripts/sync-plugin-skills.sh --write
```

The plugin also includes a `/ctx-history` command. The command is a thin entry
point that delegates to the `ctx-agent-history-search` skill instead of
duplicating the full workflow instructions.

## Slash Command Entry Points

Many agent harnesses now expose skills directly through slash-style commands.
For those providers, installing the ctx skill is the right integration. Codex
uses `/skills` or skill references, and Claude Code and Cursor expose skills as
slash commands.

Use the separate slash-command installer only for providers that still have a
documented command-file location:

```bash
ctx integrations install slash-commands --agent opencode
ctx integrations install slash-commands --agent mimocode
ctx integrations install slash-commands --agent gemini-cli
ctx integrations install slash-commands --agent qwen-code
ctx integrations install slash-commands --agent windsurf
```

See `ctx docs show slash-command-integrations` for the full provider matrix.
