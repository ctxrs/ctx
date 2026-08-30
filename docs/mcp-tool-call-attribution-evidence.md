# MCP activity attribution evidence runbook

This runbook governs exact rows in
[`mcp-tool-call-attribution-capabilities.json`](mcp-tool-call-attribution-capabilities.json).
It is an evidence contract for provider-native MCP activity, not another
provider-support table. General local history support remains authoritative in
[`provider-support-matrix.json`](provider-support-matrix.json).

## Qualification bar

An `exact` tuple must prove all of the following at the imported durable
boundary:

1. The source stores an authoritative MCP protocol marker, raw dispatch server
   alias, and advertised tool name without reconstruction.
2. The source stores exact provider call identity that can bind invocation and
   result activity without FIFO, order, or timing inference.
3. The producer, route, source format, schema, and version/generation boundary
   are explicit. Unknown generations remain `not-qualified`.
4. Current executable provider tests cover exact call/result preservation,
   malformed and duplicate/ambiguous abstention, exact field boundaries, and
   result preservation. Provider-neutral and cross-layer tests separately own
   Core bounds, stable lifecycle identities, and privacy sinks.

Configuration, current server lists, record order, punctuation splitting, and
time proximity are never identity evidence. Malformed, partial, oversized,
duplicate, or ambiguous identity evidence must not become a qualifying
`activity.invocation`.

The machine-readable authority is
[`mcp-tool-call-attribution-capabilities.json`](mcp-tool-call-attribution-capabilities.json).
Its 52 capability lanes contain three `exact`, 48 `not-qualified`, and one
`excluded` row. Each exact row names its current implementation plus the owning
Cargo/Bazel suite and live Rust tests. The checker resolves those references
against the repository; the owning targets execute the behavioral tests through
normal CI.

For Codex's session-tree route, only unversioned generation 1 is exact.
Producer versions 0.200.0, 0.201.0, and 0.202.0 are distinct
`not-qualified` lanes, and the prompt-history route remains `not-qualified`.

## Typed failure reasons

- `lossy_composite`: persistence normalizes, truncates, flattens, or
  non-injectively combines required identity.
- `exact_pair_transient_or_config`: required identity exists only in runtime,
  configuration, discovery, or provider-overridable state.
- `no_server_field`: durable call evidence has no authoritative server alias.
- `no_unique_terminal_link`: linking would require order, FIFO, timing, or name
  inference instead of an exact key.
- `route_mismatch`: a richer producer route is not the route ctx imports, or
  required identity is lost before the admitted boundary.
- `writer_version_unproven`: no public first-party writer/version contract
  proves the admitted durable shape.

`excluded` is separate from those failures and is used only for a hosted remote
trace outside ctx's local-history boundary.

## Public evidence hygiene

Evidence entries use public first-party source, release, artifact, or product
links and record only observed version bounds. Static binary inspection may
support a row when the official artifact and version are public, but local
paths, credentials, user transcripts, and nonpublic reports must never appear
in this contract.

Provider history and activity values are private and not share-safe by default.
Sanitized fixtures must preserve the structural ambiguity or exactness being
tested without copying arguments, results, paths, tokens, customer names, or
unrelated metadata.

## Change checklist

1. Add a new route/schema/producer row instead of broadening an older row by
   implication.
2. Record public evidence, observed pins, and fail-closed treatment of unknown
   generations.
3. For `exact`, bind the owning runtime tests. For `not-qualified`, choose one
   primary typed reason and explain secondary defects in `detail`.
4. Run `python3 scripts/check-mcp-tool-call-attribution-capabilities.py`, its
   mutation tests, the three focused provider suites, and the normal docs
   check.
