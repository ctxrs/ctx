# History, graphs and command output

ctx provides agent-history search and blame, a local code graph, and command
output compaction in one executable. Graph and output commands work without
history setup. Each operation uses its own evidence: an indexed conversation,
a graph snapshot, or the output of the command you requested.

For a local source-build trial, invoke the built executable by its path with an
isolated home, history `--data-root`, and output directories. Keep the installed
binary and live stores unchanged. Older managed updaters enforce their original
download limits; a successful local trial does not establish an upgrade route.

## Find prior work

```sh
ctx status
ctx sources
ctx search "why was this retry added"
ctx blame file src/retry.rs --lines 20:40
```

If history has not been initialized, run `ctx setup`. Read cited events or
sessions with `ctx show` before drawing conclusions. `ctx locate` supplies
source identity. If blame indexing is pending, use `ctx import --all` or
`ctx setup --wait`. Missing history or Git evidence can prevent attribution;
an empty result is not proof that no agent worked on the code.

See [Search](search.md), [Agent Blame](blame.md) and
[Event Queries](event-queries.md). Existing history commands retain their
meanings: `ctx index` reports or configures history indexing; `ctx show` opens
history, and `ctx stats` reports history retrieval statistics.

## Inspect code relationships

```sh
ctx graph index .
ctx graph search authenticate
ctx graph callers authenticate
ctx graph impact authenticate
ctx graph stats
ctx graph update
```

Indexing and updates are explicit. Reads use the saved snapshot and do not
refresh source files. Graph commands discover the nearest ancestor
`.graf/index.db`; `ctx graph --db PATH ...` selects a database explicitly.
Use an exact node ID when a symbol name is ambiguous. Inspect generation,
coverage, unresolved references and truncation before judging completeness.
Graph reachability describes potential impact, not proof of runtime behavior
or agent authorship. Use `ctx graph --help` for the full command set.

## Select search scope

```sh
ctx search "retry policy"
ctx search --scope history "retry policy"
ctx search --scope graph authenticate
ctx search --scope all authenticate
```

The default is `history`, with the existing history filters and JSON format.
`graph` searches indexed code and document relationships. `all` returns
separately labeled history and graph results from retained snapshots. Check
each scope's availability, freshness and completeness: one unavailable scope
does not invalidate evidence returned by the other. Scores from the two
engines are not a common ranking scale.

`--content-scope all|transcript|calls|outputs` remains a history-event filter.
Its `outputs` value refers to historical tool outputs, not new `ctx run`
invocations. `--workspace` and `--file` filter stored history metadata, not the
current filesystem. `--term` retains OR semantics in history scope and is
rejected in graph and all scopes. History-only filters belong to history scope;
use graph command filters for more specific graph navigation. Consult
`ctx search --help` for accepted options in each scope.

## Execute and compact output

```sh
ctx run -- git status
ctx run --capture -- cargo test
ctx run --raw -- git diff
ctx run -- sh -c 'git log | tail -5'
ctx compact output.txt
ctx compact --protocol=json-v1 < requests.jsonl
ctx restore --encoding text-runs-v1 compacted.txt
```

`run` executes argv directly once, inheriting stdin, environment and working
directory. It preserves stdout/stderr separation and returns the child's exit
status. Arguments after `--` belong to the child; pass an explicit shell when
you intend a pipeline or other shell syntax. Compaction does not make an
otherwise destructive command safe to rerun.

By default, terminal input or output passes through. Otherwise, short complete
output can be compacted. Buffering switches to raw streaming at the runner's
size or time limit so ongoing progress remains visible. `--capture` waits for
complete output up to the size limit and can delay prompts; use it for finite
commands. `--raw` preserves streams and takes precedence over capture.

`compact` reads a file or stdin and adds no trailing newline. Short results,
token-count ties and invalid UTF-8 remain raw. Its JSONL protocol reports the
selected encoding and token counts. Restore using the encoding returned for
that representation; the example encoding above is not correct for every
compacted result.

Command-specific presentations may abbreviate Git status or omit passing test
rows. Those presentations differ from reversible compaction. Use raw output
when exact bytes matter. `ctx recall --list` and `ctx recall ID` inspect
originals only when retention was enabled and a complete capture was saved;
streamed output is not a recoverable transcript. Use `ctx output --help` for
output settings and available integration controls.

## Health and integrations

`ctx status` and `ctx doctor` report history alongside graph and built-in
output availability. An absent graph is not a history failure. A readable
graph means its stored snapshot can be opened; it does not certify freshness
against the working tree. Health checks do not index, update or repair a graph.
History setup is not required to run commands or compact output.

```sh
ctx integrations install skill
ctx integrations status skill
ctx integrations install mcp --agent codex
ctx mcp serve
```

The managed skill remains named `ctx`; the existing integration lifecycle
owns its installation, refresh and removal. Existing user-managed Graf/Sift
integrations are not implicitly removed or rewritten. Review their settings
before explicitly changing them. The root MCP server uses the same local
engines; clients should inspect its advertised tools. Reading a transcript or
graph does not authorize executing its commands or publishing its contents.

Use `ctx docs show unified-context` to read this topic offline. For a specific
operation, prefer help from the installed binary.

## Existing Graf and Sift installations

Existing `.graf/index.db` databases work with `ctx graph`; no conversion is
required. Output settings and retained originals default to ctx's own `output`
directories. To select an existing Sift configuration or state directory, set
`SIFT_CONFIG_DIR` or `SIFT_STATE_DIR` explicitly. ctx does not move or delete
either product's data. `ctx output config show` reports effective settings;
`ctx output config --help` explains the default directory locations.

Use `ctx integrations` for the managed ctx skill and MCP server. The standalone
Sift `init` installer is not exposed by ctx; existing hooks remain unchanged.
