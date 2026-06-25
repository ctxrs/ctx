# Redaction Corpus

Search output may include sensitive local history. Redaction tests
should cover at least:

- common API key shapes;
- credential URLs;
- home-directory paths;
- private repository paths;
- environment variable dumps;
- token-like JSON fields;
- command output snippets.

Passing the corpus does not make output safe to share. It only proves the
current heuristics handle the examples in the test set.
