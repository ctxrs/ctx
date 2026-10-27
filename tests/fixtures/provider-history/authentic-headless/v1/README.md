# Native headless ASR captures

These files were written by Qwen Code 0.26.1 and Continue CLI 1.5.47 on Linux
x86_64, running against an OpenAI-compatible endpoint with gpt-6-astra. The
workspace contained only a small Python greeting function and assertions.
They are sanitized authentic captures, not authored approximations of the
native format.

- Qwen: edit and shell-test a greeting; resume; fork and request a read-only
  Explore subagent. The parent journals and child sidecar retain their layout.
- Continue: read the files, add a second assertion and run the Python test.

The test scope is session discovery, transcript and tool projection, native
field preservation, child discovery, and unchanged repeat exports. This is
not blanket qualification of these releases, every tool, or compaction.

`scripts/fixtures/sanitize-asr-text-capture.py` applies consistent literal
replacements and deterministic identifier pseudonyms to both file names and
contents. Acquisition paths become `/fixture/...`; the endpoint port is
replaced. UUIDs, call IDs and opaque hexadecimal identifiers are pseudonymized.
The transform preserves JSON formatting, line and event order, cardinality,
relative layout and cross-file identity. It does not reserialize documents.
Two independent runs produced byte-identical manifests. The tests do not claim
preservation of byte offsets, acquisition paths, or raw identifier values.

No credentials, personal history, native databases, or raw capture receipts
are included. Qwen Code and Continue are Apache-2.0 projects; their native
transcript structure and emitted tool descriptions are retained as test data.
