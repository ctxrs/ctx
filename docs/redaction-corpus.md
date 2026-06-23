# Redaction Corpus

The launch branch includes a synthetic redaction corpus and uses it in current
share-safe output tests. It does not claim a general-purpose redaction engine
for every raw command, archive, internal database, report, or dashboard path.
The fixture lives at `tests/fixtures/redaction/redaction-corpus.jsonl`.

## Purpose

Use the corpus to keep security review concrete:

- define examples of secrets and sensitive local data that ctx may
  encounter;
- separate input examples from expected redacted output;
- make current and future redaction tests deterministic;
- avoid using real customer data, real tokens, or real private repository URLs
  in tests.

## Corpus Format

Each JSONL row has:

- `id`: stable fixture id;
- `surface`: source surface such as `record_body`, `command_stdout`,
  `capture_spool`, `archive`, `dashboard`, or `pr_link`;
- `input`: synthetic sensitive text with a stable `corpus-*` marker for
  deterministic search/context tests;
- `expected_redacted`: expected safe text after shareable output redaction;
- `notes`: why the case matters.

All values must be synthetic. Tokens should use obvious fake prefixes and
invalid checksums. Private URLs should use example domains or reserved
repository names.

## Initial Coverage

The current corpus covers:

- environment variable tokens;
- GitHub-style tokens;
- cloud access key shapes;
- database URLs;
- bearer headers;
- private paths;
- pull request URLs with embedded credentials;
- command output containing customer-like data;
- archive payload snippets;
- dashboard/report preview text.

## Test Expectations

Current CLI integration tests load this corpus for active shareable surfaces
including search/context JSON, report JSON, dashboard HTML, and PR dry-run
markdown. Before adding provider transcript import, broader capture hooks, more
publishing targets, or hosted sync, add automated tests that prove each new
shareable output surface is covered by the corpus. Raw archives and internal
storage remain private data and are not treated as share-safe outputs.
