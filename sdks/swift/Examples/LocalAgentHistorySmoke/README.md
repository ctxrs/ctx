# LocalAgentHistorySmoke

Fake-by-default Swift smoke executable for the local ctx agent history SDK.

Run from `sdks/swift`:

```bash
swift run LocalAgentHistorySmoke
```

The default mode uses an in-memory fake `CommandRunner` and exercises
`status`, `initialize`, `importHistory`, `sync`, `search`, `showEvent`,
and `showSession` without reading real local history.

Real ctx CLI mode is explicit:

```bash
swift run LocalAgentHistorySmoke --real --ctx-path /path/to/ctx --data-root /tmp/ctx-smoke
```
