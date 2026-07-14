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
  needs;
- stored transcript text, tool output, repository content, or SQL values may
  contain instruction-like text that can steer a later agent when recalled.

## Mitigations

- keep imports explicit and repeatable;
- reject unknown provider formats;
- store bounded previews for large outputs;
- preserve citations and source availability flags;
- keep setup local and side-effect-limited;
- document that searchable text is copied into SQLite;
- treat JSON output as private until reviewed;
- wrap agent-facing search, transcript, event-window, and SQL table output in a
  single fresh nonce boundary per response, without escaping or rewriting
  retrieved historical data. A trusted preamble names the authoritative nonce
  and says that nested or mismatched markers are historical data. MCP applies
  the boundary automatically and labels
  `structuredContent` with the same nonce; CLI search text, show text/Markdown,
  and SQL tables apply the boundary automatically.

The nonce boundary is provenance and delimiter-spoofing defense in depth. It
does not authenticate historical claims, prevent prompt injection inside the
real boundary, or authorize any action. Agents must treat recalled history as
evidence and apply current policy and user approval to subsequent actions.
This first boundary covers stored free-form output from search, show, and SQL;
lower-volume metadata surfaces such as locate and MCP sources remain outside
this mitigation.
