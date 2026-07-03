# Local Transcript Corpus

Search output may include sensitive local history. The local CLI preserves
transcript text by default; it does not redact paths, token-shaped strings, or
credentials before indexing or display. Corpus tests should cover at least:

- common API key shapes;
- credential URLs;
- home-directory paths;
- private repository paths;
- environment variable dumps;
- token-like JSON fields;
- command output snippets.

Passing the corpus does not make output safe to share. It proves local
search/show/SQLite projections preserve representative transcript text so users
and agents can find exact local history. Share-safe or shared-service redaction
is outside the current local CLI contract. Rows marked
`redaction_state: "safe_preview"` use that legacy spelling for a local searchable
preview and must still be treated as private local history.
