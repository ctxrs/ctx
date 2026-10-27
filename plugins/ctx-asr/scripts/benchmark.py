#!/usr/bin/env python3
"""Measure hook process completion, including interpreter startup and writes."""

import argparse
import json
import os
from pathlib import Path
import statistics
import subprocess
import sys
import tempfile
import time


def distribution(values):
    ordered = sorted(values)
    return {"n": len(ordered), "medianMs": round(statistics.median(ordered), 3),
            "p95Ms": round(ordered[min(len(ordered) - 1, int(len(ordered) * .95))], 3),
            "maxMs": round(max(ordered), 3)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--iterations", type=int, default=100)
    args = parser.parse_args()
    if not 1 <= args.iterations <= 10000:
        parser.error("iterations must be between 1 and 10000")
    script = Path(__file__).with_name("observer.py")
    report = {}
    with tempfile.TemporaryDirectory(prefix="ctx-asr-benchmark-") as temp:
        env = {**os.environ, "CTX_ASR_DIR": temp}
        for event, extra in [
            ("UserPromptSubmit", {"prompt": "synthetic prompt"}),
            ("PreToolUse", {"tool_name": "Bash", "tool_use_id": "toolu_Benchmark", "tool_input": {"command": "true"}}),
            ("PostToolUse", {"tool_name": "Bash", "tool_use_id": "toolu_Benchmark", "tool_response": {"stdout": "x" * 8192}}),
            ("Stop", {"last_assistant_message": "synthetic reply"}),
            ("SessionEnd", {"reason": "other"}),
        ]:
            payload = json.dumps({"session_id": "00000000-0000-4000-8000-000000000001",
                                  "hook_event_name": event, "cwd": "/synthetic/benchmark",
                                  "permission_mode": "default", **extra}).encode()
            times = []
            for i in range(args.iterations + 5):
                start = time.perf_counter_ns()
                result = subprocess.run([sys.executable, "-I", "-S", str(script)],
                                        input=payload, capture_output=True, env=env, timeout=2)
                elapsed = (time.perf_counter_ns() - start) / 1e6
                if result.returncode or result.stdout or result.stderr:
                    raise RuntimeError("hook did not fail open silently")
                if i >= 5:
                    times.append(elapsed)
            report[event] = distribution(times)
        path = Path(temp) / "00000000-0000-4000-8000-000000000001.asr.jsonl"
        rows = [json.loads(line) for line in path.read_text().splitlines()]
        report["records"] = len(rows)
        report["capturedHooks"] = sum(r["type"] == "hook" for r in rows)
        report["expectedHooks"] = 5 * (args.iterations + 5)
        report["streamBytes"] = path.stat().st_size
        report["stateBytes"] = sum(p.stat().st_size for p in Path(temp).glob("*.state.json"))
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
