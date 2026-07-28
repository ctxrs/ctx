#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
test_root=$(mktemp -d /tmp/ctx-source-backed-codex-v0-self-test.XXXXXX)

cleanup() {
  case "$test_root" in
    /tmp/ctx-source-backed-codex-v0-self-test.*)
      rm -rf -- "$test_root"
      ;;
    *)
      printf 'refusing to remove unexpected self-test root: %s\n' "$test_root" >&2
      ;;
  esac
}
trap cleanup EXIT

corpus="$test_root/corpus"
data_root="$test_root/data"
output_dir="$test_root/output"
stdout_file="$test_root/harness.stdout"
mkdir -m 0700 -- "$corpus"
mkdir -m 0700 -- "$corpus/archived_sessions"
printf '{"type":"session_meta","payload":{"id":"fake"}}\n' \
  >"$corpus/archived_sessions/fake-session.jsonl"

if ! "$script_dir/run.py" \
  --candidate "$script_dir/fake_ctx.py" \
  --codex-home "$corpus" \
  --data-root "$data_root" \
  --query "fake benchmark query" \
  --output-dir "$output_dir" \
  >"$stdout_file"; then
  printf 'fake-candidate harness run failed:\n' >&2
  sed -n '1,240p' "$stdout_file" >&2
  while IFS= read -r stderr_file; do
    printf '[%s]\n' "$stderr_file" >&2
    sed -n '1,160p' "$stderr_file" >&2
  done < <(find "$output_dir/phases" -type f -name stderr -print 2>/dev/null | sort)
  exit 1
fi

python3 - "$output_dir/summary.json" "$stdout_file" <<'PY'
import json
import sys
from pathlib import Path

summary_path = Path(sys.argv[1])
stdout_path = Path(sys.argv[2])
summary = json.loads(summary_path.read_text(encoding="utf-8"))
stdout_lines = stdout_path.read_text(encoding="utf-8").splitlines()
assert len(stdout_lines) == 1, stdout_lines
assert json.loads(stdout_lines[0]) == summary
assert summary["schema_version"] == 1
assert summary["benchmark"] == "source-backed-codex-v0"
assert summary["status"] == "success"
assert summary["safety"]["home_repurposed"] is False
assert summary["safety"]["sandbox_requested"] == "auto"
assert summary["safety"]["sandbox_mode"] in {"bwrap", "none"}
if summary["safety"]["sandbox_mode"] == "bwrap":
    assert summary["safety"]["corpus_protection"] == "read_only_bubblewrap_bind"
else:
    assert summary["safety"]["corpus_protection"] == (
        "direct_execution_with_before_after_full_inventory_assertion"
    )
assert summary["inventories"]["source_unchanged"] is True
assert summary["inventories"]["source_before"]["file_count"] == 1
assert summary["inventories"]["ctx_data_root_final"]["file_count"] >= 3
assert summary["selected_ids"]["ctx_event_id"] == "11111111-1111-4111-8111-111111111111"
assert summary["selected_ids"]["ctx_session_id"] == "22222222-2222-4222-8222-222222222222"

expected_phases = [
    "01-query-cold",
    "02-show-event-cold",
    "03-show-session-cold",
    "04-query-warm",
    "05-show-event-warm",
    "06-show-session-warm",
]
assert [phase["name"] for phase in summary["phases"]] == expected_phases
for phase in summary["phases"]:
    metrics = phase["time"]["metrics"]
    assert metrics["wall_seconds"] >= 0
    assert metrics["user_seconds"] >= 0
    assert metrics["sys_seconds"] >= 0
    assert metrics["max_rss_kib"] > 0
    assert metrics["exit_status"] == 0
    assert phase["process_exit_status"] == 0
    stdout = summary_path.parent / phase["stdout"]["path"]
    json.loads(stdout.read_text(encoding="utf-8"))

query_phases = [summary["phases"][0], summary["phases"][3]]
for phase in query_phases:
    argv = phase["ctx_argv"]
    assert argv[argv.index("--provider") + 1] == "codex"
    assert argv[argv.index("--backend") + 1] == "lexical"
    assert argv[argv.index("--refresh") + 1] == "wait"
    assert phase["stderr_json"]["json_line_count"] == 2
    assert phase["result_attribution"]["freshness"]["mode"] == "wait"
    assert "phase_attribution" in phase["result_attribution"]["retrieval"]
PY

test ! -e "$corpus/.ctx-benchmark-write-probe"
printf 'source-backed Codex V0 self-test passed\n'
