#!/usr/bin/env python3
"""Run one command under Darwin /usr/bin/time -l and emit a strict receipt."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from decimal import Decimal, InvalidOperation
import json
import math
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tempfile
from typing import BinaryIO, NoReturn, Sequence


SCHEMA = "ctx.perf.macos.time_l.v1"
SCHEMA_VERSION = 1
ADAPTER_ERROR_EXIT = 125
TIME = Path("/usr/bin/time")
ABNORMAL_MARKER = b"Command terminated abnormally."

FLOAT_LINE = re.compile(rb"^(real|user|sys) ([0-9]+(?:\.[0-9]+)?)\r?\n?$")
INTEGER_METRIC_LINE = re.compile(rb"^\s*([0-9]+)\s+(.+?)\s*\r?\n?$")
REQUIRED_LONG_METRICS = {
    "maximum resident set size": "maximum_resident_set_size_bytes",
    "block input operations": "block_input_operations",
    "block output operations": "block_output_operations",
}
UNITS = {
    "block_input_operations": "operations",
    "block_output_operations": "operations",
    "maximum_resident_set_size_bytes": "bytes",
    "system_cpu_seconds": "seconds",
    "user_cpu_seconds": "seconds",
    "wall_time_seconds": "seconds",
}
COLLECTOR = {
    "arguments": ["-l"],
    "io_contract": "darwin_ru_inblock_ru_oublock_operations_v1",
    "platform": "macos",
    "tool": "/usr/bin/time",
}


class MeasurementError(Exception):
    """A deterministic adapter or parser failure."""

    def __init__(
        self,
        code: str,
        message: str,
        *,
        fields: Sequence[str] = (),
    ) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.fields = tuple(sorted(fields))


@dataclass(frozen=True)
class ParsedTime:
    command_stderr: bytes
    timing_output: bytes
    wall_time_seconds: float
    user_cpu_seconds: float
    system_cpu_seconds: float
    maximum_resident_set_size_bytes: int
    block_input_operations: int
    block_output_operations: int
    command_terminated_abnormally: bool

    def metrics(self) -> dict[str, float | int]:
        return {
            "block_input_operations": self.block_input_operations,
            "block_output_operations": self.block_output_operations,
            "maximum_resident_set_size_bytes": self.maximum_resident_set_size_bytes,
            "system_cpu_seconds": self.system_cpu_seconds,
            "user_cpu_seconds": self.user_cpu_seconds,
            "wall_time_seconds": self.wall_time_seconds,
        }


def _parse_seconds(raw: bytes, field: str) -> float:
    try:
        parsed = float(Decimal(raw.decode("ascii")))
    except (InvalidOperation, UnicodeDecodeError, ValueError) as error:
        raise MeasurementError(
            "malformed_required_metric",
            f"{field} is not a decimal number",
            fields=(field,),
        ) from error
    if not math.isfinite(parsed) or parsed < 0:
        raise MeasurementError(
            "malformed_required_metric",
            f"{field} is not a finite non-negative number",
            fields=(field,),
        )
    return parsed


def parse_time_l(raw: bytes) -> ParsedTime:
    """Parse the final native time(1) block and preserve command stderr bytes."""

    lines = raw.splitlines(keepends=True)
    real_indexes = [
        index
        for index, line in enumerate(lines)
        if (match := FLOAT_LINE.fullmatch(line)) is not None
        and match.group(1) == b"real"
    ]
    if not real_indexes:
        raise MeasurementError(
            "missing_time_header",
            "native time output does not contain a real/user/sys header",
        )

    real_index = real_indexes[-1]
    if real_index + 2 >= len(lines):
        raise MeasurementError(
            "malformed_time_header",
            "native time output ends before the real/user/sys header is complete",
        )

    header: dict[bytes, bytes] = {}
    for offset, expected in enumerate((b"real", b"user", b"sys")):
        match = FLOAT_LINE.fullmatch(lines[real_index + offset])
        if match is None or match.group(1) != expected:
            raise MeasurementError(
                "malformed_time_header",
                "native time output must contain consecutive real, user, and sys lines",
            )
        header[expected] = match.group(2)

    abnormal = (
        real_index > 0
        and lines[real_index - 1].rstrip(b"\r\n") == ABNORMAL_MARKER
    )
    timing_index = real_index - 1 if abnormal else real_index
    command_stderr = b"".join(lines[:timing_index])
    timing_output = b"".join(lines[timing_index:])

    required_values: dict[str, int] = {}
    for line in lines[real_index + 3 :]:
        if not line.strip():
            continue
        match = INTEGER_METRIC_LINE.fullmatch(line)
        if match is None:
            raise MeasurementError(
                "malformed_time_long_metric",
                "native time long output contains a non-integer metric line",
            )
        try:
            label = match.group(2).decode("ascii")
        except UnicodeDecodeError as error:
            raise MeasurementError(
                "malformed_time_long_metric",
                "native time long output contains a non-ASCII metric label",
            ) from error
        field = REQUIRED_LONG_METRICS.get(label)
        if field is None:
            continue
        if field in required_values:
            raise MeasurementError(
                "duplicate_required_metric",
                f"native time output repeats {field}",
                fields=(field,),
            )
        required_values[field] = int(match.group(1))

    missing = sorted(set(REQUIRED_LONG_METRICS.values()) - required_values.keys())
    if missing:
        raise MeasurementError(
            "missing_required_metric",
            "native time output is missing normative metrics",
            fields=missing,
        )

    return ParsedTime(
        command_stderr=command_stderr,
        timing_output=timing_output,
        wall_time_seconds=_parse_seconds(header[b"real"], "wall_time_seconds"),
        user_cpu_seconds=_parse_seconds(header[b"user"], "user_cpu_seconds"),
        system_cpu_seconds=_parse_seconds(header[b"sys"], "system_cpu_seconds"),
        maximum_resident_set_size_bytes=required_values[
            "maximum_resident_set_size_bytes"
        ],
        block_input_operations=required_values["block_input_operations"],
        block_output_operations=required_values["block_output_operations"],
        command_terminated_abnormally=abnormal,
    )


def canonical_json(value: object) -> bytes:
    return (
        json.dumps(
            value,
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode("utf-8")
        + b"\n"
    )


def status_record(parsed: ParsedTime, time_exit_code: int) -> dict[str, object]:
    if parsed.command_terminated_abnormally:
        signal_number = -time_exit_code if time_exit_code < 0 else None
        return {
            "exit_code": None,
            "kind": "signaled",
            "signal": signal_number,
            "signal_unavailable_reason": (
                None
                if signal_number is not None
                else "not_reported_by_darwin_time_l"
            ),
            "time_exit_code": time_exit_code if time_exit_code >= 0 else None,
        }
    return {
        "exit_code": time_exit_code,
        "kind": "exited",
        "signal": None,
        "signal_unavailable_reason": None,
        "time_exit_code": time_exit_code,
    }


def success_record(
    command: Sequence[str],
    parsed: ParsedTime,
    time_exit_code: int,
) -> dict[str, object]:
    return {
        "collector": COLLECTOR,
        "command": list(command),
        "metrics": parsed.metrics(),
        "schema": SCHEMA,
        "schema_version": SCHEMA_VERSION,
        "status": status_record(parsed, time_exit_code),
        "units": UNITS,
        "valid": True,
    }


def invalid_record(
    command: Sequence[str],
    error: MeasurementError,
    *,
    time_exit_code: int | None = None,
) -> dict[str, object]:
    detail: dict[str, object] = {
        "code": error.code,
        "message": error.message,
    }
    if error.fields:
        detail["fields"] = list(error.fields)
    return {
        "collector": COLLECTOR,
        "command": list(command),
        "error": detail,
        "schema": SCHEMA,
        "schema_version": SCHEMA_VERSION,
        "status": {
            "kind": "unavailable",
            "time_exit_code": time_exit_code,
        },
        "valid": False,
    }


def _stage_for(target: Path) -> tuple[BinaryIO, Path]:
    target.parent.resolve(strict=True)
    handle = tempfile.NamedTemporaryFile(
        mode="w+b",
        prefix=f".{target.name}.perf-macos.",
        dir=target.parent,
        delete=False,
    )
    path = Path(handle.name)
    os.chmod(path, 0o600)
    return handle, path


def _publish(stage: Path, target: Path) -> None:
    if target.exists():
        raise MeasurementError(
            "output_exists",
            f"refusing to replace existing output: {target}",
        )
    try:
        os.link(stage, target)
    except FileExistsError as error:
        raise MeasurementError(
            "output_exists",
            f"refusing to replace existing output: {target}",
        ) from error
    os.chmod(target, 0o644)
    stage.unlink()


def _write_staged(target: Path, content: bytes) -> Path:
    handle, stage = _stage_for(target)
    try:
        handle.write(content)
        handle.flush()
        os.fsync(handle.fileno())
    finally:
        handle.close()
    return stage


def _validate_outputs(paths: Sequence[Path]) -> None:
    resolved = [path.resolve(strict=False) for path in paths]
    if len(set(resolved)) != len(resolved):
        raise MeasurementError(
            "output_paths_overlap",
            "output, stdout, stderr, and time-output paths must be distinct",
        )
    for path in paths:
        if path.exists():
            raise MeasurementError(
                "output_exists",
                f"refusing to replace existing output: {path}",
            )
        if not path.parent.is_dir():
            raise MeasurementError(
                "output_parent_missing",
                f"output parent directory does not exist: {path.parent}",
            )


def _return_code(time_exit_code: int, parsed: ParsedTime) -> int:
    if time_exit_code < 0:
        return 128 + (-time_exit_code)
    if parsed.command_terminated_abnormally:
        return time_exit_code or 1
    return time_exit_code


def run_measurement(
    command: Sequence[str],
    *,
    output: Path,
    stdout: Path,
    stderr: Path,
    time_output: Path,
) -> int:
    outputs = (output, stdout, stderr, time_output)
    _validate_outputs(outputs)
    if platform.system() != "Darwin":
        error = MeasurementError(
            "unsupported_platform",
            "native measurement requires macOS (Darwin)",
        )
        result_stage = _write_staged(output, canonical_json(invalid_record(command, error)))
        try:
            _publish(result_stage, output)
        finally:
            result_stage.unlink(missing_ok=True)
        return ADAPTER_ERROR_EXIT
    if not TIME.is_file() or not os.access(TIME, os.X_OK):
        error = MeasurementError(
            "time_unavailable",
            "required collector /usr/bin/time is not executable",
        )
        result_stage = _write_staged(output, canonical_json(invalid_record(command, error)))
        try:
            _publish(result_stage, output)
        finally:
            result_stage.unlink(missing_ok=True)
        return ADAPTER_ERROR_EXIT

    stdout_handle: BinaryIO | None = None
    combined_handle: BinaryIO | None = None
    stages: list[Path] = []
    try:
        stdout_handle, stdout_stage = _stage_for(stdout)
        stages.append(stdout_stage)
        combined_handle, combined_stage = _stage_for(time_output)
        stages.append(combined_stage)
        completed = subprocess.run(
            [str(TIME), "-l", *command],
            check=False,
            stdin=None,
            stdout=stdout_handle,
            stderr=combined_handle,
        )
        stdout_handle.flush()
        os.fsync(stdout_handle.fileno())
        combined_handle.flush()
        os.fsync(combined_handle.fileno())
        stdout_handle.close()
        combined_handle.close()
        raw = combined_stage.read_bytes()

        try:
            parsed = parse_time_l(raw)
            if completed.returncode < 0 and not parsed.command_terminated_abnormally:
                raise MeasurementError(
                    "collector_terminated",
                    "/usr/bin/time terminated before reporting command status",
                )
        except MeasurementError as error:
            command_stderr = raw
            timing_output = raw
            receipt = invalid_record(
                command,
                error,
                time_exit_code=completed.returncode,
            )
            adapter_return_code = ADAPTER_ERROR_EXIT
        else:
            command_stderr = parsed.command_stderr
            timing_output = parsed.timing_output
            receipt = success_record(command, parsed, completed.returncode)
            adapter_return_code = _return_code(completed.returncode, parsed)

        stderr_stage = _write_staged(stderr, command_stderr)
        stages.append(stderr_stage)
        time_stage = _write_staged(time_output, timing_output)
        stages.append(time_stage)
        result_stage = _write_staged(output, canonical_json(receipt))
        stages.append(result_stage)

        for stage, target in (
            (stdout_stage, stdout),
            (stderr_stage, stderr),
            (time_stage, time_output),
            (result_stage, output),
        ):
            _publish(stage, target)
            stages.remove(stage)
        return adapter_return_code
    finally:
        if stdout_handle is not None:
            stdout_handle.close()
        if combined_handle is not None:
            combined_handle.close()
        for stage in stages:
            stage.unlink(missing_ok=True)


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="measure a command with native macOS /usr/bin/time -l",
    )
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--stdout", required=True, type=Path)
    parser.add_argument("--stderr", required=True, type=Path)
    parser.add_argument("--time-output", required=True, type=Path)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    if args.command[:1] == ["--"]:
        args.command = args.command[1:]
    if not args.command:
        parser.error("a command is required after --")
    return args


def fail(message: str) -> NoReturn:
    print(f"perf-macos-measure: {message}", file=sys.stderr)
    raise SystemExit(ADAPTER_ERROR_EXIT)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        return run_measurement(
            args.command,
            output=args.output,
            stdout=args.stdout,
            stderr=args.stderr,
            time_output=args.time_output,
        )
    except MeasurementError as error:
        fail(error.message)
    except OSError as error:
        fail(str(error))


if __name__ == "__main__":
    raise SystemExit(main())
