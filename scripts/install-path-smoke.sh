#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    printf 'install path smoke requires %s\n' "$1" >&2
    exit 127
  }
}

sha256_file() {
  local path="$1"

  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${path}" | awk '{ print $1 }'
    return 0
  fi

  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "${path}" | awk '{ print $1 }'
    return 0
  fi

  printf 'install path smoke requires sha256sum or shasum\n' >&2
  exit 127
}

require_cmd awk
require_cmd bash
require_cmd curl
require_cmd openssl
require_cmd python3

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-install-path-smoke.XXXXXX")"
server_pid=""
port=""

cleanup() {
  if [[ -n "${server_pid}" ]]; then
    kill "${server_pid}" 2>/dev/null || true
    wait "${server_pid}" 2>/dev/null || true
  fi
  rm -rf "${tmp_dir}"
}
trap cleanup EXIT

artifact="${tmp_dir}/ctx-linux-x64"
cat > "${artifact}" <<'SH'
#!/usr/bin/env sh
if [ "${1:-}" = "setup" ]; then
  exit "${CTX_FAKE_SETUP_EXIT:-0}"
fi
exit 0
SH
chmod +x "${artifact}"
checksum="$(sha256_file "${artifact}")"

openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout "${tmp_dir}/key.pem" \
  -out "${tmp_dir}/cert.pem" \
  -subj '/CN=127.0.0.1' \
  -addext 'subjectAltName=IP:127.0.0.1,DNS:localhost' \
  -days 1 >/dev/null 2>&1

port_file="${tmp_dir}/server.port"
python3 - "${tmp_dir}" "${tmp_dir}/cert.pem" "${tmp_dir}/key.pem" "${port_file}" <<'PY' >"${tmp_dir}/server.log" 2>&1 &
import functools
import http.server
import os
import ssl
import sys

root, cert, key, port_file = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=root)
server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(certfile=cert, keyfile=key)
server.socket = context.wrap_socket(server.socket, server_side=True)
tmp_port_file = port_file + ".tmp"
with open(tmp_port_file, "w", encoding="utf-8") as handle:
    print(server.server_address[1], file=handle)
os.replace(tmp_port_file, port_file)
server.serve_forever()
PY
server_pid="$!"

for _ in 1 2 3 4 5 6 7 8 9 10; do
  if [[ -s "${port_file}" ]]; then
    port="$(cat "${port_file}")"
    break
  fi
  if ! kill -0 "${server_pid}" 2>/dev/null; then
    cat "${tmp_dir}/server.log" >&2 || true
    exit 1
  fi
  sleep 0.2
done
if [[ -z "${port}" ]]; then
  printf 'install path smoke server did not publish a port\n' >&2
  cat "${tmp_dir}/server.log" >&2 || true
  exit 1
fi

for _ in 1 2 3 4 5 6 7 8 9 10; do
  if CURL_CA_BUNDLE="${tmp_dir}/cert.pem" curl -fsS "https://127.0.0.1:${port}/ctx-linux-x64" >/dev/null 2>&1; then
    break
  fi
  sleep 0.2
done
CURL_CA_BUNDLE="${tmp_dir}/cert.pem" curl -fsS "https://127.0.0.1:${port}/ctx-linux-x64" >/dev/null

metadata="${tmp_dir}/metadata.env"
{
  printf 'CTX_RELEASE_SCHEMA_VERSION=1\n'
  printf 'CTX_RELEASE_VERSION=0.0.0-smoke\n'
  printf 'CTX_RELEASE_BASE_URL=https://127.0.0.1:%s\n' "${port}"
  printf 'CTX_RELEASE_ARTIFACT_linux_x64=ctx-linux-x64\n'
  printf 'CTX_RELEASE_SHA256_linux_x64=%s\n' "${checksum}"
} > "${metadata}"

base_env=(CURL_CA_BUNDLE="${tmp_dir}/cert.pem" CTX_INSTALL_NO_MAN=1)
installer=(bash "${repo_root}/scripts/install.sh" --metadata "${metadata}" --platform linux-x64)

home_dry_run="${tmp_dir}/home-dry-run"
mkdir -p "${home_dry_run}"
env -u GITHUB_PATH -u CI "${base_env[@]}" PATH="/usr/bin:/bin" HOME="${home_dry_run}" SHELL="/bin/bash" "${installer[@]}" --dry-run --no-setup > "${tmp_dir}/dry-run.out"
grep -F 'Dry run: would install ctx 0.0.0-smoke (linux-x64)' "${tmp_dir}/dry-run.out" >/dev/null
! grep -F 'Installing ctx 0.0.0-smoke (linux-x64)' "${tmp_dir}/dry-run.out" >/dev/null
! grep -F 'Installed ctx binary.' "${tmp_dir}/dry-run.out" >/dev/null

