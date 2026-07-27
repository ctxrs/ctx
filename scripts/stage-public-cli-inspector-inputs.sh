#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: stage-public-cli-inspector-inputs.sh SOURCE_ROOT ARTIFACT_DIR BINARY_NAME OUTPUT_DIR" >&2
}

[[ $# -eq 4 ]] || { usage; exit 64; }
source_root="$1"
artifact_dir="$2"
binary_name="$3"
output_dir="$4"

[[ "$source_root" == /* && "$artifact_dir" == /* && "$output_dir" == /* ]] \
  || { echo "error: inspector staging paths must be absolute" >&2; exit 1; }
[[ -d "$source_root" && ! -L "$source_root" ]] \
  || { echo "error: inspector source root must be a non-symlink directory" >&2; exit 1; }
[[ -d "$artifact_dir" && ! -L "$artifact_dir" ]] \
  || { echo "error: inspector artifact root must be a non-symlink directory" >&2; exit 1; }
[[ -d "$output_dir" && ! -L "$output_dir" && -z "$(find "$output_dir" -mindepth 1 -maxdepth 1 -print -quit)" ]] \
  || { echo "error: inspector output must be an empty non-symlink directory" >&2; exit 1; }

source_root="$(cd "$source_root" && pwd -P)"
artifact_dir="$(cd "$artifact_dir" && pwd -P)"
case "$artifact_dir" in
  "$source_root"/*) ;;
  *) echo "error: inspector artifact root escapes the source snapshot" >&2; exit 1 ;;
esac

copy_regular() {
  local source="$1"
  local destination="$2"
  local mode="$3"
  [[ -f "$source" && ! -L "$source" ]] \
    || { echo "error: inspector input is not a regular non-symlink file: $source" >&2; exit 1; }
  local resolved
  resolved="$(perl -MCwd=realpath -e 'print realpath($ARGV[0]) // q{}' "$source")"
  case "$resolved" in
    "$source_root"/*) ;;
    *) echo "error: inspector input escapes the source snapshot: $source" >&2; exit 1 ;;
  esac
  mkdir -p "$(dirname "$destination")"
  install -m "$mode" "$source" "$destination"
}

chmod 0755 "$output_dir"
copy_regular "$source_root/scripts/check-public-cli-artifact.sh" \
  "$output_dir/scripts/check-public-cli-artifact.sh" 0555
copy_regular "$source_root/scripts/check-release-binary-compat.sh" \
  "$output_dir/scripts/check-release-binary-compat.sh" 0555
copy_regular "$source_root/scripts/run-native-candidate-smoke.sh" \
  "$output_dir/scripts/run-native-candidate-smoke.sh" 0555
copy_regular "$source_root/tests/fixtures/custom-history-jsonl/basic.jsonl" \
  "$output_dir/tests/fixtures/custom-history-jsonl/basic.jsonl" 0444
copy_regular "$artifact_dir/$binary_name" "$output_dir/artifacts/$binary_name" 0555
copy_regular "$artifact_dir/$binary_name.sha256" "$output_dir/artifacts/$binary_name.sha256" 0444
copy_regular "$artifact_dir/$binary_name.version" "$output_dir/artifacts/$binary_name.version" 0444
find "$output_dir" -type d -exec chmod 0755 {} +

printf 'public CLI inspector snapshot: %s\n' "$output_dir"
