# Redaction Corpus

The launch branch documents a redaction corpus but does not yet ship a
general-purpose redaction engine for every command, archive, report, or
dashboard path. The fixture in
`tests/fixtures/redaction/redaction-corpus.jsonl` is an intentionally inert
corpus for future tests and review.

## Purpose

Use the corpus to keep security review concrete:

- define examples of secrets and sensitive local data that Work Recorder may
  encounter;
- separate input examples from expected redacted output;
- make future redaction tests deterministic;
- avoid using real customer data, real tokens, or real private repository URLs
  in tests.

## Corpus Format

Each JSONL row has:

- `id`: stable fixture id;
- `surface`: source surface such as `record_body`, `command_stdout`,
  `capture_spool`, `archive`, `dashboard`, or `pr_link`;
- `input`: synthetic sensitive text;
- `expected_redacted`: expected safe text for a future redactor;
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

## Future Test Expectations

Before adding provider transcript import, broader capture hooks, PR publishing,
or hosted sync, add automated tests that prove the relevant output surface is
covered by the corpus. For launch docs, the corpus is documentation and fixture
material only.
