# Schema-22 upgrade fixture

`index/` was produced by the unmodified public `ctx` CLI at commit
`6511f14956342f6c78be2105b645ae0e44bc6b29`, built with
`scripts/bazelw build //crates/ctx-cli:ctx --config=test` on Linux x86_64.

The input is the authored `source.jsonl`: one synthetic Codex session and one
tool result with both an item ID and a call ID. It is not a native provider
capture. The CLI imported it through `ctx daemon run --force` in a disposable
home, with automatic indexing and analytics, upgrades and blame disabled.
Only the published lexical generation, manifest and pointer are retained;
runtime locks and physical certification caches are excluded. No personal
history, account data or credentials were used.

The regressions copy these bytes into private temporary directories and restore
the published segment files' read-only permissions, which Git does not retain.
They verify
search before, during and after a schema-23 rebuild, including interrupted
staging and unchanged event identity. The CLI regression also verifies automatic
daemon startup without any search request and includes a second provider route. Do not regenerate this fixture with the
current writer: that would remove the compatibility boundary being tested.
