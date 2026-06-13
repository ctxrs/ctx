#!/usr/bin/env bash
set -euo pipefail

mode="${1:-}"
shift || true

data_dir=""
allow_user=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --data-dir)
      data_dir="${2:-}"
      shift 2
      ;;
    --allow-user)
      allow_user="${2:-}"
      shift 2
      ;;
    -h|--help)
      cat <<'EOF'
Usage:
  bootstrap.sh stage --data-dir <ctx-data-dir>
  bootstrap.sh status --data-dir <ctx-data-dir>
  bootstrap.sh activate --data-dir <ctx-data-dir> [--allow-user <username>]
EOF
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ -z "${mode}" ]]; then
  echo "error: mode is required" >&2
  exit 2
fi

if [[ -z "${data_dir}" ]]; then
  echo "error: --data-dir is required" >&2
  exit 2
fi

bootstrap_root="${data_dir%/}/linux-sandbox-runtime"
cache_dir="${bootstrap_root}/cache"
downloads_dir="${cache_dir}/downloads"
debs_dir="${cache_dir}/debs"
status_path="${bootstrap_root}/status.json"
ready_marker="${bootstrap_root}/runtime-ready"
managed_nerdctl_path="/usr/local/bin/nerdctl"
wrapper_path="/usr/local/bin/ctx-rootful-nerdctl"
system_containerd_address="/run/containerd/containerd.sock"
system_containerd_namespace="default"
nerdctl_version="v2.2.1"
cni_plugin_dir="/opt/cni/bin"
cni_bridge_plugin_path="${cni_plugin_dir}/bridge"

mkdir -p "${bootstrap_root}" "${downloads_dir}" "${debs_dir}"

json_escape() {
  printf '%s' "${1:-}" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

write_status() {
  local state="$1"
  local supported="$2"
  local message="${3:-}"
  local distro="${4:-}"
  cat > "${status_path}" <<EOF
{"state":"$(json_escape "${state}")","supported":${supported},"message":"$(json_escape "${message}")","distro":"$(json_escape "${distro}")"}
EOF
}

status_json() {
  if [[ -f "${status_path}" ]]; then
    cat "${status_path}"
    return 0
  fi
  cat <<'EOF'
{"state":"download_pending","supported":false,"message":"","distro":""}
EOF
}

detect_arch() {
  case "$(uname -m)" in
    x86_64|amd64)
      printf '%s' "amd64"
      ;;
    aarch64|arm64)
      printf '%s' "arm64"
      ;;
    *)
      echo "unsupported"
      ;;
  esac
}

nerdctl_expected_sha256() {
  case "$1" in
    amd64)
      printf '%s' "34144de7f12756aa4b9dc42a907fd95b0c5eb82a63566a650ca10c8abe7a26a0"
      ;;
    arm64)
      printf '%s' "abc83c9ac3d843c3442eedfb61c6456b8b59b1e4cd69f69598ca1582acc7c094"
      ;;
    *)
      return 1
      ;;
  esac
}

detect_distro() {
  if [[ ! -f /etc/os-release ]]; then
    echo "unknown"
    return 0
  fi
  # shellcheck disable=SC1091
  . /etc/os-release
  printf '%s' "${ID:-unknown}"
}

distro_supported() {
  case "$(detect_distro)" in
    ubuntu|debian)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

nerdctl_tarball_name() {
  local arch="$1"
  printf 'nerdctl-%s-linux-%s.tar.gz' "${nerdctl_version#v}" "${arch}"
}

staged_nerdctl_archive_path() {
  local arch="$1"
  printf '%s/%s' "${downloads_dir}" "$(nerdctl_tarball_name "${arch}")"
}

verify_nerdctl_checksum() {
  local arch="$1"
  local archive_path="$2"
  local expected
  expected="$(nerdctl_expected_sha256 "${arch}")"
  if [[ ! -f "${archive_path}" ]]; then
    return 1
  fi
  local actual
  actual="$(sha256sum "${archive_path}" | awk '{print $1}')"
  [[ "${actual}" == "${expected}" ]]
}

