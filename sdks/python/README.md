# ctx Python SDK

Experimental Python SDK for the local ctx `agent-history-v1` API.

The SDK is intentionally small and network-free by default. It wraps local
`ctx` CLI JSON and normalizes it into the shared `agent-history-v1` contract. Hosted
configuration types are present so application code can be written against one
client shape, but hosted transport is not implemented yet.

## Install For Local Development

```bash
cd sdks/python
python -m pip install -e .
```

No API keys are required. The local client shells out to `ctx`.

## Quick Start

```python
from ctx_agent_history import AgentHistoryClient

client = AgentHistoryClient.local(ctx_binary="ctx", data_root="/tmp/ctx")

status = client.status()
sources = client.sources()
response = client.search("sqlite storage", limit=5, refresh="off")

for hit in response["search"].get("results", []):
    print(hit.get("ctxSessionId"), hit.get("snippet"))
```

## API

The public methods mirror the agent-history-v1 client surface:

- `status()`
- `init()` for `ctx setup --json`
- `sources()`
- `import_()` and `sync()` (`import` is a reserved Python keyword)
- `search()` with a query, term, or file option
- `show_event()` / `showEvent()`
- `show_session()` / `showSession()`
- `locate_event()` / `locateEvent()`
- `locate_session()` / `locateSession()`
- `version()` and `versioning()`

Every operation returns a dictionary with `contractVersion: "agent-history-v1"`,
`schemaVersion: 1`, `operation`, `backend`, and an operation-specific payload
such as `status`, `sources`, `import`, `search`, `event`, `session`, or
`location`.

The package includes PEP 561 type metadata and exports operation-specific
`TypedDict` envelopes such as `StatusResponse`, `SearchResponse`,
`ShowEventResponse`, and `LocateSessionResponse`. These are hand-written to
match the shared `agent-history-v1` contract while keeping runtime dependencies empty.

`sync()` is an alias for import because the current local agent-history-v1
implementation syncs by importing local provider history into the ctx index.

## Errors

All SDK errors inherit from `CtxAgentHistoryError` and expose:

- `code`
- `message`
- `retryable`
- `details`
- `cause`

CLI failures raise `CtxAgentHistoryCliError` with `exit_code`, `stderr`, and the
command argv. Invalid or missing JSON raises `CtxAgentHistoryProtocolError`.

## Hosted Placeholder

```python
from ctx_agent_history import HostedConfig, AgentHistoryClient

client = AgentHistoryClient.hosted(HostedConfig(base_url="https://example.invalid"))
client.status()  # raises HostedTransportNotImplementedError
```

Hosted transport deliberately makes no network calls in this experimental SDK.

## Dogfood Example

```bash
cd sdks/python
python3 examples/dogfood_local.py
```

By default the example creates a temporary fake `ctx` binary and exercises
`status`, `search`, `show_event`, `show_session`, `locate_event`, and
`locate_session` without network access, API keys, or private history. Set
`CTX_AGENT_HISTORY_CTX=/path/to/ctx` to point it at a real local CLI instead.

## Tests

```bash
cd sdks/python
python3 -m unittest discover -s tests
```

The native tests use fake local CLI scripts and do not require network access,
API keys, or a populated ctx index. If shared contract fixtures are later added
under `contracts/agent-history-v1/fixtures`, the fixture smoke test will consume them
automatically.
