#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-terminal-dependency-boundary.sh TERMINAL_CARGO_TOML' >&2
  exit 64
fi

manifest="$(readlink -f "$1")"
root="$(dirname "$(dirname "$(dirname "$manifest")")")"
if [[ ! -f "$manifest" ]]; then
  echo "ctx-terminal manifest is absent: $manifest" >&2
  exit 1
fi
allowed='anstream|anstyle|anyhow|jiff|serde|serde_json|supports-unicode|terminal_size|unicode-segmentation|unicode-width|uuid'
actual="$(awk '/^\[dependencies\]/{found=1; next} found && /^\[/{exit} found {print}' "$manifest" | awk -F '[.=]' 'NF {print $1}' | LC_ALL=C sort | paste -sd'|' -)"
if [[ "$actual" != "$allowed" ]]; then
  echo "ctx-terminal dependency inventory differs: $actual" >&2
  exit 1
fi
if rg -n '(^|[^[:alnum:]_])(clap|ureq|ring|ed25519_dalek|sha2|zeroize|url)::|ctx_(cli|history|daemon|semantic|agent)($|[^[:alnum:]_])' "${root}/crates/ctx-terminal/src"; then
  echo 'ctx-terminal production source contains a forbidden dependency backedge' >&2
  exit 1
fi
if rg -n 'from_schema_v1|schema_v1_fields|SourceBacked(CurrentSource|Refresh)' "${root}/crates/ctx-terminal/src"; then
  echo 'ctx-terminal production source contains a domain wire-schema adapter' >&2
  exit 1
fi