home_idem="${tmp_dir}/home-idem"
mkdir -p "${home_idem}"
env -u GITHUB_PATH -u CI "${base_env[@]}" PATH="/usr/bin:/bin" HOME="${home_idem}" SHELL="/bin/bash" "${installer[@]}" --no-setup > "${tmp_dir}/idem-1.out"
env -u GITHUB_PATH -u CI "${base_env[@]}" PATH="/usr/bin:/bin" HOME="${home_idem}" SHELL="/bin/bash" "${installer[@]}" --no-setup > "${tmp_dir}/idem-2.out"
test "$(grep -c 'ctx installer PATH setup' "${home_idem}/.bashrc")" = 1
grep -F 'Found existing PATH setup' "${tmp_dir}/idem-2.out" >/dev/null

home_on_path="${tmp_dir}/home-on-path"
mkdir -p "${home_on_path}"
env -u GITHUB_PATH -u CI "${base_env[@]}" PATH="${home_on_path}/.local/bin:/usr/bin:/bin" HOME="${home_on_path}" SHELL="/bin/bash" "${installer[@]}" --no-setup > "${tmp_dir}/on-path.out"
test ! -e "${home_on_path}/.bashrc"

home_env_bin="${tmp_dir}/home-env-bin"
env_bin="${tmp_dir}/ctx-env-bin"
mkdir -p "${home_env_bin}"
env -u GITHUB_PATH -u CI "${base_env[@]}" PATH="/usr/bin:/bin" HOME="${home_env_bin}" SHELL="/bin/bash" CTX_BIN_DIR="${env_bin}" "${installer[@]}" --no-setup > "${tmp_dir}/env-bin.out"
test -x "${env_bin}/ctx"
grep -F "${env_bin}" "${home_env_bin}/.bashrc" >/dev/null

home_bin_override="${tmp_dir}/home-bin-override"
env_override_bin="${tmp_dir}/ctx-env-override-bin"
flag_override_bin="${tmp_dir}/ctx-flag-override-bin"
mkdir -p "${home_bin_override}"
env -u GITHUB_PATH -u CI "${base_env[@]}" PATH="/usr/bin:/bin" HOME="${home_bin_override}" SHELL="/bin/bash" CTX_BIN_DIR="${env_override_bin}" "${installer[@]}" --bin-dir "${flag_override_bin}" --no-setup > "${tmp_dir}/bin-override.out"
test ! -e "${env_override_bin}/ctx"
test -x "${flag_override_bin}/ctx"
grep -F "${flag_override_bin}" "${home_bin_override}/.bashrc" >/dev/null

home_marker_change="${tmp_dir}/home-marker-change"
old_marker_bin="${tmp_dir}/old-marker-bin"
new_marker_bin="${tmp_dir}/new-marker-bin"
mkdir -p "${home_marker_change}"
{
  printf '# ctx installer PATH setup\n'
  printf 'case ":${PATH}:" in\n'
  printf '  *":%s:"*) ;;\n' "${old_marker_bin}"
  printf '  *) export PATH="%s:${PATH}" ;;\n' "${old_marker_bin}"
  printf 'esac\n'
} > "${home_marker_change}/.bashrc"
env -u GITHUB_PATH -u CI "${base_env[@]}" PATH="/usr/bin:/bin" HOME="${home_marker_change}" SHELL="/bin/bash" "${installer[@]}" --bin-dir "${new_marker_bin}" --no-setup > "${tmp_dir}/marker-change.out"
grep -F "${new_marker_bin}" "${home_marker_change}/.bashrc" >/dev/null
PATH="/usr/bin:/bin" HOME="${home_marker_change}" bash -c 'source "$HOME/.bashrc"; command -v ctx' >/dev/null

home_comment_path="${tmp_dir}/home-comment-path"
comment_path_bin="${tmp_dir}/comment-path-bin"
mkdir -p "${home_comment_path}"
printf '# PATH may include %s later\n' "${comment_path_bin}" > "${home_comment_path}/.bashrc"
env -u GITHUB_PATH -u CI "${base_env[@]}" PATH="/usr/bin:/bin" HOME="${home_comment_path}" SHELL="/bin/bash" "${installer[@]}" --bin-dir "${comment_path_bin}" --no-setup > "${tmp_dir}/comment-path.out"
test "$(grep -c 'ctx installer PATH setup' "${home_comment_path}/.bashrc")" = 1
PATH="/usr/bin:/bin" HOME="${home_comment_path}" bash -c 'source "$HOME/.bashrc"; command -v ctx' >/dev/null

