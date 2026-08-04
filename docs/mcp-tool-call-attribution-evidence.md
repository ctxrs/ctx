# MCP attribution evidence runbook

This runbook governs additions to
[`mcp-tool-call-attribution-capabilities.json`](mcp-tool-call-attribution-capabilities.json).
It is an evidence contract for exact MCP tool-call attribution, not another
provider-support table. General local history support remains authoritative in
[`provider-support-matrix.json`](provider-support-matrix.json).

## Qualification bar

Runtime knowledge alone is insufficient. An `exact` tuple must prove all of
the following at the imported durable boundary:

1. The authoritative raw dispatch server alias and MCP-advertised tool name.
2. A durable, unique call identity linking that invocation to one canonical
   success, error, cancellation, or other terminal result.
3. A producer, route, source format, and schema/version boundary. An
   unversioned observation qualifies only the recorded pin; unknown generations
   remain `not-qualified`.
4. Executable positive, ambiguity/duplicate, and stable-identity test IDs in
   `exact_checks` that resolve through the public conformance authority.

The machine authority is
`crates/ctx-history-capture/tests/mcp-attribution-conformance.manifest.json`.
Its only executable suite registry is
`crates/ctx-history-capture/tests/mcp_attribution_suites.bzl`, which binds suite
aliases to Bazel targets, named tests, and evidence classes. The capabilities
JSON is the user-facing projection: its `exact`, `not-qualified`, and
`excluded` states map to the manifest's `supported`, `not_qualified`, and
`excluded` states. It does not define an independent suite table.

Manifest capability revision 5 freezes 41 providers, 43 base routes, 42 imported
schema generations, and 46 capability lanes: three `supported`, 42 `not_qualified`,
and one `excluded`. For Codex's session-tree route, only unversioned generation
1 is supported. Producer versions 0.200.0, 0.201.0, and 0.202.0 are distinct
`not_qualified` lanes, and the prompt-history route remains `not_qualified`.

Configuration, current server lists, record order, names split on punctuation,
and FIFO or time proximity are never identity evidence. A malformed, partial,
oversized, duplicate, or ambiguous pair must retain the ordinary event and omit
`mcp_tool_call`.

## Typed failure reasons

- `lossy_composite`: persistence sanitizes, normalizes, truncates, flattens, or
  non-injectively combines the pair.
- `exact_pair_transient_or_config`: the pair exists only in runtime,
  configuration, discovery, or provider-overridable state.
- `no_server_field`: durable call evidence has no authoritative server alias.
- `no_unique_terminal_link`: linking requires order, FIFO, timing, or name
  inference rather than a durable unique key.
- `route_mismatch`: a richer producer route is not the route ctx imports, or
  identity is lost before the admitted boundary.
- `writer_version_unproven`: no public first-party writer/version contract
  proves the admitted durable fields and lifecycle.

`excluded` is separate from those failures. It is used here only for a hosted
remote trace outside ctx's local-only history boundary.

## Public evidence hygiene

Evidence entries use public first-party source, release, artifact, or product
links and record only version bounds that were actually observed. Static binary
inspection may support a row when the official artifact and version are public,
but local extraction paths, credentials, user transcripts, and private reports
must never appear in this contract.

Provider history and exact server/tool names are private and not share-safe by
default. Sanitized fixtures must preserve the structural ambiguity or exactness
being tested without copying arguments, results, paths, tokens, customer names,
or unrelated metadata.

## Change checklist

When adding or changing a tuple:

1. Add a new route/schema/producer row instead of broadening an older row by
   implication.
2. Record public evidence, observed pins, and the fail-closed treatment of
   unknown generations.
3. For `exact`, add the executable test IDs. For `not-qualified`, choose one
   primary typed reason and explain secondary defects in `detail`.
4. Run `python3 scripts/check-mcp-tool-call-attribution-capabilities.py` and the
   normal docs checks. The checker freezes 41 providers, 43 base routes, 46
   tuple rows, three exact tuples, 42 not-qualified tuples, and one excluded
   tuple. When the conformance files are present, it also cross-checks the
   manifest arithmetic, Codex partition, and suite/test references.
