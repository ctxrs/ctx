# Semantic Embedding Executors

ctx keeps one active semantic vector space per data root. The built-in
multilingual E5 executor is the local default. An external executor may instead
provide its own vector space over loopback HTTP or remote HTTPS. The selected
executor is used for both document and query embeddings; ctx never silently
falls back to another executor.

## Select an executor

Select or restore the built-in E5 executor explicitly:

```sh
ctx semantic enable --executor builtin
```

Bare `ctx semantic enable` only enables semantic search; it preserves the
current executor selection. On a new data root with no `[semantic]` config, that
selection defaults to the built-in executor.

Select a loopback executor:

```sh
ctx semantic enable --executor http://127.0.0.1:8080
```

Select a remote executor:

```sh
export CTX_SEMANTIC_EMBEDDING_TOKEN='your-token'
ctx semantic enable --executor https://embeddings.example.com/ctx
```

Plain HTTP is accepted only for a literal loopback IP address. A remote
executor requires HTTPS and the `CTX_SEMANTIC_EMBEDDING_TOKEN` bearer token.
ctx binds that token to the explicitly selected endpoint and does not send it
to another endpoint. Loopback describes only ctx's first HTTP hop: the receiving
process can retain, log, or forward content, including to another machine. A
remote URL explicitly sends semantic content off the machine.

`ctx semantic enable --executor URL` discovers `GET <base>/v2/contract`,
validates protocol schema V2, accepts the returned identity, and persists the
endpoint plus opaque `space_id` and `dimensions` for the current data root.
`schema_version` is validated on the wire, not persisted. Discovery sends no
history or query text. `ctx semantic status` reads local state without
contacting the endpoint.

For compatibility with the earlier fixed-E5 HTTP executor, explicit discovery
falls back to V1 only when the V2 contract route returns HTTP 404. It does not
downgrade after authentication, transport, TLS, malformed-response, server, or
identity errors. A V1 selection remains endpoint-only in configuration and
retains the exact built-in E5 vector contract.

The accepted identity is fail-closed. If the endpoint later reports a different
identity, ctx stops semantic indexing and querying until the user reruns
`ctx semantic enable --executor URL`. Explicitly accepting a changed identity
deletes and rebuilds only the derived semantic index. Imported history and the
lexical index remain intact.

## Passive manual-mode queries

With automatic indexing disabled, direct CLI searches using `--refresh off` or
`--refresh background` are read-only for Core and semantic storage. Before ctx
constructs an embedding executor or contacts an HTTP endpoint, it pins Core and
checks that the exact semantic projection for that Core generation is complete
and compatible.

If that projection is missing, stale, partial, unreadable, or incompatible, a
semantic-only search returns its typed semantic readiness error. A hybrid
search returns lexical results with that same stable reason code and retryable
classification. Neither case contacts an
executor, starts a daemon, waits for IPC, acquires a model, embeds documents,
or changes Core or semantic state.

An exact empty projection succeeds without constructing the selected executor.
For a nonempty projection, the built-in executor may load only an already
verified local model cache; it never acquires a model or runtime asset. Core ML
uses only an existing validated compiled artifact, Windows ML only an
already-ready provider, and ONNX only existing cache/runtime files. Auto may
fall back from a non-integrity accelerator load or authorization failure to an
already-cached CPU model; cached-artifact integrity failures remain fatal.
An HTTP executor uses the exact selected endpoint and its endpoint-bound
authentication. It may send the normal conformance probes and query embedding
request after preflight, so
`--refresh off` means no indexing or mutation, not necessarily no network when
HTTP was explicitly selected. ctx never substitutes the built-in executor for
an HTTP selection.

Direct CLI passive preflight takes a shared lock on the semantic store's existing Flat
transaction lock, refuses a SQLite WAL or rollback journal, opens the main
database read-only with immutable semantics, validates control metadata, and
pins the exact Flat generation before releasing the lock. The passive path
does not create a lock, database, WAL, SHM, journal, directory, or cache file.
Daemon and explicit Reconcile paths instead retain ordinary SQLite read-only
semantics so they can observe committed WAL state.

## V2 HTTP protocol

The base URL exposes JSON routes:

- `GET <base>/v2/contract`
- `POST <base>/v2/embeddings`

Remote requests use `Authorization: Bearer <token>`. Embedding requests use
`Content-Type: application/json`.

`GET /v2/contract` returns:

```json
{
  "schema_version": 2,
  "space_id": "opaque-space-id",
  "dimensions": 2
}
```

`space_id` is an opaque executor-defined, globally unique identifier for one
vector coordinate system. Use a collision-resistant value under a namespace
you control; do not use generic values such as `default`. The executor must
keep it stable while vectors remain compatible and change it when they do not.
ctx does not parse it as a provider or model name. Reusing an ID asserts that
the vectors are compatible even when the serving endpoint changes.

`POST /v2/embeddings` accepts one `input_kind`, either `query` or `documents`:

```json
{
  "schema_version": 2,
  "space_id": "opaque-space-id",
  "dimensions": 2,
  "request_id": "request-123",
  "input_kind": "query",
  "inputs": [
    {"id": "input-1", "text": "raw ctx search text"}
  ]
}
```

For `documents`, each input contains the raw text of a ctx-created document
chunk. ctx does not add model-specific prefixes, tokenize, truncate for a model,
or otherwise preprocess either input kind.

The response echoes the accepted space and request identity:

```json
{
  "schema_version": 2,
  "space_id": "opaque-space-id",
  "dimensions": 2,
  "request_id": "request-123",
  "embeddings": [
    {"id": "input-1", "embedding": [0.6, 0.8]}
  ]
}
```

The response must exactly match the accepted `schema_version`, `space_id`,
`dimensions`, and `request_id`, and return one unique embedding for every input
ID with no missing or extra IDs. Embeddings may be returned in any order because
ctx matches them by ID. Every vector must have exactly `dimensions` finite
values, be nonzero, and have a squared L2 norm within `0.001` of `1.0`. Any
mismatch fails semantic work closed; lexical search remains available.

## Bounds and transport

- Endpoint strings are nonempty, have no surrounding whitespace, are at most 2
  KiB, and cannot contain credentials, a query, or a fragment. Plain HTTP
  requires a literal loopback IP; other endpoints require HTTPS. HTTPS uses
  operating-system trust roots.
- `space_id` is 1–256 bytes using ASCII letters, digits, `.`, `_`, `:`, `/`,
  `@`, `+`, `=`, or `-`. `dimensions` is from 1 through 4,096.
- A bearer token is at most 4 KiB of non-whitespace printable ASCII. It is
  required for remote endpoints and optional for loopback.
- A contract response is at most 4 KiB. An embedding request and embedding
  response are each at most 8 MiB.
- One embedding request contains at most 512 inputs and at most 262,144 output
  vector scalars: the effective input limit is
  `min(512, floor(262144 / dimensions))`. ctx splits document work at that
  limit; an oversized encoded request still fails closed.
- DNS resolution and connection establishment each have a 5-second ceiling.
  Discovery and embedding operations have one 24-second aggregate budget.
  Redirects and ambient HTTP proxy discovery are disabled.

Every request carries `Accept: application/json`, `Accept-Encoding: identity`,
`Cache-Control: no-store`, and `X-Ctx-Semantic-Schema-Version: 2`. Requests
to embed content assert the accepted space in the JSON body and use
`Content-Type: application/json`. Authorization is added only when a bound token
is configured.

ctx makes at most two attempts. It retries once after a transport failure or
HTTP `408`, `429`, `500`, `502`, `503`, or `504`; other HTTP, schema, identity,
correlation, and vector-validation failures are not retried. An embedding retry
reuses the exact encoded body, including the same `request_id` and input IDs.
Executors must therefore treat `request_id` as an idempotency key: return the
same result for the same ID and body, and reject reuse with different bytes.

## Advanced manual configuration

Prefer `ctx semantic enable --executor URL`, which discovers and records the
identity atomically. Operators can instead author the complete accepted
identity in `config.toml`:

```toml
[semantic]
executor = "https://embeddings.example.com/ctx/"
space_id = "opaque-space-id"
dimensions = 768
```

All three fields are required for a V2 external-space executor. Writing this
triple is an advanced, manual acceptance of that endpoint and vector-space identity; config
loading does not discover it. Before sending content, ctx still verifies that
the endpoint serves protocol schema V2 and the exact accepted identity. If an
operator intentionally changes `space_id` or `dimensions`, ctx rebuilds the
derived semantic vectors for the new identity. Moving the same declared vector
space to a different endpoint restarts runtime routing but does not rebuild
compatible vectors.
Do not add `schema_version` to the config.

## Retained fixed-E5 V1

An endpoint-only `semantic.executor` written by the earlier fixed-E5
implementation remains a fully functional V1 selection:

```toml
[semantic]
executor = "https://embeddings.example.com/ctx/"
```

V1 uses `GET <base>/v1/contract` and `POST <base>/v1/embeddings`, with
`schema_version: 1`, `X-Ctx-Semantic-Schema-Version: 1`, `model_key`, and
`model_contract_fingerprint`. ctx applies
the pinned E5 query/document prefixes before sending text and verifies the
historical public conformance canary. Its vector fingerprint is exactly the
same as the built-in E5 executor, while the endpoint remains separately fenced
as runtime routing state. ctx never rewrites this selection during ordinary
loading or status. Explicit selection prefers V2 and persists a V2 triple; it
retains endpoint-only V1 only when V2 returns 404 and the V1 identity exactly
matches the pinned built-in contract.

## Responsibility boundary

The executor owns model selection and execution, including preprocessing,
tokenization, model-specific truncation, and query/document treatment. ctx owns
history ingestion, semantic document construction and chunking, the derived
vector index, and lexical, semantic, and hybrid retrieval.