home_env_no="${tmp_dir}/home-env-no"
mkdir -p "${home_env_no}"
env -u GITHUB_PATH -u CI "${base_env[@]}" PATH="/usr/bin:/bin" HOME="${home_env_no}" SHELL="/bin/bash" CTX_INSTALL_NO_MODIFY_PATH=1 "${installer[@]}" --no-setup > "${tmp_dir}/env-no.out"
test ! -e "${home_env_no}/.bashrc"
grep -F 'shell startup file update skipped' "${tmp_dir}/env-no.out" >/dev/null

home_flag_no="${tmp_dir}/home-flag-no"
github_path_no="${tmp_dir}/github-path-no"
mkdir -p "${home_flag_no}"
: > "${github_path_no}"
env -u CI "${base_env[@]}" PATH="/usr/bin:/bin" HOME="${home_flag_no}" SHELL="/bin/bash" GITHUB_PATH="${github_path_no}" "${installer[@]}" --no-setup --no-modify-path > "${tmp_dir}/flag-no.out"
test ! -e "${home_flag_no}/.bashrc"
test ! -s "${github_path_no}"
grep -F 'shell startup file update skipped' "${tmp_dir}/flag-no.out" >/dev/null

home_gha="${tmp_dir}/home-gha"
github_path="${tmp_dir}/github-path"
mkdir -p "${home_gha}"
: > "${github_path}"
env -u CI "${base_env[@]}" PATH="/usr/bin:/bin" HOME="${home_gha}" SHELL="/bin/bash" GITHUB_PATH="${github_path}" "${installer[@]}" --no-setup > "${tmp_dir}/gha.out"
grep -F "${home_gha}/.local/bin" "${github_path}" >/dev/null
test ! -e "${home_gha}/.bashrc"

home_ci="${tmp_dir}/home-ci"
mkdir -p "${home_ci}"
env -u GITHUB_PATH "${base_env[@]}" PATH="/usr/bin:/bin" HOME="${home_ci}" SHELL="/bin/bash" CI=true "${installer[@]}" --no-setup > "${tmp_dir}/ci.out"
test ! -e "${home_ci}/.bashrc"
grep -F 'CI detected' "${tmp_dir}/ci.out" >/dev/null

home_shell_empty="${tmp_dir}/home-shell-empty"
mkdir -p "${home_shell_empty}"
env -u GITHUB_PATH -u CI "${base_env[@]}" PATH="/usr/bin:/bin" HOME="${home_shell_empty}" SHELL="" "${installer[@]}" --no-setup > "${tmp_dir}/shell-empty.out"
grep -F 'ctx installer PATH setup' "${home_shell_empty}/.profile" >/dev/null

if command -v zsh >/dev/null 2>&1; then
  home_zsh="${tmp_dir}/home-zsh"
  mkdir -p "${home_zsh}"
  zsh_bin="$(command -v zsh)"
  env -u GITHUB_PATH -u CI "${base_env[@]}" PATH="/usr/bin:/bin" HOME="${home_zsh}" SHELL="${zsh_bin}" "${installer[@]}" --no-setup > "${tmp_dir}/zsh.out"
  grep -F 'ctx installer PATH setup' "${home_zsh}/.zshrc" >/dev/null
  PATH="/usr/bin:/bin" HOME="${home_zsh}" "${zsh_bin}" -c 'source "$HOME/.zshrc"; command -v ctx' >/dev/null
else
  printf 'zsh not found; skipping zsh PATH execution check\n'
fi

if command -v fish >/dev/null 2>&1; then
  home_fish="${tmp_dir}/home-fish"
  mkdir -p "${home_fish}"
  fish_bin="$(command -v fish)"
  env -u GITHUB_PATH -u CI "${base_env[@]}" PATH="/usr/bin:/bin" HOME="${home_fish}" SHELL="${fish_bin}" "${installer[@]}" --no-setup > "${tmp_dir}/fish.out"
  grep -F 'ctx installer PATH setup' "${home_fish}/.config/fish/config.fish" >/dev/null
  env PATH="/usr/bin:/bin" HOME="${home_fish}" "${fish_bin}" -c 'source "$HOME/.config/fish/config.fish"; command -q ctx'
else
  printf 'fish not found; skipping fish PATH execution check\n'
fi

home_fail="${tmp_dir}/home-fail"
mkdir -p "${home_fail}"
set +e
env -u GITHUB_PATH -u CI "${base_env[@]}" PATH="/usr/bin:/bin" HOME="${home_fail}" SHELL="/bin/bash" CTX_FAKE_SETUP_EXIT=42 "${installer[@]}" --no-skill > "${tmp_dir}/setup-fail.out" 2>"${tmp_dir}/setup-fail.err"
setup_status="$?"
set -e
test "${setup_status}" = 42
grep -F 'ctx installer PATH setup' "${home_fail}/.bashrc" >/dev/null
grep -F 'ctx setup failed after install' "${tmp_dir}/setup-fail.err" >/dev/null

printf 'install path smoke ok\n'
