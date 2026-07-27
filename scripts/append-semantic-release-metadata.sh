#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  printf 'usage: %s BASE_METADATA SEMANTIC_ENV OUTPUT_METADATA\n' "$0" >&2
  exit 2
fi

base_metadata="$1"
semantic_env="$2"
output_metadata="$3"
[[ -f "${base_metadata}" && -f "${semantic_env}" ]] || {
  printf 'base metadata and Semantic field input must be regular files\n' >&2
  exit 1
}
[[ ! -e "${output_metadata}" && ! -L "${output_metadata}" ]] || {
  printf 'refusing to replace metadata output: %s\n' "${output_metadata}" >&2
  exit 1
}
mkdir -p "$(dirname "${output_metadata}")"

python3 - "${base_metadata}" "${semantic_env}" "${output_metadata}" <<'PY'
import base64
import json
import os
import pathlib
import tempfile
import sys

base_path, semantic_path, output_path = map(pathlib.Path, sys.argv[1:])
expected = [
    "CTX_RELEASE_SEMANTIC_SCHEMA_VERSION",
    "CTX_RELEASE_SEMANTIC_ASSETS",
    "CTX_RELEASE_SEMANTIC_AUTHORITY_apple_silicon_coreml",
    "CTX_RELEASE_SEMANTIC_AUTHORITY_windows_windows_ml",
    "CTX_RELEASE_SEMANTIC_AUTHORITY_linux_nvidia_ort_cuda",
    "CTX_RELEASE_SEMANTIC_AUTHORITY_universal_ort_cpu",
]
base = base_path.read_bytes()
if b"CTX_RELEASE_SEMANTIC_" in base:
    raise SystemExit("base metadata already contains Semantic release fields")
try:
    lines = semantic_path.read_text(encoding="ascii").splitlines()
except UnicodeError as error:
    raise SystemExit(f"Semantic release fields must be ASCII: {error}") from error
if len(lines) != len(expected):
    raise SystemExit("Semantic release input must contain exactly six fields")
values = []
for line, key in zip(lines, expected, strict=True):
    prefix = f"{key}="
    if not line.startswith(prefix):
        raise SystemExit(f"Semantic release field order mismatch: expected {key}")
    value = line[len(prefix):]
    if not value:
        raise SystemExit(f"empty Semantic release field: {key}")
    if key == "CTX_RELEASE_SEMANTIC_SCHEMA_VERSION":
        if value != "1":
            raise SystemExit("unsupported Semantic release schema version")
    else:
        decoded = base64.b64decode(value, validate=True)
        parsed = json.loads(decoded)
        canonical = json.dumps(
            parsed, ensure_ascii=True, separators=(",", ":"), sort_keys=True
        ).encode("ascii")
        if decoded != canonical or base64.b64encode(decoded).decode("ascii") != value:
            raise SystemExit(f"noncanonical Semantic release field: {key}")
    values.append(line.encode("ascii") + b"\n")
payload = base
if payload and not payload.endswith(b"\n"):
    payload += b"\n"
payload += b"".join(values)
parent = output_path.parent
descriptor, temporary_name = tempfile.mkstemp(
    prefix=f".{output_path.name}.", dir=parent
)
try:
    with os.fdopen(descriptor, "wb") as output:
        output.write(payload)
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary_name, output_path)
except BaseException:
    try:
        os.unlink(temporary_name)
    except FileNotFoundError:
        pass
    raise
PY

printf 'appended six unsigned Semantic fields to %s\n' "${output_metadata}"
