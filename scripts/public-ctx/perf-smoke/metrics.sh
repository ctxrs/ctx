#!/usr/bin/env bash

perf_smoke_emit_python_statistics() {
  cat <<'PY'
def round2(value: float) -> float:
    return round(value, 2)


def percentile(sorted_samples: list[float], pct: float) -> float:
    if not sorted_samples:
        raise HarnessError("cannot compute percentile for empty samples")
    index = math.ceil((len(sorted_samples) - 1) * (pct / 100.0))
    return sorted_samples[min(index, len(sorted_samples) - 1)]


def timing_stats(samples: list[float]) -> dict[str, object]:
    sorted_samples = sorted(samples)
    return {
        "sample_count": len(samples),
        "samples_ms": [round2(sample) for sample in samples],
        "p50_ms": round2(percentile(sorted_samples, 50.0)),
        "p95_ms": round2(percentile(sorted_samples, 95.0)),
        "min_ms": round2(sorted_samples[0]),
        "max_ms": round2(sorted_samples[-1]),
    }


def integer_stats(samples: list[int]) -> dict[str, object]:
    if not samples:
        raise HarnessError("cannot compute integer statistics without samples")
    ordered = sorted(samples)
    return {
        "sample_count": len(samples),
        "samples": samples,
        "p50": percentile(ordered, 50.0),
        "p95": percentile(ordered, 95.0),
        "min": ordered[0],
        "max": ordered[-1],
        "total": sum(samples),
    }


def float_stats(samples: list[float]) -> dict[str, object]:
    if not samples:
        raise HarnessError("cannot compute numeric statistics without samples")
    ordered = sorted(samples)
    return {
        "sample_count": len(samples),
        "samples": [round2(sample) for sample in samples],
        "p50": round2(percentile(ordered, 50.0)),
        "p95": round2(percentile(ordered, 95.0)),
        "min": round2(ordered[0]),
        "max": round2(ordered[-1]),
        "total": round2(sum(samples)),
    }


PY
}

