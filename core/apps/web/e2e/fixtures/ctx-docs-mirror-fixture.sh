#!/usr/bin/env bash
set -euo pipefail

out=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    mirror)
      shift
      ;;
    --config)
      shift 2
      ;;
    --out)
      out="$2"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done

if [[ -z "$out" ]]; then
  echo "missing --out" >&2
  exit 1
fi

mkdir -p "$out"
cat <<'DOC' > "$out/index.md"
# Docs Mirror Fixture

This is a fixture output.
DOC
