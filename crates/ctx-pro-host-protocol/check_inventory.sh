#!/usr/bin/env bash
set -euo pipefail

generator="$1"
inventory="$2"
generated="$(mktemp)"
trap 'rm -f "$generated"' EXIT
"$generator" >"$generated"
diff -u "$inventory" "$generated"
