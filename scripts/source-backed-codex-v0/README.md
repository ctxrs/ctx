# Source-backed Codex V0 benchmark

`run.py` is a create-only, Linux end-to-end benchmark harness for a runnable
source-backed Codex V0 candidate. It measures the real public CLI path:

```text
ctx search --provider codex --backend lexical --refresh wait
ctx show event
ctx show session
```

The search result is JSON on stdout. Search refresh progress and any candidate
phase-attribution JSON remain byte-for-byte in each phase's raw stderr file.
The harness selects one returned result containing both a ctx event ID and a
ctx session ID, passes those IDs as argv elements (never shell text), and uses
them for cold and warm `show` calls.

## Usage

All four paths must be absolute and canonical (no symlink components). The
candidate and corpus must exist. The data root and output directory must not
exist; the harness creates both with mode `0700` and never removes or reuses
them.

```bash
scripts/source-backed-codex-v0/run.py \
  --candidate /absolute/path/to/ctx \
  --codex-home /absolute/path/to/codex-home-compatible-corpus \
  --data-root /absolute/task-owned/path/fresh-ctx-data \
  --query 'a query known to return at least one result' \
  --output-dir /absolute/task-owned/path/benchmark-output \
  --sandbox auto
```

`--show-content complete` is the default so the measured show path exercises
verified provider-source resolution. Use `--show-content indexed` only when a
comparison explicitly calls for indexed previews.

The corpus root must contain at least one ordinary `sessions/`,
`archived_sessions/`, or `history.jsonl` source. The data and output roots must
be disjoint and outside the corpus.

## Safety modes

`--sandbox auto` is the default. Before creating either measured root, the
harness probes Bubblewrap with a read-only-root `/bin/true` invocation. If the
probe succeeds, every measured process uses Bubblewrap:

- the host root and the corpus have read-only mounts;
- only the explicit fresh ctx data root and output directory are writable;
- the task-owned `output/runtime/tmp` is mounted at `/tmp`;

If Bubblewrap is missing or unprivileged user namespaces are unavailable,
`auto` uses direct execution. Direct mode retains create-only data/output
paths, routes XDG and temporary state under output, and makes pre/post full
source inventories an equality gate, but it cannot enforce a read-only host
filesystem. The summary records `sandbox_mode: "none"` and
`host_root_mount: "not_sandboxed"` explicitly.

Use `--sandbox bwrap` to make a failed probe fatal, or `--sandbox none` to
request direct mode without probing. In every mode, analytics, local usage,
daemon startup, semantic search, and auto-upgrade are disabled; `HOME` is
inherited unchanged rather than replaced with a benchmark directory.

The harness does not delete partial runs. A failure preserves completed phase
receipts and emits a failed `summary.json` when output creation succeeded.

## Measured sequence

1. cold lexical query with synchronous Codex refresh;
2. cold complete-content event show;
3. cold complete-content session show;
4. warm lexical query with synchronous no-op refresh;
5. warm event show using the originally selected ID;
6. warm session show using the originally selected ID.

Every phase stores:

- `stdout.json`;
- raw `stderr`;
- a GNU `/usr/bin/time` receipt;
- an exit-status receipt.

GNU time captures wall seconds, user seconds, system seconds, and maximum RSS
in KiB. The aggregate `summary.json` includes those metrics, candidate SHA-256,
the exact argv for each phase, selected IDs, stderr JSON phase counts/samples,
search freshness/retrieval attribution, and source/data-root inventories.
Inventories report regular-file bytes, allocated bytes, file/directory counts,
largest files, and a metadata digest before/after the run.

The harness prints exactly the same summary as one compact JSON line on stdout.
Human phase updates go to stderr.

## Self-test

The self-test uses a tiny fake candidate and corpus. It exercises the automatic
sandbox probe/fallback, all six measured phases, ID extraction, source
immutability checks, GNU time parsing, inventories, and summary equivalence.

```bash
bash -n scripts/source-backed-codex-v0/self_test.sh
python3 -c 'import ast, pathlib; ast.parse(pathlib.Path("scripts/source-backed-codex-v0/run.py").read_text())'
scripts/source-backed-codex-v0/self_test.sh
```

## Frozen production corpus

The later frozen corpus is:

```text
/home/daddy/code/ctx-day1-performance-runs/nativepath-frozen-pair-pending-20260727T0013Z/corpus
```

Its recorded inventory is 11.07 GiB and 3,464 files. Do not launch that run
until the candidate has passed its readiness gate; prepare fresh, task-owned
data and output paths when it is ready.