download_with_retries() {
  local url="$1"
  local dest="$2"
  local label="$3"
  local attempts="${CTX_LINUX_SANDBOX_BOOTSTRAP_DOWNLOAD_ATTEMPTS:-5}"
  local delay_seconds="${CTX_LINUX_SANDBOX_BOOTSTRAP_DOWNLOAD_RETRY_DELAY_SECONDS:-3}"
  local attempt
  for attempt in $(seq 1 "${attempts}"); do
    rm -f "${dest}"
    if curl -fsSL "${url}" -o "${dest}"; then
      return 0
    fi
    local status=$?
    rm -f "${dest}"
    if [[ "${attempt}" -ge "${attempts}" ]]; then
      echo "error: downloading ${label} failed after ${attempts} attempts from ${url} (curl exit ${status})" >&2
      return "${status}"
    fi
    echo "warning: downloading ${label} failed on attempt ${attempt}/${attempts} from ${url} (curl exit ${status}); retrying in ${delay_seconds}s" >&2
    sleep "${delay_seconds}"
  done
  return 1
}

acquire_nerdctl_download_lock() {
  local lock_dir="${downloads_dir}/.nerdctl-download.lock"
  for _ in $(seq 1 120); do
    if mkdir "${lock_dir}" 2>/dev/null; then
      printf '%s\n' "${lock_dir}"
      return 0
    fi
    sleep 1
  done
  echo "error: timed out waiting for Linux sandbox runtime download lock" >&2
  exit 1
}

download_nerdctl() {
  local arch="$1"
  local tarball
  tarball="$(nerdctl_tarball_name "${arch}")"
  local url="https://github.com/containerd/nerdctl/releases/download/${nerdctl_version}/${tarball}"
  local dest
  dest="$(staged_nerdctl_archive_path "${arch}")"
  local lock_dir
  lock_dir="$(acquire_nerdctl_download_lock)"
  release_nerdctl_download_lock() {
    rm -rf "${lock_dir}"
  }
  trap release_nerdctl_download_lock RETURN
  if verify_nerdctl_checksum "${arch}" "${dest}"; then
    return 0
  fi
  rm -f "${dest}"
  local partial="${dest}.partial.$$"
  rm -f "${partial}"
  if ! download_with_retries "${url}" "${partial}" "Linux sandbox runtime archive"; then
    rm -f "${partial}"
    release_nerdctl_download_lock
    trap - RETURN
    exit 1
  fi
  if ! verify_nerdctl_checksum "${arch}" "${partial}"; then
    rm -f "${partial}"
    release_nerdctl_download_lock
    trap - RETURN
    echo "error: staged Linux sandbox runtime archive failed checksum verification" >&2
    exit 1
  fi
  mv -f "${partial}" "${dest}"
  release_nerdctl_download_lock
  trap - RETURN
}

stage_apt_debs() {
  if ! command -v apt >/dev/null 2>&1; then
    return 1
  fi
  (
    cd "${debs_dir}"
    apt download containerd containernetworking-plugins >/dev/null
  )
}

apt_expected_filename() {
  local package="$1"
  local filename
  filename="$(apt-cache show "${package}" | awk '/^Filename: /{print $2; exit}')"
  printf '%s\n' "${filename##*/}"
}

apt_expected_sha256() {
  local package="$1"
  apt-cache show "${package}" | awk '/^SHA256: /{print $2; exit}'
}

copy_verified_staged_deb() {
  local package="$1"
  local dest_dir="$2"
  local expected_name
  expected_name="$(apt_expected_filename "${package}")"
  local expected_sha
  expected_sha="$(apt_expected_sha256 "${package}")"
  if [[ -z "${expected_name}" || -z "${expected_sha}" ]]; then
    return 1
  fi
  local source_path="${debs_dir}/${expected_name}"
  if [[ ! -f "${source_path}" ]]; then
    return 1
  fi
  local verified_copy="${dest_dir}/${expected_name}"
  install -m 0644 "${source_path}" "${verified_copy}"
  local actual_sha
  actual_sha="$(sha256sum "${verified_copy}" | awk '{print $1}')"
  if [[ "${actual_sha}" != "${expected_sha}" ]]; then
    rm -f "${verified_copy}"
    return 1
  fi
  printf '%s\n' "${verified_copy}"
}

resolve_cni_plugin_source_dir() {
  local candidate
  for candidate in \
    "/usr/lib/cni" \
    "/usr/libexec/cni" \
    "${cni_plugin_dir}"
  do
    if [[ -x "${candidate}/bridge" ]]; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done
  return 1
}

