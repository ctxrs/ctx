# Search

`ctx search` finds matching indexed sessions and events. It reads the local
SQLite store and writes nothing.

## Search

Examples:

```bash
ctx search "build failure"
ctx search "sqlite storage" --provider codex
ctx search "retry handling" --repo checkout --since 60d
ctx search "tool output" --event-type tool_output
ctx search --file crates/foo/src/lib.rs
ctx search "token budget" --limit 5 --json
```

A result can include:

- an opaque item ID usable with `ctx show`;
- title or event label;
- snippet with redaction and truncation where needed;
- rank and match reasons;
- provider;
- session ID;
- event ID or event sequence;
- timestamp;
- working directory when known;
- source path and cursor when available;
- source availability flag when known;
- citations.

## Filters

Search filters narrow both human output and JSON:

- `--provider codex|pi`;
- `--repo <name-or-path>`;
- `--since <rfc3339-or-days>d`;
- `--event-type <event-type>`;
- `--file <path>`;
- `--primary-only`;
- `--include-subagents`;
- `--limit <n>`.

`--since` accepts RFC 3339 timestamps such as `2026-06-01T00:00:00Z` or a day
window such as `30d`.

The default includes subagent material. `--primary-only` excludes it unless
`--include-subagents` is also passed.

## Machine Output

Use `ctx search --json` for agent workflows and scripts. JSON results include
the same result metadata and citations as the human output. A citation with
`source_exists: false` means ctx can return indexed text, but the raw provider
file was not available at the stored path when the result was built.

`ctx context` is deprecated. It remains available for older scripts, but it is
hidden from the default CLI help and is not part of the supported search MVP.
