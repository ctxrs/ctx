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
local data and should not write outside the ctx data root. `ctx status` does
not mutate canonical history or local Pro graph data and does not initialize or
migrate local storage; Pro entitlement authorization may advance nonsecret
anti-clock-rollback security metadata. `ctx show
session --out` writes only the explicit output path requested by the user.
The default `ctx show session --content indexed` and
`ctx show event --content indexed` paths do not open provider files.
Explicit `--content complete` may read only the recorded source needed for the
selected events; it does not scan for replacement transcripts or use network
fallbacks.

Source repositories and provider homes remain outside ctx ownership. Provider
files are read as import sources, not modified.

Local Pro deletion is owned by the native CLI. Hosted uninstallers delegate to
`ctx pro uninstall` before removing the CLI binary and never delete the ctx data
root themselves. Noninteractive deletion requires an explicit `--delete-data`
or `--keep-data` choice, and `local_pro_data: "deleted"` is emitted only after
the authoritative local inventory verifies deletion. The installation's
small anti-rollback watermark may remain after deletion; it contains no graph
key, transcript content, account token, or entitlement body and is ignored by
the installed/not-installed status decision.

## Risks

- indexed prompts or output may contain secrets;
- local paths and repository names may reveal private work;
- copied JSON output may leave the machine;
- stale citations may point to moved or deleted raw files;
- unsupported provider formats may be parsed incorrectly if adapters are too
  permissive;
- compatibility JSON fields may expose more local store detail than an agent
  needs.
- complete transcript output may contain secrets absent from the bounded index,
  and MCP hosts may retain or forward it;
- a provider source can move, be replaced, or change after import.

## Mitigations

- keep imports explicit and repeatable;
- reject unknown provider formats;
- omit command/tool result bodies and retain only typed evidence plus a
  provider-independent full-body content identity;
- preserve citations and source availability flags;
- keep setup local and side-effect-limited;
- document that searchable text is copied into SQLite;
- treat JSON output as private until reviewed.
- require an explicit complete-content policy, exact native-record and prefix
  verification, bounded body/output sizes, and all-or-nothing results;
- keep typed hydration errors path- and body-free and direct diagnosis through
  `ctx locate event`.