containerd_provider_available() {
  if ! command -v containerd >/dev/null 2>&1; then
    return 1
  fi
  if command -v systemctl >/dev/null 2>&1 && ! systemctl cat containerd.service >/dev/null 2>&1; then
    return 1
  fi
  return 0
}

install_apt_requirements() {
  if ! command -v apt-get >/dev/null 2>&1; then
    echo "error: apt-get is required for Linux sandbox activation" >&2
    exit 1
  fi

  local required_packages=()
  if ! containerd_provider_available; then
    required_packages+=(containerd)
  fi
  if ! resolve_cni_plugin_source_dir >/dev/null 2>&1; then
    required_packages+=(containernetworking-plugins)
  fi
  if [[ ${#required_packages[@]} -eq 0 ]]; then
    return 0
  fi

  local tmp_dir
  tmp_dir="$(mktemp -d)"
  local verified_debs=()
  local package
  for package in "${required_packages[@]}"; do
    local verified_deb
    if ! verified_deb="$(copy_verified_staged_deb "${package}" "${tmp_dir}")"; then
      verified_debs=()
      break
    fi
    verified_debs+=("${verified_deb}")
  done

  if [[ ${#verified_debs[@]} -eq ${#required_packages[@]} ]]; then
    apt-get install -y "${verified_debs[@]}"
  else
    apt-get update
    apt-get install -y "${required_packages[@]}"
  fi
  rm -rf "${tmp_dir}"
}

install_managed_nerdctl() {
  local arch="$1"
  local tarball
  tarball="$(staged_nerdctl_archive_path "${arch}")"
  if ! verify_nerdctl_checksum "${arch}" "${tarball}"; then
    echo "error: staged nerdctl archive is missing or invalid" >&2
    exit 1
  fi
  local tmp_dir
  tmp_dir="$(mktemp -d)"
  tar -xzf "${tarball}" -C "${tmp_dir}" nerdctl
  install -m 0755 "${tmp_dir}/nerdctl" "${managed_nerdctl_path}"
  rm -rf "${tmp_dir}"
}

validate_allow_user_name() {
  local user_name="$1"
  if [[ -z "${user_name}" ]]; then
    echo "error: activation requires --allow-user" >&2
    exit 1
  fi
  if [[ ! "${user_name}" =~ ^[A-Za-z0-9._-]+$ ]]; then
    echo "error: activation requires a POSIX-safe username" >&2
    exit 1
  fi
}

install_rootful_wrapper() {
  local allow_user_name="$1"
  validate_allow_user_name "${allow_user_name}"
  local allow_uid
  allow_uid="$(id -u "${allow_user_name}")"
  local allow_gid
  allow_gid="$(id -g "${allow_user_name}")"
  local tmp_dir
  tmp_dir="$(mktemp -d)"
  cat > "${tmp_dir}/ctx-rootful-nerdctl" <<EOF
#!/usr/bin/env bash
set -euo pipefail
allowed_user="${allow_user_name}"
allowed_uid="${allow_uid}"
allowed_gid="${allow_gid}"
if [[ "\${EUID}" -ne 0 ]]; then
  exec sudo --non-interactive "\$0" "\$@"
fi
if [[ -n "\${SUDO_USER:-}" && "\${SUDO_USER}" != "\${allowed_user}" ]]; then
  echo "error: ctx sandbox wrapper only permits ${allow_user_name}" >&2
  exit 1
fi

is_container_name() {
  [[ "\$1" =~ ^ctx-harness-[A-Za-z0-9._:-]+$ ]]
}

is_volume_name() {
  [[ "\$1" =~ ^ctx-ws-[A-Za-z0-9._:-]+$ ]]
}

is_absolute_path() {
  [[ "\$1" == /* ]]
}

is_safe_user_value() {
  [[ "\$1" =~ ^[0-9]+(:[0-9]+)?$ ]]
}

is_allowed_user_value() {
  [[ "\$1" == "\${allowed_uid}" || "\$1" == "\${allowed_uid}:\${allowed_gid}" ]]
}

is_root_user_value() {
  [[ "\$1" == "0" || "\$1" == "0:0" ]]
}

is_safe_uid_gid_pair() {
  [[ "\$1" =~ ^[0-9]+:[0-9]+$ ]]
}

is_materialization_workspace_path() {
  case "\${1:-}" in
    /ctx/ws|/ctx/ws/worktrees|/ctx/ws/worktrees/*)
      return 0
      ;;
  esac
  return 1
}

is_safe_env_assignment() {
  [[ "\$1" =~ ^[A-Z0-9_]+= ]]
}

is_allowed_root_exec_env_assignment() {
  local key="\${1%%=*}"
  case "\$key" in
    CTX_CONTAINER_TERMINAL_USER|CTX_CONTAINER_TERMINAL_HOME|CTX_CONTAINER_TERMINAL_UID|CTX_CONTAINER_TERMINAL_GID)
      [[ "\$1" != *$'\n'* ]]
      ;;
    *)
      return 1
      ;;
  esac
}

is_allowed_materialization_root_exec() {
  if [[ "\${1:-}" == "mkdir" && "\${2:-}" == "-p" && "\${3:-}" == "--" && \$# -eq 4 ]]; then
    is_materialization_workspace_path "\${4:-}" || return 1
    return 0
  fi

  if [[ "\${1:-}" == "chown" && \$# -eq 3 ]]; then
    [[ "\${2:-}" == "\${allowed_uid}:\${allowed_gid}" ]] || return 1
    is_materialization_workspace_path "\${3:-}" || return 1
    return 0
  fi

  if [[ "\${1:-}" == "sh" && "\${2:-}" == "-lc" && "\${4:-}" == "sh" && \$# -eq 5 ]]; then
    is_materialization_workspace_path "\${5:-}" || return 1
    case "\${3:-}" in
      'mkdir -p -- "\$1" && chmod 0777 "\$1"'|'find "\$1" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +')
        return 0
        ;;
    esac
  fi

  return 1
}

canonical_owned_path() {
  local raw_path="\$1"
  local resolved
  resolved="\$(readlink -f -- "\$raw_path")"
  [[ -n "\$resolved" && -e "\$resolved" ]] || return 1
  local owner_uid
  owner_uid="\$(stat -c '%u' -- "\$resolved")"
  [[ "\$owner_uid" == "\${allowed_uid}" ]] || return 1
  printf '%s\n' "\$resolved"
}

validate_mount() {
  local spec="\$1"
  local type=""
  local src=""
  local dst=""
  IFS=',' read -r -a parts <<< "\$spec"
  for part in "\${parts[@]}"; do
    case "\$part" in
      type=*) type="\${part#type=}" ;;
      src=*|source=*) src="\${part#*=}" ;;
      dst=*|target=*|destination=*) dst="\${part#*=}" ;;
      ro|rw) ;;
      *) ;;
    esac
  done
  if [[ "\$type" == "bind" ]]; then
    [[ -n "\$src" && -n "\$dst" ]] || return 1
    canonical_owned_path "\$src" >/dev/null
    is_absolute_path "\$dst" || return 1
    return 0
  fi
  if [[ "\$type" == "volume" ]]; then
    [[ -n "\$src" && -n "\$dst" ]] || return 1
    is_volume_name "\$src" || return 1
    is_absolute_path "\$dst" || return 1
    return 0
  fi
  return 1
}

validate_image_ref() {
  [[ "\$1" =~ ^[A-Za-z0-9._/@:-]+$ ]]
}

validate_exec() {
  shift
  local args=()
  local env_assignments=()
  local exec_user="\${allowed_uid}:\${allowed_gid}"
  local saw_interactive=0
  local saw_workdir=0
  while [[ \$# -gt 0 ]]; do
    case "\$1" in
      --interactive)
        saw_interactive=1
        args+=("\$1")
        shift
        ;;
      --user)
        is_safe_user_value "\${2:-}" || return 1
        if is_root_user_value "\${2:-}"; then
          exec_user="0"
        else
          is_allowed_user_value "\${2:-}" || return 1
          exec_user="\${allowed_uid}:\${allowed_gid}"
        fi
        shift 2
        ;;
      --workdir)
        is_absolute_path "\${2:-}" || return 1
        saw_workdir=1
        args+=("\$1" "\$2")
        shift 2
        ;;
      --env)
        is_safe_env_assignment "\${2:-}" || return 1
        env_assignments+=("\$2")
        args+=("\$1" "\$2")
        shift 2
        ;;
      --)
        shift
        break
        ;;
      -*)
        return 1
        ;;
      *)
        break
        ;;
    esac
  done
  is_container_name "\${1:-}" || return 1
  local container_name="\$1"
  shift
  [[ \$# -gt 0 ]] || return 1
  if [[ "\$exec_user" == "0" ]]; then
    [[ "\$saw_interactive" -eq 0 && "\$saw_workdir" -eq 0 ]] || return 1
    local assignment
    for assignment in "\${env_assignments[@]}"; do
      is_allowed_root_exec_env_assignment "\$assignment" || return 1
    done
    if [[ "\${1:-}" == "/bin/sh" && "\${2:-}" == "-lc" && \$# -eq 3 ]]; then
      [[ "\${3:-}" == *'CTX_CONTAINER_TERMINAL_USER'* ]] || return 1
      [[ "\${3:-}" == *'__CTX_CONTAINER_TERMINAL_SUDO_MISSING__'* ]] || return 1
      [[ "\${3:-}" == *'/etc/sudoers.d/\$user'* ]] || return 1
    elif is_allowed_materialization_root_exec "\$@"; then
      :
    elif [[ "\${1:-}" == "sh" && "\${2:-}" == "-c" && \$# -eq 3 ]]; then
      local script="\${3:-}"
      if [[ "\${script}" == *"command -v iptables"* && "\${script}" == *"test -x '"* ]]; then
        :
      elif [[ "\${script}" == *"ctx-egress-proxy.log"* && "\${script}" == *'echo \$! > "\$pid_file"'* ]]; then
        :
      elif [[ "\${script}" == *"failed to stop transparent proxy pid"* ]]; then
        :
      elif [[ "\${script}" == *"iptables -P OUTPUT DROP"* && "\${script}" == *"REDIRECT --to-ports"* ]]; then
        :
      elif [[ "\${script}" == *"iptables -P OUTPUT ACCEPT"* && "\${script}" == *"iptables -t nat -F OUTPUT"* ]]; then
        :
      else
        return 1
      fi
    else
      return 1
    fi
  fi
  exec "${managed_nerdctl_path}" --address "${system_containerd_address}" --namespace "${system_containerd_namespace}" exec --user "\${exec_user}" "\${args[@]}" "\$container_name" "\$@"
}

validate_run() {
  shift
  local args=(-d --user "\${allowed_uid}:\${allowed_gid}")
  local saw_detach=0
  local saw_name=0
  while [[ \$# -gt 0 ]]; do
    case "\$1" in
      -d)
        saw_detach=1
        shift
        ;;
      --name)
        is_container_name "\${2:-}" || return 1
        args+=("\$1" "\$2")
        saw_name=1
        shift 2
        ;;
      --hostname)
        [[ "\${2:-}" =~ ^[A-Za-z0-9.-]+$ ]] || return 1
        args+=("\$1" "\$2")
        shift 2
        ;;
      --userns=keep-id)
        shift
        ;;
      --user)
        is_safe_user_value "\${2:-}" || return 1
        is_allowed_user_value "\${2:-}" || return 1
        shift 2
        ;;
      --network=slirp4netns:allow_host_loopback=true|--net=slirp4netns:allow_host_loopback=true)
        shift
        ;;
      --network|--net)
        [[ "\${2:-}" == "slirp4netns:allow_host_loopback=true" ]] || return 1
        shift 2
        ;;
      --cap-add)
        [[ "\${2:-}" == "NET_ADMIN" ]] || return 1
        args+=("\$1" "\$2")
        shift 2
        ;;
      --add-host)
        [[ "\${2:-}" == "host.containers.internal:host-gateway" ]] || return 1
        args+=("\$1" "\$2")
        shift 2
        ;;
      --mount)
        validate_mount "\${2:-}" || return 1
        args+=("\$1" "\$2")
        shift 2
        ;;
      -*)
        return 1
        ;;
      *)
        break
        ;;
    esac
  done
  [[ "\${saw_detach}" -eq 1 && "\${saw_name}" -eq 1 ]] || return 1
  validate_image_ref "\${1:-}" || return 1
  local image_ref="\$1"
  shift
  [[ "\${1:-}" == "/bin/sh" && "\${2:-}" == "-c" && "\${3:-}" == "while true; do sleep 100000; done" ]] || return 1
  shift 3
  [[ \$# -eq 0 ]] || return 1
  exec "${managed_nerdctl_path}" --address "${system_containerd_address}" --namespace "${system_containerd_namespace}" --snapshotter native run "\${args[@]}" "\$image_ref" /bin/sh -c "while true; do sleep 100000; done"
}

validate_simple_named_command() {
  local verb="\$1"
  shift
  case "\$verb" in
    start)
      is_container_name "\${1:-}" || return 1
      [[ \$# -eq 1 ]] || return 1
      exec "${managed_nerdctl_path}" --address "${system_containerd_address}" --namespace "${system_containerd_namespace}" start "\$1"
      ;;
    rm)
      [[ "\${1:-}" == "-f" ]] || return 1
      is_container_name "\${2:-}" || return 1
      [[ \$# -eq 2 ]] || return 1
      exec "${managed_nerdctl_path}" --address "${system_containerd_address}" --namespace "${system_containerd_namespace}" rm -f "\$2"
      ;;
    inspect)
      is_container_name "\${1:-}" || return 1
      [[ \$# -eq 1 ]] || return 1
      exec "${managed_nerdctl_path}" --address "${system_containerd_address}" --namespace "${system_containerd_namespace}" inspect "\$1"
      ;;
    *)
      return 1
      ;;
  esac
}

validate_volume() {
  [[ \$# -ge 2 ]] || return 1
  local subcommand="\$2"
  case "\$subcommand" in
    inspect|create)
      is_volume_name "\${3:-}" || return 1
      [[ \$# -eq 3 ]] || return 1
      exec "${managed_nerdctl_path}" --address "${system_containerd_address}" --namespace "${system_containerd_namespace}" volume "\$subcommand" "\$3"
      ;;
    rm)
      [[ "\${3:-}" == "-f" ]] || return 1
      is_volume_name "\${4:-}" || return 1
      [[ \$# -eq 4 ]] || return 1
      exec "${managed_nerdctl_path}" --address "${system_containerd_address}" --namespace "${system_containerd_namespace}" volume rm -f "\$4"
      ;;
    ls)
      [[ "\${3:-}" == "--format" && "\${4:-}" == "{{.Name}}" && \$# -eq 4 ]] || return 1
      exec "${managed_nerdctl_path}" --address "${system_containerd_address}" --namespace "${system_containerd_namespace}" volume ls --format "{{.Name}}"
      ;;
    *)
      return 1
      ;;
  esac
}

validate_container() {
  [[ \$# -ge 2 ]] || return 1
  [[ "\$2" == "inspect" ]] || return 1
  if [[ "\${3:-}" == "--format" ]]; then
    [[ "\${4:-}" == "{{.State.Running}}" ]] || return 1
    is_container_name "\${5:-}" || return 1
    [[ \$# -eq 5 ]] || return 1
    exec "${managed_nerdctl_path}" --address "${system_containerd_address}" --namespace "${system_containerd_namespace}" container inspect --format "{{.State.Running}}" "\$5"
  fi
  is_container_name "\${3:-}" || return 1
  [[ \$# -eq 3 ]] || return 1
  exec "${managed_nerdctl_path}" --address "${system_containerd_address}" --namespace "${system_containerd_namespace}" container inspect "\$3"
}

validate_image() {
  [[ "\${2:-}" == "inspect" ]] || return 1
  validate_image_ref "\${3:-}" || return 1
  [[ \$# -eq 3 ]] || return 1
  exec "${managed_nerdctl_path}" --address "${system_containerd_address}" --namespace "${system_containerd_namespace}" image inspect "\$3"
}

validate_load() {
  [[ "\${2:-}" == "-i" ]] || return 1
  local archive_path
  archive_path="\$(canonical_owned_path "\${3:-}")" || return 1
  [[ \$# -eq 3 ]] || return 1
  exec "${managed_nerdctl_path}" --address "${system_containerd_address}" --namespace "${system_containerd_namespace}" load -i "\$archive_path"
}

case "\${1:-}" in
  info)
    [[ \$# -eq 1 ]] || exit 1
    exec "${managed_nerdctl_path}" --address "${system_containerd_address}" --namespace "${system_containerd_namespace}" info
    ;;
  exec)
    validate_exec "\$@"
    ;;
  run)
    validate_run "\$@"
    ;;
  start|rm|inspect)
    validate_simple_named_command "\$@"
    ;;
  volume)
    validate_volume "\$@"
    ;;
  container)
    validate_container "\$@"
    ;;
  image)
    validate_image "\$@"
    ;;
  load)
    validate_load "\$@"
    ;;
esac
echo "error: unsupported ctx sandbox wrapper invocation: \$*" >&2
exit 1
EOF
  install -m 0755 "${tmp_dir}/ctx-rootful-nerdctl" "${wrapper_path}"
  rm -rf "${tmp_dir}"
}

install_sudoers_rule() {
  local user_name="$1"
  validate_allow_user_name "${user_name}"
  local sudoers_path="/etc/sudoers.d/ctx-managed-nerdctl-${user_name}"
  cat > "${sudoers_path}" <<EOF
${user_name} ALL=(root) NOPASSWD: ${wrapper_path}
EOF
  chmod 0440 "${sudoers_path}"
}

ensure_containerd_running() {
  if ! command -v systemctl >/dev/null 2>&1; then
    echo "error: systemctl is required for Linux sandbox activation" >&2
    exit 1
  fi
  systemctl enable --now containerd.service
  for _ in $(seq 1 20); do
    if [[ -S "${system_containerd_address}" ]]; then
      return 0
    fi
    sleep 1
  done
  echo "error: containerd socket did not become ready" >&2
  exit 1
}

mark_ready() {
  mkdir -p "${bootstrap_root}"
  : > "${ready_marker}"
}

emit_current_status() {
  if [[ -f "${ready_marker}" && -x "${wrapper_path}" ]]; then
    if [[ -S "${system_containerd_address}" ]] && "${wrapper_path}" info >/dev/null 2>&1; then
      write_status "ready" true "" "${distro}"
    else
      write_status "failed" true "Installed Linux sandbox runtime is not healthy." "${distro}"
    fi
  elif [[ -f "${staged_archive_path}" ]]; then
    if verify_nerdctl_checksum "${arch}" "${staged_archive_path}"; then
      write_status "downloaded_not_activated" true "" "${distro}"
    else
      rm -f "${staged_archive_path}"
      write_status "failed" true "Staged Linux sandbox runtime download failed verification." "${distro}"
    fi
  else
    write_status "download_pending" true "" "${distro}"
  fi
  status_json
}

if [[ "$(uname -s)" != "Linux" ]]; then
  write_status "manual_runtime_required" false "Linux sandbox bootstrap is only available on Linux." ""
  status_json
  exit 0
fi

arch="$(detect_arch)"
if [[ "${arch}" == "unsupported" ]]; then
  write_status "failed" false "Unsupported Linux architecture." "$(detect_distro)"
  status_json
  exit 1
fi

distro="$(detect_distro)"
if ! distro_supported; then
  write_status "manual_runtime_required" false "Managed sandbox bootstrap is currently supported on Ubuntu/Debian only." "${distro}"
  status_json
  exit 0
fi

staged_archive_path="$(staged_nerdctl_archive_path "${arch}")"

if [[ "${mode}" == "status" ]]; then
  emit_current_status
  exit 0
fi

if [[ "${mode}" == "stage" ]]; then
  if [[ -f "${ready_marker}" && -x "${wrapper_path}" ]]; then
    emit_current_status
    exit 0
  fi
  write_status "downloading" true "" "${distro}"
  download_nerdctl "${arch}"
  stage_apt_debs || true
  write_status "downloaded_not_activated" true "" "${distro}"
  status_json
  exit 0
fi

if [[ "${mode}" == "activate" ]]; then
  if [[ "$(id -u)" -ne 0 ]]; then
    echo "error: activation must run as root" >&2
    exit 1
  fi
  install_managed_nerdctl "${arch}"
  install_rootful_wrapper "${allow_user}"
  install_sudoers_rule "${allow_user}"
  install_apt_requirements
  ensure_containerd_running
  mark_ready
  write_status "ready" true "" "${distro}"
  status_json
  exit 0
fi

echo "error: unsupported mode: ${mode}" >&2
exit 2
