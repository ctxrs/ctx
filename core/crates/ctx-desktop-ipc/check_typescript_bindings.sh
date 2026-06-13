#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 2 ]]; then
  echo "usage: check_typescript_bindings.sh <generator-bin> <generated-typescript>" >&2
  exit 2
fi

generator_bin="$1"
generated_typescript="$2"

"$generator_bin" --output "$generated_typescript" --check
