# Schema-22 blame upgrade fixture

The unmodified public CLI at `1609b7ed4dc7df978fed91ea3adb8b603869d24a`
produced `search/` with `ctx import --all` on Linux x86_64. This includes
schema-22 Core and its working attribution generation, using the materializer
revision introduced by #1157. The producer was built with
`scripts/bazelw build //crates/ctx-cli:ctx --config=test`.

`source.jsonl` is authored synthetic Codex input, not a provider capture. It
records a Git commit and its full object ID from a disposable repository with
remote `https://github.com/example/upgrade-review.git`. `commit.txt` is that
object ID. The old CLI's commit blame returned one match and one citation.
All paths refer to the disposable `/tmp/ctx-schema22-blame-fixture` tree;
no personal history or credentials were used. Runtime locks, progress and
physical certification caches are excluded.

Tests restore private directory and sealed segment permissions after copying.
They query the existing attribution generation without the original repository
or provider files, rebuild Core, then catch attribution up from retained Core.
Do not regenerate with the current writer: the old format is the test boundary.
