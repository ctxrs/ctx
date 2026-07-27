#!/usr/bin/env bash
set -euo pipefail

generator="$1"
inventory="$2"
fingerprint_source="$3"
generated="$(mktemp)"
generated_fingerprint="$(mktemp)"
trap 'rm -f "$generated" "$generated_fingerprint"' EXIT
"$generator" >"$generated"
"$generator" --fingerprint-rust >"$generated_fingerprint"
diff -u "$inventory" "$generated"
diff -u "$fingerprint_source" "$generated_fingerprint"
