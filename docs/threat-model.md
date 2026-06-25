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
during normal setup and import commands. Search, list, show, sources, status,
doctor, and validate read local data and should not write outside the ctx data
root.

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
- treat JSON output as private until reviewed.
