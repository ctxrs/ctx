#!/usr/bin/env bash

perf_smoke_emit_python_validation_oracles() {
  cat <<'PY'
def expect_source_refresh(
    packet: dict[str, object],
    *,
    changed: bool,
) -> dict[str, object]:
    jobs = packet.get("jobs")
    refresh = jobs.get("source_backed_refresh") if isinstance(jobs, dict) else None
    if (
        packet.get("status") != "completed"
        or not isinstance(refresh, dict)
        or refresh.get("status") != "completed"
        or refresh.get("request_state") != "published"
        or refresh.get("generation_changed") is not changed
        or not isinstance(refresh.get("published_generation"), str)
        or int(refresh.get("source_count", 0)) < 1
        or int(refresh.get("certified_source_count", 0)) < 1
    ):
        raise HarnessError(f"daemon did not publish the expected source refresh: {packet}")
    return {
        "generation_changed": changed,
        "published_generation": refresh["published_generation"],
        "source_count": refresh["source_count"],
        "certified_source_count": refresh["certified_source_count"],
        "certified_source_bytes": refresh.get("certified_source_bytes"),
        "scanned_routes": refresh.get("scanned_routes"),
        "timings_us": refresh.get("timings_us"),
    }


def expect_source_backed_status(
    packet: dict[str, object],
    data_root: Path,
    expected_sessions: int,
    expected_events: int,
) -> None:
    history_epoch = packet.get("history_epoch")
    lexical = packet.get("lexical")
    catalog = packet.get("catalog")
    semantic = packet.get("semantic")
    relational = packet.get("relational")
    if packet.get("schema_version") != 2 or packet.get("initialized") is not True:
        raise HarnessError(f"status is not a ready v0.26 source epoch: {packet}")
    if (
        not isinstance(history_epoch, dict)
        or history_epoch.get("name") != "v0.26_source_backed"
        or history_epoch.get("status") != "ready"
        or history_epoch.get("origin") != "fresh"
        or history_epoch.get("phase") != "ready"
    ):
        raise HarnessError(f"status has an unexpected source epoch: {history_epoch}")
    lexical_path = data_root / "search" / "lexical"
    if (
        not isinstance(lexical, dict)
        or lexical.get("status") != "ready"
        or lexical.get("reason") is not None
        or lexical.get("path") != str(lexical_path)
        or int(lexical.get("indexed_documents", -1)) != expected_events
        or not lexical_path.is_dir()
        or not (lexical_path / "meta.json").is_file()
    ):
        raise HarnessError(f"status has an unexpected lexical generation path: {lexical}")
    generation_id = lexical.get("generation_id")
    if (
        not isinstance(catalog, dict)
        or catalog.get("status") != "ready"
        or catalog.get("generation_matches") is not True
        or catalog.get("generation_id") != generation_id
        or int(catalog.get("certified_sources", 0)) < 1
    ):
        raise HarnessError(f"status has an unexpected source catalog: {catalog}")
    flat_f32 = semantic.get("flat_f32") if isinstance(semantic, dict) else None
    semantic_path = data_root / "search" / "semantic"
    if (
        not isinstance(semantic, dict)
        or semantic.get("enabled") is not False
        or semantic.get("status") != "disabled"
        or not isinstance(flat_f32, dict)
        or flat_f32.get("status") != "disabled"
        or flat_f32.get("path") != str(semantic_path)
        or semantic_path.exists()
    ):
        raise HarnessError(f"status has an unexpected semantic generation path: {semantic}")
    relational_path = data_root / "relational.sqlite"
    if (
        not isinstance(relational, dict)
        or relational.get("status") != "ready"
        or relational.get("projection_status") != "ready"
        or relational.get("generation_matches") is not True
        or relational.get("active_core_generation_id") != generation_id
        or relational.get("path") != str(relational_path)
        or int(relational.get("session_count", -1)) != expected_sessions
        or int(relational.get("event_count", -1)) != expected_events
        or not relational_path.is_file()
    ):
        raise HarnessError(f"status has an unexpected relational projection path: {relational}")


def effective_role(role: str, version: str) -> str:
    if role not in {"baseline", "candidate", "single"}:
        raise HarnessError(f"unknown performance role: {role}")
    if version != "ctx 1.0.0":
        raise HarnessError(
            f"{role} performance binary must be a ctx 1.0.0 source-backed build, got {version}"
        )
    return role


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
            "source_backed_storage_high_water_bytes": integer_stats(
                [
                    int(result_metric(result, "source_backed_storage_high_water_bytes"))
                    for result in results
                ]
            ),
            "source_backed_storage_growth_high_water_bytes": integer_stats(
                [
                    int(
                        result_metric(
                            result,
                            "source_backed_storage_growth_high_water_bytes",
                        )
                    )
                    for result in results
                ]
            ),
        },
    }


def source_backed_footprint_bytes(data_root: Path) -> int:
    return source_backed_storage_footprint(data_root)["total"]


def storage_sample(label: str, data_root: Path, corpus_events: int) -> dict[str, object]:
    files = source_backed_storage_footprint(data_root)
    footprint = files["total"]
    return {
        "label": label,
        "files": files,
        "source_backed_footprint_bytes": footprint,
        "source_backed_bytes_per_generated_event": round2(
            footprint / max(corpus_events, 1)
        ),
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

    if observed_by_role["baseline"] == observed_by_role["candidate"]:
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


def source_refresh_command() -> list[str]:
    return ["daemon", "run", "--once", "--force", "--format=json"]


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
        "policy": "relative_regression",
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
            else (
                "candidate must be "
                f"{'at most' if inclusive else 'below'} {max_ratio:.2f}x baseline"
            )
        ),
    }


PY
}
