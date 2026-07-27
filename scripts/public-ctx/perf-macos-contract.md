# macOS performance measurement contract

`perf-macos-measure.py` runs one supplied command directly under
`/usr/bin/time -l` on macOS and produces a deterministic JSON receipt. It is a
Darwin adapter, not a compatibility translation of GNU `time -v`.

## Invocation and artifacts

All four output paths must be distinct, their parent directories must already
exist, and the adapter refuses to replace any existing output:

```sh
scripts/public-ctx/perf-macos-measure.py \
  --output "$run_dir/measurement.json" \
  --stdout "$run_dir/command.stdout" \
  --stderr "$run_dir/command.stderr" \
  --time-output "$run_dir/time-l.txt" \
  -- "$binary" import --provider codex --path "$fixture"
```

The command is passed as an argument vector; no shell evaluates it. The four
artifacts are:

- `measurement.json`: canonical compact JSON, terminated by one newline;
- `command.stdout`: the command's stdout bytes;
- `command.stderr`: command stderr with the native timing footer removed;
- `time-l.txt`: the exact native timing footer, including the abnormal
  termination marker when `/usr/bin/time` emits one.

`measurement.json` is published last and is the completion marker. No
timestamp, hostname, temporary path, or environment-dependent ordering is
added to it.

## Normative metrics

A receipt with `valid: true` always contains every metric below. Missing,
malformed, or duplicate normative fields make the measurement invalid.

| JSON field | Native `time -l` field | Unit |
| --- | --- | --- |
| `wall_time_seconds` | `real` | seconds |
| `user_cpu_seconds` | `user` | seconds |
| `system_cpu_seconds` | `sys` | seconds |
| `maximum_resident_set_size_bytes` | `maximum resident set size` | bytes |
| `block_input_operations` | `block input operations` | operations |
| `block_output_operations` | `block output operations` | operations |

The I/O contract is
`darwin_ru_inblock_ru_oublock_operations_v1`. The two values are the Darwin
resource-usage block input/output operation counters reported by
`/usr/bin/time -l`. They are counts of operations. They are not byte counts,
sector counts, read/write syscall counts, or GNU fields renamed for
convenience. Consumers must not convert them to bytes or compare them as if
they were Linux `/usr/bin/time -v` `File system inputs` and
`File system outputs`.

Zero is a valid observed operation count. An unavailable counter is not zero:
it invalidates the measurement.

## Status and failures

A nonzero command exit does not invalidate otherwise complete metrics. The
receipt records `status.kind: "exited"` and the exit code, and the adapter
returns that code.

When native `time` emits `Command terminated abnormally.`, the receipt records
`status.kind: "signaled"`. If the collector process status carries the signal
number, `status.signal` contains it. Otherwise Darwin `time -l` does not expose
the number in its output, so `status.signal` is `null` and
`status.signal_unavailable_reason` is `"not_reported_by_darwin_time_l"`; the
raw native exit code remains in `status.time_exit_code`. This is explicit
unavailability, not an inferred signal number.

Adapter/preflight/parser failures return 125. If the command ran but the timing
footer was invalid, the adapter preserves stdout, the unsplit stderr, and the
raw collector output, then writes `valid: false` with a stable error code.
Normative `metrics` and `units` are absent from invalid receipts rather than
partially populated or filled with null/zero values. An unsupported platform
produces only the invalid JSON completion record because no command ran.

The adapter never treats Linux output as native macOS evidence. Its parser
requires the Darwin `real`/`user`/`sys` header and the named long metrics.

## Receipt shape

```json
{
  "collector": {
    "arguments": ["-l"],
    "io_contract": "darwin_ru_inblock_ru_oublock_operations_v1",
    "platform": "macos",
    "tool": "/usr/bin/time"
  },
  "command": ["/path/to/ctx", "status", "--json"],
  "metrics": {
    "block_input_operations": 4,
    "block_output_operations": 9,
    "maximum_resident_set_size_bytes": 32112640,
    "system_cpu_seconds": 0.03,
    "user_cpu_seconds": 0.08,
    "wall_time_seconds": 0.12
  },
  "schema": "ctx.perf.macos.time_l.v1",
  "schema_version": 1,
  "status": {
    "exit_code": 0,
    "kind": "exited",
    "signal": null,
    "signal_unavailable_reason": null,
    "time_exit_code": 0
  },
  "units": {
    "block_input_operations": "operations",
    "block_output_operations": "operations",
    "maximum_resident_set_size_bytes": "bytes",
    "system_cpu_seconds": "seconds",
    "user_cpu_seconds": "seconds",
    "wall_time_seconds": "seconds"
  },
  "valid": true
}
```

## `perf-smoke` integration

The caller must temporarily disable shell fail-fast behavior so it can retain
the command's nonzero status while still consuming the receipt:

```sh
set +e
"$repo/scripts/public-ctx/perf-macos-measure.py" \
  --output "$run_dir/measurement.json" \
  --stdout "$run_dir/command.stdout" \
  --stderr "$run_dir/command.stderr" \
  --time-output "$run_dir/time-l.txt" \
  -- "$CTX_PERF_SMOKE_BIN" "$@"
measurement_rc=$?
set -e

python3 - "$run_dir/measurement.json" <<'PY'
import json
import pathlib
import sys

receipt = json.loads(pathlib.Path(sys.argv[1]).read_bytes())
if not receipt.get("valid"):
    raise SystemExit("invalid macOS performance measurement")
metrics = receipt["metrics"]
print(metrics["wall_time_seconds"])
print(metrics["maximum_resident_set_size_bytes"])
print(metrics["block_input_operations"])
print(metrics["block_output_operations"])
PY

exit "$measurement_rc"
```
