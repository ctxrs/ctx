#!/usr/bin/env sh
set -eu

ORT_COMMIT="f7994dc91b8f48c78afc506880b3f9f558957919"
ORT_ENVIRONMENT_BEFORE="988dba4a3c2b55a16abc6ea6beb1e038bca36c4dbe9578995b950cf739bf8968"
ORT_ENVIRONMENT_AFTER="e82c55aefbf390569b2d3664278a458ab4be9b27ec80c209ecda8db4d639cb0c"

usage() {
  printf 'usage: %s CARGO_HOME PATCH_FILE\n' "$(basename "$0")" >&2
  exit 2
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v sha256 >/dev/null 2>&1; then
    sha256 -q "$1"
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

if [ "$#" -ne 2 ]; then
  usage
fi

cargo_home="$1"
patch_file="$2"
case "${cargo_home}" in
  /*) ;;
  *)
    printf 'error: CARGO_HOME must be absolute: %s\n' "${cargo_home}" >&2
    exit 1
    ;;
esac
[ -d "${cargo_home}/git/checkouts" ] || {
  printf 'error: Cargo has not fetched git checkouts under %s\n' "${cargo_home}" >&2
  exit 1
}
[ -f "${patch_file}" ] || {
  printf 'error: ORT patch does not exist: %s\n' "${patch_file}" >&2
  exit 1
}

ort_checkout=""
for candidate in "${cargo_home}"/git/checkouts/ort-*/*; do
  [ -d "${candidate}" ] || continue
  [ ! -L "${candidate}" ] || continue
  candidate_commit="$(git -C "${candidate}" rev-parse --verify HEAD 2>/dev/null || true)"
  if [ "${candidate_commit}" = "${ORT_COMMIT}" ]; then
    [ -z "${ort_checkout}" ] || {
      printf 'error: multiple ORT checkouts match %s\n' "${ORT_COMMIT}" >&2
      exit 1
    }
    ort_checkout="${candidate}"
  fi
done
[ -n "${ort_checkout}" ] || {
  printf 'error: ORT checkout %s is absent from %s\n' \
    "${ORT_COMMIT}" "${cargo_home}" >&2
  exit 1
}

environment_source="${ort_checkout}/src/environment.rs"
[ -f "${environment_source}" ] || {
  printf 'error: ORT environment source is absent: %s\n' "${environment_source}" >&2
  exit 1
}

before="$(sha256_file "${environment_source}")"
case "${before}" in
  "${ORT_ENVIRONMENT_BEFORE}")
    git -C "${ort_checkout}" apply --unidiff-zero --check "${patch_file}"
    git -C "${ort_checkout}" apply --unidiff-zero "${patch_file}"
    ;;
  "${ORT_ENVIRONMENT_AFTER}")
    ;;
  *)
    printf 'error: unreviewed ORT environment source digest: %s\n' "${before}" >&2
    exit 1
    ;;
esac

after="$(sha256_file "${environment_source}")"
[ "${after}" = "${ORT_ENVIRONMENT_AFTER}" ] || {
  printf 'error: patched ORT environment digest mismatch: %s\n' "${after}" >&2
  exit 1
}

changed="$(git -C "${ort_checkout}" diff --name-only)"
[ "${changed}" = "src/environment.rs" ] || {
  printf 'error: ORT checkout has unexpected tracked changes: %s\n' "${changed}" >&2
  exit 1
}
unexpected="$(
  git -C "${ort_checkout}" status --porcelain --untracked-files=all |
    awk '$0 != " M src/environment.rs" && $0 != "?? .cargo-ok" {print}'
)"
[ -z "${unexpected}" ] || {
  printf 'error: ORT checkout has unexpected state:\n%s\n' "${unexpected}" >&2
  exit 1
}

printf 'FreeBSD ORT release source: OK (%s, %s)\n' \
  "${ORT_COMMIT}" "${ORT_ENVIRONMENT_AFTER}"