perf_smoke_emit_python_metrics() {
  cat <<'PY'
def tree_bytes(path: Path) -> int:
    if path.is_file():
        return path.stat().st_size
    total = 0
    try:
        entries = path.rglob("*")
    except OSError:
        return 0
    for entry in entries:
        try:
            if entry.is_file():
                total += entry.stat().st_size
        except OSError:
            continue
    return total


def source_backed_storage_footprint(data_root: Path) -> dict[str, int]:
    relational_files = [
        data_root / "relational.sqlite",
        data_root / "relational.sqlite-wal",
        data_root / "relational.sqlite-shm",
    ]
    sizes = {
        "search/lexical": tree_bytes(data_root / "search" / "lexical"),
        "search/semantic": tree_bytes(data_root / "search" / "semantic"),
        "relational": sum(tree_bytes(path) for path in relational_files),
    }
    sizes["total"] = sum(sizes.values())
    return sizes


def read_proc_io(pid: int) -> dict[str, int] | None:
    path = Path("/proc") / str(pid) / "io"
    try:
        lines = path.read_text(encoding="ascii").splitlines()
    except (FileNotFoundError, PermissionError, ProcessLookupError):
        return None
    values: dict[str, int] = {}
    for line in lines:
        key, separator, raw = line.partition(":")
        if separator and key in PROC_IO_FIELDS:
            try:
                values[key] = int(raw.strip())
            except ValueError:
                continue
    return values if values else None


class MetricSampler:
    def __init__(self, pid: int, data_root: Path, interval_ms: int) -> None:
        self.pid = pid
        self.data_root = data_root
        self.interval_seconds = interval_ms / 1000.0
        self.stop_event = threading.Event()
        self.proc_io_max = {field: 0 for field in PROC_IO_FIELDS}
        self.proc_io_available = False
        self.storage_start = source_backed_storage_footprint(data_root)
        self.storage_max = dict(self.storage_start)
        self.thread = threading.Thread(target=self._monitor, daemon=True)
        self.sample()
        self.thread.start()

    def sample(self) -> None:
        proc_io = read_proc_io(self.pid)
        if proc_io is not None:
            self.proc_io_available = True
            for field in PROC_IO_FIELDS:
                self.proc_io_max[field] = max(self.proc_io_max[field], proc_io.get(field, 0))
        sizes = source_backed_storage_footprint(self.data_root)
        for name, size in sizes.items():
            self.storage_max[name] = max(self.storage_max.get(name, 0), size)

    def _monitor(self) -> None:
        while not self.stop_event.wait(self.interval_seconds):
            self.sample()

    def finish(self) -> dict[str, object]:
        self.sample()
        self.stop_event.set()
        self.thread.join()
        storage_end = source_backed_storage_footprint(self.data_root)
        for name, size in storage_end.items():
            self.storage_max[name] = max(self.storage_max.get(name, 0), size)
        storage_start = self.storage_start["total"]
        storage_high = self.storage_max["total"]
        return {
            "sampling_interval_ms": round2(self.interval_seconds * 1000.0),
            "proc_io_available": self.proc_io_available,
            "proc_io": dict(self.proc_io_max) if self.proc_io_available else None,
            "source_backed_storage_start_bytes": self.storage_start,
            "source_backed_storage_high_water_bytes": self.storage_max,
            "source_backed_storage_end_bytes": storage_end,
            "source_backed_storage_growth_high_water_bytes": max(
                0,
                storage_high - storage_start,
            ),
        }


class TrackedProcess:
    def __init__(
        self,
        ctx_bin: Path,
        args: list[str],
        env: dict[str, str],
        data_root: Path,
        sampling_interval_ms: int,
    ) -> None:
        self.ctx_bin = ctx_bin
        self.args = args
        self.command = [str(ctx_bin), *args]
        self.timeout_seconds = env_float("CTX_PERF_SMOKE_COMMAND_TIMEOUT_SECONDS", 300.0, 1.0)
        self.total_timeout_seconds = env_float(
            "CTX_PERF_SMOKE_TOTAL_TIMEOUT_SECONDS", 1800.0, 1.0
        )
        process_temp_root = data_root.parent / "tmp"
        self.stdout_file = tempfile.TemporaryFile(mode="w+b", dir=process_temp_root)
        self.stderr_file = tempfile.TemporaryFile(mode="w+b", dir=process_temp_root)
        self.started = time.perf_counter()
        self.process = subprocess.Popen(
            self.command,
            cwd=REPO_ROOT,
            env=env,
            stdout=self.stdout_file,
            stderr=self.stderr_file,
        )
        self.sampler = MetricSampler(self.process.pid, data_root, sampling_interval_ms)

    def finish(self) -> dict[str, object]:
        if not hasattr(os, "wait4") or not hasattr(os, "waitid") or not hasattr(os, "WNOWAIT"):
            self.process.kill()
            self.process.wait()
            raise HarnessError("per-process measurements require wait4/waitid WNOWAIT support")

        command_deadline = self.started + self.timeout_seconds
        harness_deadline = HARNESS_STARTED + self.total_timeout_seconds
        deadline = min(command_deadline, harness_deadline)
        timed_out = threading.Event()

        def terminate_at_deadline() -> None:
            try:
                self.process.kill()
                timed_out.set()
            except ProcessLookupError:
                return

        timeout_timer = threading.Timer(
            max(0.0, deadline - time.perf_counter()),
            terminate_at_deadline,
        )
        timeout_timer.daemon = True
        timeout_timer.start()
        os.waitid(os.P_PID, self.process.pid, os.WEXITED | os.WNOWAIT)
        timeout_timer.cancel()
        timeout_timer.join()
        self.sampler.sample()
        waited_pid, wait_status, usage = os.wait4(self.process.pid, 0)
        if waited_pid != self.process.pid:
            raise HarnessError(f"wait4 returned unexpected pid {waited_pid}")
        return_code = os.waitstatus_to_exitcode(wait_status)
        self.process.returncode = return_code
        wall_ms = (time.perf_counter() - self.started) * 1000.0
        sampled = self.sampler.finish()

        self.stdout_file.seek(0)
        stdout = self.stdout_file.read().decode("utf-8", errors="replace")
        self.stderr_file.seek(0)
        stderr = self.stderr_file.read().decode("utf-8", errors="replace")
        self.stdout_file.close()
        self.stderr_file.close()

        peak_rss_bytes = int(usage.ru_maxrss)
        if sys.platform.startswith("linux"):
            peak_rss_bytes *= 1024
        proc_io = sampled["proc_io"] if sampled["proc_io_available"] else None
        device_read_bytes = int(proc_io["read_bytes"]) if proc_io is not None else None
        device_write_bytes = int(proc_io["write_bytes"]) if proc_io is not None else None
        metrics = {
            "wall_ms": round2(wall_ms),
            "user_cpu_ms": round2(float(usage.ru_utime) * 1000.0),
            "system_cpu_ms": round2(float(usage.ru_stime) * 1000.0),
            "cpu_total_ms": round2((float(usage.ru_utime) + float(usage.ru_stime)) * 1000.0),
            "peak_rss_bytes": peak_rss_bytes,
            "filesystem_read_chars": int(proc_io["rchar"]) if proc_io is not None else None,
            "filesystem_write_chars": int(proc_io["wchar"]) if proc_io is not None else None,
            "device_read_bytes": device_read_bytes,
            "device_write_bytes": device_write_bytes,
            "device_total_io_bytes": (
                device_read_bytes + device_write_bytes
                if device_read_bytes is not None and device_write_bytes is not None
                else None
            ),
            "cancelled_device_write_bytes": (
                int(proc_io["cancelled_write_bytes"]) if proc_io is not None else None
            ),
            "block_input_proxy_bytes": int(usage.ru_inblock) * 512,
            "block_output_proxy_bytes": int(usage.ru_oublock) * 512,
            "source_backed_storage_high_water_bytes": int(
                sampled["source_backed_storage_high_water_bytes"]["total"]
            ),
            "source_backed_storage_growth_high_water_bytes": int(
                sampled["source_backed_storage_growth_high_water_bytes"]
            ),
        }
        if timed_out.is_set():
            raise HarnessError(
                "ctx performance harness command exceeded its command/total timeout: "
                f"{' '.join(self.command)}"
            )
        return {
            "args": self.args,
            "command": " ".join(self.command),
            "returncode": return_code,
            "stdout": stdout,
            "stderr": stderr,
            "metrics": metrics,
            "sampling": sampled,
        }


def start_ctx(
    ctx_bin: Path,
    args: list[str],
    env: dict[str, str],
    data_root: Path,
    sampling_interval_ms: int,
) -> TrackedProcess:
    return TrackedProcess(ctx_bin, args, env, data_root, sampling_interval_ms)


def finish_ctx(process: TrackedProcess) -> dict[str, object]:
    result = process.finish()
    if result["returncode"] != 0:
        raise HarnessError(
            "ctx command failed\n"
            f"command: {result['command']}\n"
            f"exit: {result['returncode']}\n"
            f"stdout:\n{result['stdout']}\n"
            f"stderr:\n{result['stderr']}"
        )
    try:
        packet = json.loads(str(result["stdout"]))
    except json.JSONDecodeError as exc:
        raise HarnessError(
            f"ctx command did not return JSON: {result['command']}\n{result['stdout']}"
        ) from exc
    if not isinstance(packet, dict):
        raise HarnessError(f"ctx command returned non-object JSON: {result['command']}")
    result["packet"] = packet
    return result


def run_ctx(
    ctx_bin: Path,
    args: list[str],
    env: dict[str, str],
    data_root: Path,
    sampling_interval_ms: int,
) -> dict[str, object]:
    return finish_ctx(start_ctx(ctx_bin, args, env, data_root, sampling_interval_ms))


def command_string(ctx_bin: Path, args: list[str]) -> str:
    rendered = [str(ctx_bin), *args]
    return " ".join(rendered)


def measure(
    label: str,
    ctx_bin: Path,
    args: list[str],
    repeats: int,
    env: dict[str, str],
    data_root: Path,
    sampling_interval_ms: int,
    validate,
) -> tuple[dict[str, object], dict[str, object], list[dict[str, object]]]:
    results: list[dict[str, object]] = []
    last: dict[str, object] | None = None
    for _ in range(repeats):
        result = run_ctx(ctx_bin, args, env, data_root, sampling_interval_ms)
        packet = result["packet"]
        validate(packet)
        results.append(result)
        last = packet
    if last is None:
        raise HarnessError(f"{label} collected no samples")
    return command_profile(ctx_bin, args, results), last, results


PY
}
