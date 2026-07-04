# Search

`ctx search` finds matching indexed history. Default results are session-diverse:
ctx shows the strongest matching span from each session, then lets you drill
into dense event-level results when needed. By default it first performs a quiet
best-effort refresh of discovered native provider sources and enabled auto
history-source plugins, then queries the local SQLite store.

## Search

Examples:

```bash
ctx search "build failure"
ctx search "sqlite storage" --provider codex
ctx search "retry handling" --workspace checkout --since 60d
ctx search "tool output" --event-type tool_output
ctx search --file crates/foo/src/lib.rs
ctx search "token budget" --refresh off
ctx search "signed metadata" --term checksum --term release
ctx search "token budget" --limit 5
ctx search "token budget" --session <ctx-session-id>
ctx search "review findings" --include-subagents
ctx search "this current task" --include-current-session
```

A result can include:

- `ctx_event_id`, the ctx-owned event ID for event hits;
- `ctx_session_id`, the ctx-owned session ID when known;
- `provider_session_id`, the provider-owned session ID when known;
- title or event label;
- snippet with truncation where needed;
- rank, result scope, and match reasons;
- session importance and more-matches count for default session results;
- provider;
- event sequence;
- timestamp;
- working directory when known;
- source path and cursor when available;
- source availability flag when known;
- citations;
- `suggested_next_commands`, copyable commands for `ctx show`, `ctx locate`,
  and scoped follow-up searches.

Search result IDs are ctx-owned. Commands accept full ctx IDs or unambiguous
ctx ID prefixes of at least eight hex characters. Provider-owned IDs are
exposed as metadata so humans can recognize the original provider session, but
they are not positional lookup IDs. Provider-owned lookup must be explicit, for
example `--provider codex --provider-session <provider-session-id>` on commands
that support it.

## Filters

Search filters narrow both human output and JSON:

- `--provider codex|pi|claude|opencode|kilo|kiro-cli|crush|goose|dexto|openclaw|hermes|nanoclaw|astrbot|shelley|continue|openhands|antigravity|gemini|cursor|zed|copilot-cli|factory-ai-droid|qwen-code|kimi-code-cli|autohand-code|iflow-cli|codebuddy|cline|roo`;
- `--history-source <plugin/source-or-provider_key/source_id>`, for custom
  history imports;
- `--provider-key <key>`, `--source-id <id>`, and
  `--source-format <format>`, for exact custom history source filters;
- `--workspace <name-or-path>`, substring match over stored workspace, cwd,
  source path, or repository-name text;
- `--since <rfc3339-or-days>d`;
- `--event-type <event-type>`, one of `message`, `tool_call`, `tool_output`,
  `command_started`, `command_output`, `command_finished`, `file_touched`,
  `vcs_change`, `artifact`, `summary`, or `notice`;
- `--file <path>`, indexed touched-file path metadata, not the current
  filesystem;
- `--session <ctx-session-id-or-prefix>`;
- `--term <query-or-keyword>`, repeatable broadening terms merged with OR-style
  semantics, not required terms;
- `--events`;
- `--include-subagents`;
- `--limit <n>`;
- `--refresh auto|off|strict`;
- `--include-current-session`.

CLI provider filters use the kebab-case names above. JSON output and stable SQL
views use provider IDs in ctx output; multiword provider IDs may be snake_case,
such as `copilot_cli`, `factory_ai_droid`, `qwen_code`, `kimi_code_cli`,
`autohand_code`, or `iflow_cli`, while compact IDs such as `openclaw`,
`nanoclaw`, `astrbot`, `shelley`, `continue`, and `openhands` stay compact.

`--since` accepts RFC 3339 timestamps such as `2026-06-01T00:00:00Z` or a day
window such as `30d`.

`--file <path>` filters by normalized `files_touched` metadata when provider
transcripts expose touched paths. Use it without a query to list indexed events
for a file, or combine it with query terms to find sessions that both mention a
topic and touched that path. It searches paths recorded during import; it does
not inspect the current filesystem.

Search requires a non-empty query, at least one non-empty `--term`, or
`--file <path>`. Provider, workspace, time, session, event, source, and result
flags only narrow an actual search; by themselves they do not browse recent
history.

The default searches primary-agent sessions so human intent and decisions stay
prominent. Use `--include-subagents` when you want implementation details, code
review notes, test output, or failure analysis from subagent sessions too.

`--limit` defaults to `20` and is capped at `200`.

Default search returns diverse session-level results. Use
`--session <ctx-session-id>` after a default search has identified a session to
inspect; scoped session search returns dense event hits. Use `--events` without
`--session` when you want dense event hits across sessions.

When ctx is run from Codex and `CODEX_THREAD_ID` is available, search excludes
the active Codex session tree by default so the current prompt and its subagent
work do not dominate history research. Use `--include-current-session` when you
are intentionally looking for material from the active session tree.

`--refresh` defaults to `auto`. `auto` attempts a best-effort pre-search import
of discovered native provider sources and enabled auto history-source plugins,
then serves the existing index if that refresh fails. On large discovered
sources or already-cataloged indexes, `auto` serves current results without a
foreground catch-up scan; use `--refresh strict` or `ctx import --all` when you
need a full catch-up before querying. `off` skips the pre-search refresh and
never runs plugin commands. `strict` fails the search if the refresh cannot run
or import successfully. Preview native sources such as NanoClaw and AstrBot,
plus search-only sources without native import support, are searched from the
existing index until they are explicitly imported through a supported path.

Use `--refresh off` for a strictly read-only search over the existing ctx index.
This avoids provider imports, plugin execution, and updates to the ctx SQLite
store.

## History Reports

Use the agent history-search skill when a topic needs a cited report instead of
a ranked hit list. The skill should run several `ctx search` queries, inspect the
best cited events or sessions with `ctx show`, and write the report itself. ctx
only retrieves indexed local evidence; it does not synthesize conclusions.

## Machine Output

Use default text output for agent reading. Use `ctx search <query> --json` or a
term/file search with `--json` for scripts, `jq`, or exact field extraction.
JSON results include the same result metadata and citations as the human output,
plus a top-level `freshness` object
describing the pre-search refresh mode and outcome. A citation with
`source_exists: false` means ctx can return indexed text, but the raw provider
file was not available at the stored path when the result was built.

Search output is local/private by default and is not redacted for sharing.
Review and redact copied snippets, JSON, or transcripts before sending them
outside the machine.
