#!/usr/bin/env bash

perf_smoke_emit_python_validation_oracles() {
  cat <<'PY'
def expect_import_totals(packet: dict[str, object]) -> dict[str, int]:
    totals = packet.get("totals")
    if not isinstance(totals, dict):
        raise HarnessError(f"import output is missing totals: {packet}")
    failed = int(totals.get("failed", 0))
    failed_sources = int(totals.get("failed_sources", 0))
    if failed or failed_sources:
        raise HarnessError(f"import reported failures: {totals}")
    return {key: int(value) for key, value in totals.items() if isinstance(value, int)}


def profile_summary(packet: dict[str, object]) -> dict[str, object]:
    totals = expect_import_totals(packet)
    return {
        "source_files": totals.get("source_files", 0),
        "source_bytes": totals.get("source_bytes", 0),
        "imported_sessions": totals.get("imported_sessions", 0),
        "imported_events": totals.get("imported_events", 0),
        "imported_edges": totals.get("imported_edges", 0),
        "skipped": totals.get("skipped", 0),
    }


def expect_exact_import_delta(
    summary: dict[str, object],
    label: str,
    sessions: int,
    events: int,
    edges: int = 0,
) -> None:
    expected = {
        "imported_sessions": sessions,
        "imported_events": events,
        "imported_edges": edges,
    }
    actual = {key: summary[key] for key in expected}
    if actual != expected:
        raise HarnessError(f"{label} imported unexpected fixture totals: {actual} != {expected}")


def effective_role(role: str, version: str) -> str:
    if role == "baseline-v0.25":
        if version != EXACT_BASELINE_VERSION:
            raise HarnessError(
                "comparison baseline must be the exact v0.25 release version: "
                f"expected {EXACT_BASELINE_VERSION!r}, got {version!r}"
            )
        return role
    if role == "candidate":
        if version == EXACT_BASELINE_VERSION:
            raise HarnessError(
                "candidate resolves to the v0.25 baseline version; supply the candidate binary"
            )
        return role
    if role == "single":
        return "baseline-v0.25" if version == EXACT_BASELINE_VERSION else "candidate"
    raise HarnessError(f"unknown performance role: {role}")


def append_expectation(role: str, changed_files: int) -> dict[str, object]:
    if role == "baseline-v0.25":
        return {
            "role": role,
            "imported_sessions": changed_files,
            "imported_events": changed_files,
            "imported_edges": 0,
            "shape": "v0.25 reports each appended source as an imported session",
        }
    if role == "candidate":
        return {
            "role": role,
            "imported_sessions": 0,
            "imported_events": changed_files,
            "imported_edges": 0,
            "shape": "candidate reports appended events without re-importing the session",
        }
    raise HarnessError(f"append expectation is unavailable for role: {role}")


def result_metric(result: dict[str, object], name: str) -> int | float:
    metrics = result["metrics"]
    value = metrics[name]
    if not isinstance(value, (int, float)):
        raise HarnessError(f"measurement {name} is unavailable for {result['command']}")
    return value


def command_profile(
    ctx_bin: Path,
    args: list[str],
    results: list[dict[str, object]],
) -> dict[str, object]:
    return {
        "command": command_string(ctx_bin, args),
        "timings": timing_stats([float(result_metric(result, "wall_ms")) for result in results]),
        "resources": {
            "user_cpu_ms": float_stats(
                [float(result_metric(result, "user_cpu_ms")) for result in results]
            ),
            "system_cpu_ms": float_stats(
                [float(result_metric(result, "system_cpu_ms")) for result in results]
            ),
            "cpu_total_ms": float_stats(
                [float(result_metric(result, "cpu_total_ms")) for result in results]
            ),
            "peak_rss_bytes": integer_stats(
                [int(result_metric(result, "peak_rss_bytes")) for result in results]
            ),
            "filesystem_read_chars": integer_stats(
                [int(result_metric(result, "filesystem_read_chars")) for result in results]
            ),
            "filesystem_write_chars": integer_stats(
                [int(result_metric(result, "filesystem_write_chars")) for result in results]
            ),
            "device_read_bytes": integer_stats(
                [int(result_metric(result, "device_read_bytes")) for result in results]
            ),
            "device_write_bytes": integer_stats(
                [int(result_metric(result, "device_write_bytes")) for result in results]
            ),
            "device_total_io_bytes": integer_stats(
                [int(result_metric(result, "device_total_io_bytes")) for result in results]
            ),
            "cancelled_device_write_bytes": integer_stats(
                [int(result_metric(result, "cancelled_device_write_bytes")) for result in results]
            ),
            "block_input_proxy_bytes": integer_stats(
                [int(result_metric(result, "block_input_proxy_bytes")) for result in results]
            ),
            "block_output_proxy_bytes": integer_stats(
                [int(result_metric(result, "block_output_proxy_bytes")) for result in results]
            ),
            "wal_high_water_bytes": integer_stats(
                [int(result_metric(result, "wal_high_water_bytes")) for result in results]
            ),
            "wal_growth_high_water_bytes": integer_stats(
                [int(result_metric(result, "wal_growth_high_water_bytes")) for result in results]
            ),
        },
    }


def db_footprint_bytes(data_root: Path) -> int:
    return sum(sqlite_footprint(data_root).values())


def storage_sample(label: str, data_root: Path, corpus_events: int) -> dict[str, object]:
    files = sqlite_footprint(data_root)
    footprint = sum(files.values())
    return {
        "label": label,
        "files": files,
        "db_footprint_bytes": footprint,
        "db_bytes_per_generated_event": round2(footprint / max(corpus_events, 1)),
    }


def file_tree_bytes(root: Path) -> int:
    total = 0
    for path in root.rglob("*"):
        if path.is_file():
            total += path.stat().st_size
    return total


def binary_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def expected_sha256(role: str, required: bool) -> tuple[str, str | None]:
    env_name = EXPECTED_SHA256_ENV_BY_ROLE[role]
    value = os.environ.get(env_name)
    if value is None or value == "":
        if required:
            raise HarnessError(
                f"{env_name} is required for enforced baseline/candidate comparisons"
            )
        return env_name, None
    if re.fullmatch(r"[0-9a-f]{64}", value) is None:
        raise HarnessError(
            f"{env_name} must be exactly 64 lowercase hexadecimal characters"
        )
    return env_name, value


def preflight_binary_hashes(
    run_mode: str,
    enforce: bool,
    run_specs: list[tuple[Path, str, str]],
) -> dict[str, object]:
    bindings: list[dict[str, object]] = []
    observed_by_role: dict[str, str] = {}
    if run_mode == "single":
        ctx_bin, label, role = run_specs[0]
        observed = binary_sha256(ctx_bin)
        bindings.append(
            {
                "role": role,
                "label": label,
                "path": str(ctx_bin),
                "expected_sha256_env": None,
                "expected_sha256": None,
                "observed_sha256": observed,
                "matched": None,
            }
        )
        return {
            "status": "observed-only",
            "required": False,
            "bindings": bindings,
        }

    if run_mode != "comparison":
        raise HarnessError(f"unknown run mode during binary hash preflight: {run_mode}")

    for ctx_bin, label, role in run_specs:
        env_name, expected = expected_sha256(role, enforce)
        observed = binary_sha256(ctx_bin)
        if expected is not None and observed != expected:
            raise HarnessError(
                f"{env_name} does not match {role} binary bytes: "
                f"expected {expected}, observed {observed}"
            )
        observed_by_role[role] = observed
        bindings.append(
            {
                "role": role,
                "label": label,
                "path": str(ctx_bin),
                "expected_sha256_env": env_name,
                "expected_sha256": expected,
                "observed_sha256": observed,
                "matched": observed == expected if expected is not None else None,
            }
        )

    if observed_by_role["baseline-v0.25"] == observed_by_role["candidate"]:
        raise HarnessError("baseline and candidate binaries must be distinct")
    return {
        "status": (
            "verified"
            if all(binding["matched"] is True for binding in bindings)
            else "observed-only"
        ),
        "required": enforce,
        "bindings": bindings,
    }


def ctx_version(ctx_bin: Path, env: dict[str, str]) -> str:
    command_timeout = env_float("CTX_PERF_SMOKE_COMMAND_TIMEOUT_SECONDS", 300.0, 1.0)
    total_timeout = env_float("CTX_PERF_SMOKE_TOTAL_TIMEOUT_SECONDS", 1800.0, 1.0)
    remaining_total = HARNESS_STARTED + total_timeout - time.perf_counter()
    timeout = min(command_timeout, remaining_total)
    if timeout <= 0:
        raise HarnessError("ctx performance harness reached its total timeout before --version")
    try:
        completed = subprocess.run(
            [str(ctx_bin), "--version"],
            cwd=REPO_ROOT,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as exc:
        raise HarnessError(f"ctx --version exceeded its timeout: {ctx_bin}") from exc
    except subprocess.CalledProcessError as exc:
        raise HarnessError(
            f"ctx --version failed for {ctx_bin}: {exc.stderr.strip()}"
        ) from exc
    return completed.stdout.strip()


def import_command(corpus_root: Path, resume: bool = False) -> list[str]:
    args = [
        "import",
        "--provider",
        "codex",
        "--path",
        str(corpus_root),
        "--no-daemon",
        "--json",
        "--progress",
        "none",
    ]
    if resume:
        args.insert(5, "--resume")
    return args


def process_is_active(pid: int) -> bool:
    stat_path = Path("/proc") / str(pid) / "stat"
    try:
        value = stat_path.read_text(encoding="ascii")
    except (FileNotFoundError, PermissionError, ProcessLookupError):
        return False
    _, separator, suffix = value.rpartition(")")
    if not separator:
        return False
    fields = suffix.strip().split()
    return bool(fields) and fields[0] not in {"Z", "X"}


PY
}

perf_smoke_emit_python_validation_policy() {
  cat <<'PY'
def percent_change(baseline: float, head: float) -> float | None:
    if baseline == 0:
        return None
    return round2(((head - baseline) / baseline) * 100.0)


def lookup_metric(run: dict[str, object], phase: str, group: str, metric: str) -> float:
    value = run["profiles"][phase][group][metric]
    if not isinstance(value, (int, float)):
        raise HarnessError(f"comparison metric is unavailable: {phase}.{group}.{metric}")
    return float(value)


def lookup_resource(
    run: dict[str, object],
    phase: str,
    resource: str,
    statistic: str,
) -> float:
    value = run["profiles"][phase]["resources"][resource][statistic]
    if not isinstance(value, (int, float)):
        raise HarnessError(
            f"comparison resource is unavailable: {phase}.{resource}.{statistic}"
        )
    return float(value)


def regression_check(
    name: str,
    baseline: float,
    head: float,
    allowed_regression_pct: float,
    unit: str,
) -> dict[str, object]:
    if baseline == 0:
        passed = head == 0
        threshold = 0.0
    else:
        threshold = baseline * (1.0 + allowed_regression_pct / 100.0)
        passed = head <= threshold
    return {
        "name": name,
        "policy": "legacy_relative_regression",
        "unit": unit,
        "baseline": round2(baseline),
        "candidate": round2(head),
        "head": round2(head),
        "change_pct": percent_change(baseline, head),
        "threshold": round2(threshold),
        "allowed_regression_pct": allowed_regression_pct,
        "passed": passed,
    }


def absolute_limit_check(
    name: str,
    baseline: float,
    head: float,
    limit: float,
    unit: str,
) -> dict[str, object]:
    return {
        "name": name,
        "policy": "absolute_resource_limit",
        "unit": unit,
        "baseline": round2(baseline),
        "candidate": round2(head),
        "head": round2(head),
        "change_pct": percent_change(baseline, head),
        "threshold": round2(limit),
        "passed": head <= limit,
    }


def ratio_check(
    name: str,
    baseline: float,
    candidate: float,
    max_ratio: float,
    unit: str,
    policy: str,
    *,
    inclusive: bool,
) -> dict[str, object]:
    if baseline == 0:
        ratio = 1.0 if candidate == 0 else None
        passed = candidate == 0
        threshold = 0.0
    else:
        ratio = candidate / baseline
        threshold = baseline * max_ratio
        passed = ratio <= max_ratio if inclusive else ratio < max_ratio
    return {
        "name": name,
        "policy": policy,
        "unit": unit,
        "baseline": round2(baseline),
        "candidate": round2(candidate),
        "head": round2(candidate),
        "change_pct": percent_change(baseline, candidate),
        "ratio": round(ratio, 6) if ratio is not None else None,
        "max_ratio": max_ratio,
        "threshold": round2(threshold),
        "comparator": "<=" if inclusive else "<",
        "passed": passed,
        "reason": (
            None
            if passed
            else f"candidate must be {'at most' if inclusive else 'below'} {max_ratio:.2f}x v0.25"
        ),
    }


PY
}
