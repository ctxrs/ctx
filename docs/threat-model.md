# Threat Model

The current CLI protects provider-owned source history, ctx-owned search state,
local usage aggregates, and optional Local Pro data.

## Assets

- provider transcripts in provider-owned homes;
- ctx-owned Core/Tantivy lexical generations and optional flat-F32 semantic
  generations;
- the content-free `usage.sqlite` sidecar and optional encrypted Local Pro graph;
- configuration and import cursors;
- logs and diagnostic output;
- JSON and Markdown command output.

## Boundaries

The default-enabled persistent daemon and explicit provider-source import route
write Core generations and derived state only under the configured ctx data
root. Search and MCP may send a bounded, content-free daemon wake, but query
processes do not become foreground history writers. Show, locate, sources, and
doctor do not write provider data or repositories. `ctx status` does not mutate
Core history or local Pro graph data and does not initialize or migrate local
storage; Pro entitlement authorization may advance nonsecret
anti-clock-rollback security metadata. `ctx show session --out` writes only the
explicit output path requested by the user.

Show reads complete policy-selected normalized records from the active verified
Core/Tantivy generation. It does not reopen provider history, scan for
replacement transcripts, or use a network fallback.

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
Pro initialization evidence is persisted before the first commercial vault
write. Deletion uses only opaque record IDs derived from that root's
installation identity and its recorded production/staging thumbprints; it does
not broadly enumerate another installation's credentials. This also covers
graph keys created before the first graph database file. Corrupt thumbprint
inventory fails before deletion. A bounded nonsecret root-local cleanup phase
is durably published before graph-key deletion and retained through credential
and helper verification, so retry does not depend on credential records that a
prior attempt already removed. Setup and preservation are blocked while this
phase remains; successful deletion removes it.
Users deleting the Core data root must run the identity-aware
`ctx pro uninstall --delete-data` operation first. Removing `install.json`
before native deletion can make the opaque vault records impossible to locate
and orphan credentials or graph keys.

## Risks

- searchable terms and imported prompts or output may contain secrets;
- local paths and repository names may reveal private work;
- copied JSON output may leave the machine;
- unsupported provider formats may be parsed incorrectly if adapters are too
  permissive;
- compatibility JSON fields may expose more local data-root detail than an agent
  needs.
- transcript output may contain secrets, and MCP hosts may
  retain or forward it;
- provider sources can move, be replaced, or change before a later import.

## Mitigations

- keep imports explicit and repeatable;
- reject unknown provider formats;
- keep complete policy-selected searchable content and stable provider/source
  identities in immutable Core/Tantivy generations;
- preserve stable ctx citations and provider session identity;
- keep setup local and side-effect-limited;
- open provider-native SQLite histories only through short read-only logical
  snapshots without checkpoints or writes;
- treat JSON output as private until reviewed.
- require bounded Core body/output sizes and all-or-nothing show results.
