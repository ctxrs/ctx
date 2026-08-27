# Limitations

ctx is production-scoped to local history indexing and search retrieval.
These limitations are intentional unless another document says a capability has
shipped.

## Provider Coverage

- Codex local import is supported for documented raw `.jsonl` and standard
  Zstandard `.jsonl.zst` rollout sources. Compressed rollout admission retains
  at most 256 MiB of combined compressed snapshot plus decoded spool per leaf,
  and all parallel leaves share the 1 GiB route scratch ceiling.
- Pi local import is supported when matching local session JSONL files exist
  under `~/.pi/agent/sessions`, or when an explicit Pi session JSONL file is
  passed with `--path`.
- Additional supported agent harnesses are listed in the provider matrix and are
  imported only when their documented local history paths exist and match the
  supported native formats.
- NanoClaw local import participates in native automatic discovery from an
  exact project CWD or official launchd/systemd service registration. Alternate
  unregistered roots require exact `--path`. AstrBot is supported for bounded
  `data_v4.db` locations and imports local LLM context plus available platform
  history rows when present, but upstream AstrBot still treats non-WebChat raw
  IM replies as platform-side history rather than guaranteed `data_v4.db`
  transcript rows.
- Unknown provider formats should not be parsed optimistically.
- Automatic discovery checks the single winner from each provider's precedence,
  except for current providers that genuinely maintain finite coexisting stores.
  Removed probes do not remove already indexed history.
- One-shot flags, API paths, moved roots, past launch directories, and container
  host mappings generally require exact `--path`. Manual selection bypasses
  discovery precedence, not parser or path-safety validation.
- Current Kiro ACP/v3 remains detected but unsupported. Provider selectors that
  cannot be safely reconstructed, including unsafe Qoder SDK selections, require
  exact `--path`.

## Import Semantics

- Automatic indexing is the default and permits eligible setup, import, and
  background search operations to start or wake persistent ctx-owned daemon
  maintenance. Manual indexing starts no persistent/background daemon; setup
  and background search remain inert, while explicit import and search
  `--refresh wait` may start a finite Core worker. Use `ctx setup --no-daemon`
  or `ctx import --no-daemon` for a one-run process-start opt-out. Explicit
  `ctx daemon run --force` runs persistent maintenance in the foreground even
  when manual mode is configured, blocks until stopped, and does not change the
  configured indexing mode. The canonical setting is `[indexing] mode = "auto"`
  or `"manual"`; use `ctx index mode` to read or change it.
- Finite Core workers use the same daemon refresh engine and endpoint, but do
  not install supervision or run watcher, timer, semantic, reconciliation, or
  upgrade maintenance. They exit only after an admitted request exists, all
  Core requests are terminal, and IPC is quiescent.
- Automatic restart of continuous refresh for a hosted managed install requires
  an operational native current-user service manager: systemd-user on Linux,
  the launchd GUI user domain on macOS, or Task Scheduler on Windows. When that
  manager is unavailable, automatic setup and eligible imports use the same
  persistent detached CLI-self-healing daemon as unmanaged installs and custom
  data roots. The process has no finite idle lifetime, but native automatic
  restart after a crash, login, logout, or reboot is unavailable. The next
  eligible automatic ctx command restarts an absent fallback daemon. Manual
  finite workers never install this fallback.
- Current importers use idempotent rescans.
- `--resume` is reported in output but is not a universal provider cursor
  contract.
- Explicit `--path` imports are not remembered as future defaults.

## Search Semantics

- Search quality depends on what providers expose and what importers index.
- Command and tool result bodies are not searchable; only compact typed
  outcome/evidence metadata is indexed.
- Ranking is deterministic for the same local database and options, but it is
  not a claim of semantic understanding.
- Empty or punctuation-only search is invalid. Broad valid queries can still
  return metadata-driven matches.
- Semantic embeddings depend on a compatible local ONNX Runtime backend and
  the opt-in ctx daemon query service. Release/platform combinations without a
  validated local runtime remain lexical-safe: `hybrid` falls back to lexical
  and explicit `semantic` reports a local unavailable/runtime error instead of
  linking an unsupported backend.
- Automatic semantic indexing requires auto mode. Use `ctx index mode auto`
  before `ctx semantic enable --wait` when manual mode is configured. A plain
  `ctx semantic enable` still records the opt-in in manual mode. Lexical search
  remains available while embeddings build, and hybrid uses both backends when
  coverage is ready.
- Semantic indexing intensity supports only `quiet` and `full`; quiet is the
  background-friendly default. Full removes deliberate inter-batch pacing for
  semantic document index construction, but remains subject to safety,
  resource, and admission limits. Exact speedups and stable CPU percentages are
  not guaranteed.
- Intensity covers initial backfill, incremental refresh, rebuild/recovery,
  daemon reconciliation, and finite foreground reconciliation. It does not
  affect interactive query embedding, lexical indexing, or embedding
  identity/readiness, and it does not change automatic/manual indexing mode.
- There is no persistent CLI intensity setter in this version. Edit
  `indexing_intensity = "full"` under `[semantic]` in `config.toml` for
  persistent full behavior. `ctx semantic enable --wait --intensity full` is
  temporary: it does not rewrite the configured intensity and expires with the
  wait, a waiting process crash, or a daemon restart.
- Semantic embedding is local. External and bring-your-own embedding services
  are not supported.
- The ctx macOS CLI targets macOS 13, but ONNX Runtime 1.27 follows its upstream
  macOS 14 minimum. On macOS 13, daemon-backed lexical search remains available
  while semantic search is unavailable.

## Retrieval Semantics

- Search output is retrieval material, not generated analysis.
- Token counts are estimates.
- JSON is local/private and can include sensitive content.
- Show reads complete policy-selected normalized records from the active
  verified Core/Tantivy generation without reopening provider history.
- Exact presentation includes accepted structured tool input, tool output,
  command output, patch, and diff content. Explicitly redacted content,
  unsupported binary payloads, and provider-private blobs remain unavailable.

## Operations

- Core setup/import/search are local filesystem operations.
- Official installer-managed binaries can use signed release metadata for an
  explicit `ctx upgrade` command and automatic checks while automatic upgrades
  are enabled. Auto indexing with the full daemon profile uses the persistent
  daemon as the sole automatic-upgrade authority; manual and
  source-refresh-only modes perform no automatic upgrade work.
- Ordinary foreground commands and MCP do not claim or spawn automatic
  upgrades.
- Finite Core workers do not perform automatic upgrade checks or application.
- Unmanaged installs do not self-upgrade.
- No provider beyond the support matrix should be described as supported.
