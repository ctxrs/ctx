# Semantic Embedding Executors

ctx uses one pinned semantic vector-space contract. The built-in executor runs
that contract locally and remains the default. You can instead select a
conforming HTTP endpoint that runs the same contract elsewhere.

This is an execution-location choice, not a model selector. Document indexing
and query embedding always use the same selected executor. Switching between
conforming executors does not rebuild compatible vectors, and ctx never falls
back silently to a different executor.

## Select an executor

Keep the built-in default:

```sh
ctx semantic enable --executor builtin
```

Select a loopback service:

```sh
ctx semantic enable --executor http://127.0.0.1:8080
```

Select a remote service:

```sh
export CTX_SEMANTIC_EMBEDDING_TOKEN='your-token'
ctx semantic enable --executor https://embeddings.example.com/ctx
```

Omitting `--executor` preserves the current selection. Disabling semantic
search stops semantic embedding work but retains derived data:

```sh
ctx semantic disable
```

`ctx semantic status` is credential-free and does not contact the endpoint. It
reports the selected endpoint, whether content leaves the machine, daemon
activation state, and the last closed failure category.

## Passive daemon-free queries

With the daemon disabled, manual searches using `--refresh off` or
`--refresh background` are read-only. Before ctx constructs an embedding
executor or contacts an HTTP endpoint, it pins Core and checks that the exact
semantic projection for that Core generation is complete and compatible.

If that projection is missing, stale, partial, unreadable, or incompatible, a
semantic-only search returns its typed semantic readiness error. A hybrid
search returns lexical results with that same reason. Neither case contacts an
executor, starts a daemon, waits for IPC, acquires a model, embeds documents,
or changes Core or semantic state.

An exact empty projection succeeds without constructing the selected executor.
For a nonempty projection, the built-in executor may load only an already
verified local model cache; it never acquires a model. An HTTP executor uses
the exact selected endpoint and its endpoint-bound authentication. It may send
the normal conformance probes and query embedding request after preflight, so
`--refresh off` means no indexing or mutation, not necessarily no network when
HTTP was explicitly selected. ctx never substitutes the built-in executor for
an HTTP selection.

## Transport policy

- Plain HTTP is accepted only for a literal loopback IP address.
- Every non-loopback endpoint requires HTTPS and
  `CTX_SEMANTIC_EMBEDDING_TOKEN`.
- Redirects and ambient HTTP proxy discovery are disabled.
- HTTPS uses the operating system trust roots.
- Request and response bodies are bounded. Each operation has one aggregate
  deadline and at most one retry for a transport failure or HTTP 408, 429, 500,
  502, 503, or 504.
- A retried embedding request reuses the exact request ID and bytes. Servers
  must return the same result for the same request ID and body, and reject a
  request ID reused with different bytes.

The bearer token is never written to ctx status or logs. ctx internally binds
an inherited token to the normalized selected endpoint before a daemon can use
it; a missing or mismatched binding fails before any network request.

Setting `CTX_SEMANTIC_EMBEDDING_TOKEN` authorizes the remote endpoint selected
for that process. An existing independent binding is never silently changed;
an explicit `--executor URL` selection is the authority to rebind it. A token
is sent to loopback only when the same invocation explicitly selects that
loopback URL. In automatic mode, selecting or re-enabling an HTTP executor,
rotating its token with `ctx semantic enable`, or disabling it performs a
bounded daemon restart. Native supervisor state stores the endpoint-bound pair
in owner-private artifacts so automatic restart can keep working; disabling
semantic search or selecting `builtin` recreates those artifacts without it.

## HTTP contract

The base URL exposes these routes:

- `GET <base>/v1/contract`
- `POST <base>/v1/embeddings`

Requests include these headers:

```text
Accept: application/json
Cache-Control: no-store
X-Ctx-Semantic-Schema-Version: 1
X-Ctx-Semantic-Model-Key: <pinned model key>
X-Ctx-Semantic-Model-Contract-Fingerprint: <pinned fingerprint>
Authorization: Bearer <token>    # when configured
```

`GET /v1/contract` returns:

```json
{
  "schema_version": 1,
  "model_key": "<pinned model key>",
  "model_contract_fingerprint": "<pinned fingerprint>"
}
```

`POST /v1/embeddings` accepts:

```json
{
  "schema_version": 1,
  "model_key": "<pinned model key>",
  "model_contract_fingerprint": "<pinned fingerprint>",
  "request_id": "<uuid>",
  "input_kind": "query",
  "inputs": [
    {"id": "<uuid>", "text": "query: prepared text"}
  ]
}
```

`input_kind` is `query` or `documents`. Text is already prepared with the
pinned contract's exact query or document prefix and truncation rules.

The response is:

```json
{
  "schema_version": 1,
  "model_key": "<pinned model key>",
  "model_contract_fingerprint": "<pinned fingerprint>",
  "request_id": "<same uuid>",
  "embeddings": [
    {"id": "<matching input uuid>", "embedding": [0.01, -0.02]}
  ]
}
```

Responses may return embeddings in any order because ctx matches them by input
ID. IDs must be unique and complete. Vectors must have the pinned dimension,
contain only finite values, and be L2-normalized.

## Conformance and trust

Before sending user history or query text, ctx checks the asserted contract and
runs a frozen query/document probe pair against the pinned vector space.
Ordinary schema, identity, correlation, vector, or conformance errors fail
closed and are not hot-retried. Lexical search remains available.

The endpoint is still an integrity-trusted execution authority. The probes
catch accidental incompatibility and ordinary misconfiguration; they
do not defend against an endpoint intentionally special-casing the public pair
and returning different embeddings for user content.
