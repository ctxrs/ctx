# Threat Model

The current CLI protects provider-owned source history, ctx-owned search state,
and local usage aggregates.

## Assets

- provider transcripts in provider-owned homes;
- ctx-owned Core/Tantivy lexical generations and optional flat-F32 semantic
  generations;
- the content-free `usage.sqlite` sidecar;
- configuration and import cursors;
- logs and diagnostic output;
- JSON and Markdown command output.

## Boundaries

The local integrity boundary assumes that another process running as the same
OS user does not maliciously modify the owner-private ctx data root while ctx is
using it. Such a process can directly replace the active-generation pointer or
rewrite candidate bytes regardless of clone strategy; defending against it
requires an OS sandbox or a distinct service identity. Generation fences still
fail closed on ordinary concurrent mutation, replacement, truncation, and
corruption within the supported single-owner lifecycle.

The default-enabled persistent daemon and explicit provider-source import route
write Core generations and derived state only under the configured ctx data
root. Search and MCP may send a bounded, content-free daemon wake, but query
processes do not become foreground history writers. Show, locate, sources, and
doctor do not write provider data or repositories. `ctx status` does not mutate
Core history and does not initialize or migrate local storage.

Show reads complete policy-selected normalized records from the active verified
Core/Tantivy generation. It does not reopen provider history, scan for
replacement transcripts, or use a network fallback.

Source repositories and provider history roots remain outside ctx ownership.
Provider files are read as import sources, not modified.


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
