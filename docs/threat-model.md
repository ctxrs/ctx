# Threat Model

The current CLI protects a local search index for developer agent history.

## Assets

- provider transcripts in provider-owned homes;
- the ctx SQLite index;
- configuration and import cursors;
- logs and diagnostic output;
- JSON and Markdown command output.

## Boundaries

ctx reads provider history and writes only to the configured ctx data root
during normal setup and import commands. Search, show, sources, and doctor read
local data and should not write outside the ctx data root. `ctx status` is
strictly read-only and does not initialize or migrate local storage. `ctx show
session --out` writes only the explicit output path requested by the user.

Source repositories and provider homes remain outside ctx ownership. Provider
files are read as import sources, not modified.

## Risks

- indexed prompts or output may contain secrets;
- local paths and repository names may reveal private work;
- copied JSON output may leave the machine;
- stale citations may point to moved or deleted raw files;
- unsupported provider formats may be parsed incorrectly if adapters are too
  permissive;
- compatibility JSON fields may expose more local store detail than an agent
  needs.

## Mitigations

- keep imports explicit and repeatable;
- reject unknown provider formats;
- store bounded previews for large outputs;
- preserve citations and source availability flags;
- keep setup local and side-effect-limited;
- document that searchable text is copied into SQLite;
- treat JSON output as private until reviewed;
- wrap recalled snippets in `[[RECALLED_DATA nonce=X]]...
  [[/RECALLED_DATA nonce=X]]` delimiters with a per-response random
  nonce. Consumers (agents, MCP clients) MUST validate that opening and
  closing nonce values match before trusting content as 'recalled data' —
  checking only for the presence of the `[[RECALLED_DATA` tag is NOT
  sufficient, since escaped snippet content may itself contain literal
  text resembling this pattern. The nonce match is the only thing that
  distinguishes a real boundary from injected text mimicking one.
- this delimiter wrapping applies only to human/agent-facing text
  rendering (CLI text output and MCP text responses). It does NOT apply
  to the `--json` output path (`SearchDto::packet`) or any SDK consumer
  reading the raw `snippet` field from JSON — that path intentionally
  returns unwrapped, unescaped text to remain machine-parseable (e.g.
  for `jq` pipelines). Any agent or integration that treats JSON-sourced
  snippet content as trusted input should apply its own sanitization,
  since it bypasses this mitigation entirely.
