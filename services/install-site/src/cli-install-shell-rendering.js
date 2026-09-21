export function renderCliInstallShellRendering({
  normalizedReleaseFunctionsBase,
  normalizedInstallTelemetryEndpoint,
  normalizedChannel,
  normalizedInstallUrl,
  normalizedInstallAttemptId,
}) {
  return `#!/bin/sh
set -eu

log() {
  printf '%s\\n' "$*" >&2
}

styled_output=0
if [ -z "\${NO_COLOR+x}" ] && [ -t 2 ]; then
  styled_output=1
fi

receipt_item() {
  if [ "$styled_output" = "1" ]; then
    printf '\\033[32m✓\\033[0m %s\\n' "$*" >&2
  else
    log "$*"
  fi
}

receipt_warning() {
  log "warning: $*"
}

usage() {
  cat <<'USAGE'
usage: curl -fsSL ${normalizedInstallUrl} | sh
       curl -fsSL ${normalizedInstallUrl} | sh -s -- --no-setup
       curl -fsSL ${normalizedInstallUrl} | sh -s -- --no-daemon

Installs the ctx CLI from signed release metadata, installs the bundled
agent-history skill, then runs ctx setup to index discovered local agent
history.

Prerequisites:
  curl, OpenSSL, install, ln, mktemp, awk, date, id, sort, stat, uname

Options:
  --semantic           Explicitly enable signed Semantic runtime provisioning
                       and semantic setup for this install.
  --no-setup           Install only; do not install the skill or run ctx setup
                       unless a skill option is also passed.
  --no-daemon          Run installer setup with ctx setup --no-daemon.
  --no-skill           Do not install the bundled ctx agent skill.
  --skill-agent AGENT  Install the skill into a specific agent skill dir.
                       Repeat for multiple agents.
  --all-skill-agents   Install the skill into all supported agent skill dirs.
  --no-modify-path     Do not update shell startup files when the install
                       directory is not on PATH.
  --no-man             Do not install generated man pages.
  --man-dir D          Man page directory. Defaults to $HOME/.local/share/man/man1.
  -h, --help           Show this help.

Environment:
  CTX_INSTALL_SEMANTIC=1              Enable signed Semantic runtime provisioning.
  CTX_SEARCH_SEMANTIC=true|false      Override persisted Semantic search for this install.
  CTX_INSTALL_NO_SETUP=1             Install only; do not install the skill or run ctx setup
                                     unless a skill option is also passed.
  CTX_INSTALL_NO_DAEMON=1            Run installer setup with ctx setup --no-daemon.
  CTX_INSTALL_NO_SKILL=1             Do not install the bundled ctx agent skill.
  CTX_INSTALL_SKILL_AGENTS=codex,... Install the skill into specific agent dirs.
  CTX_INSTALL_ALL_SKILL_AGENTS=1     Install the skill into all supported agent dirs.
  CTX_INSTALL_NO_MODIFY_PATH=1       Do not update shell startup files.
  CTX_INSTALL_NO_MAN=1               Do not install generated man pages.
  CTX_MAN_DIR=$HOME/.local/share/man/man1
                                     Override man page install directory.
  CTX_SETUP_PROGRESS=auto            Setup progress mode: auto, plain, or none.
  CTX_ANALYTICS_ENABLED=false        Disable installer diagnostics and CLI analytics.
  CTX_DAEMON_ENABLED=false           Disable daemon maintenance. Effective Semantic
                                     installation requires daemon maintenance to remain enabled.
  CTX_INSTALL_ATTEMPT_ID=ia_...      Override installer attempt ID for tests.
  CTX_ALLOW_CUSTOM_RELEASE_BASE_URL=1
                                     Allow non-cli.ctx.rs artifact metadata for development.
  CTX_RELEASE_METADATA_SIGNATURE_URL Override detached metadata signature URL.
USAGE
}

fail() {
  if command -v stop_install_animation >/dev/null 2>&1; then
    stop_install_animation
  fi
  log "error: $*"
  exit 1
}

legacy_control_truthy() {
  legacy_value="$1"
  while :; do
    case "$legacy_value" in
      [[:space:]]*) legacy_value="\${legacy_value#?}" ;;
      *) break ;;
    esac
  done
  while :; do
    case "$legacy_value" in
      *[[:space:]]) legacy_value="\${legacy_value%?}" ;;
      *) break ;;
    esac
  done
  case "$legacy_value" in
    ""|0|[Ff][Aa][Ll][Ss][Ee]|[Nn][Oo]|[Oo][Ff][Ff]) return 1 ;;
    *) return 0 ;;
  esac
}

canonical_analytics_disabled() {
  analytics_value="\${CTX_ANALYTICS_ENABLED-}"
  while :; do
    case "$analytics_value" in
      [[:space:]]*) analytics_value="\${analytics_value#?}" ;;
      *) break ;;
    esac
  done
  while :; do
    case "$analytics_value" in
      *[[:space:]]) analytics_value="\${analytics_value%?}" ;;
      *) break ;;
    esac
  done
  case "$analytics_value" in
    0|[Ff][Aa][Ll][Ss][Ee]|[Nn][Oo]|[Oo][Ff][Ff]) return 0 ;;
    *) return 1 ;;
  esac
}

valid_install_attempt_id() {
  attempt_id_value="$1"
  case "$attempt_id_value" in
    ia_*) ;;
    *) return 1 ;;
  esac
  case "$attempt_id_value" in
    *[!A-Za-z0-9_-]*) return 1 ;;
  esac
  attempt_id_length="\${#attempt_id_value}"
  [ "$attempt_id_length" -ge 11 ] && [ "$attempt_id_length" -le 131 ]
}

deprecated_control_warning=
note_deprecated_control() {
  deprecated_mapping="$1 -> $2"
  if [ -n "$deprecated_control_warning" ]; then
    deprecated_control_warning="$deprecated_control_warning; $deprecated_mapping"
  else
    deprecated_control_warning="$deprecated_mapping"
  fi
}

apply_deprecated_controls() {
  if [ "\${CTX_ANALYTICS_OFF+x}" = x ]; then
    note_deprecated_control CTX_ANALYTICS_OFF CTX_ANALYTICS_ENABLED=false
    if legacy_control_truthy "$CTX_ANALYTICS_OFF"; then
      CTX_ANALYTICS_ENABLED=false
      export CTX_ANALYTICS_ENABLED
    fi
  fi
  if [ "\${CTX_DISABLE_ANALYTICS+x}" = x ]; then
    note_deprecated_control CTX_DISABLE_ANALYTICS CTX_ANALYTICS_ENABLED=false
    if legacy_control_truthy "$CTX_DISABLE_ANALYTICS"; then
      CTX_ANALYTICS_ENABLED=false
      export CTX_ANALYTICS_ENABLED
    fi
  fi
  if [ "\${CTX_INSTALL_DIAGNOSTICS_OFF+x}" = x ]; then
    note_deprecated_control CTX_INSTALL_DIAGNOSTICS_OFF CTX_ANALYTICS_ENABLED=false
    if legacy_control_truthy "$CTX_INSTALL_DIAGNOSTICS_OFF"; then
      CTX_ANALYTICS_ENABLED=false
      export CTX_ANALYTICS_ENABLED
    fi
  fi
  if [ "\${CTX_DAEMON_OFF+x}" = x ]; then
    note_deprecated_control CTX_DAEMON_OFF CTX_DAEMON_ENABLED=false
    if legacy_control_truthy "$CTX_DAEMON_OFF"; then
      CTX_DAEMON_ENABLED=false
      export CTX_DAEMON_ENABLED
    fi
  fi
  if [ "\${CTX_DISABLE_DAEMON+x}" = x ]; then
    note_deprecated_control CTX_DISABLE_DAEMON CTX_DAEMON_ENABLED=false
    if legacy_control_truthy "$CTX_DISABLE_DAEMON"; then
      CTX_DAEMON_ENABLED=false
      export CTX_DAEMON_ENABLED
    fi
  fi
  if [ "\${CTX_UPGRADE_OFF+x}" = x ]; then
    note_deprecated_control CTX_UPGRADE_OFF CTX_UPGRADE_AUTO=off
    if legacy_control_truthy "$CTX_UPGRADE_OFF"; then
      CTX_UPGRADE_AUTO=off
      export CTX_UPGRADE_AUTO
    fi
  fi
  if [ "\${CTX_DISABLE_AUTO_UPGRADE+x}" = x ]; then
    note_deprecated_control CTX_DISABLE_AUTO_UPGRADE CTX_UPGRADE_AUTO=off
    if legacy_control_truthy "$CTX_DISABLE_AUTO_UPGRADE"; then
      CTX_UPGRADE_AUTO=off
      export CTX_UPGRADE_AUTO
    fi
  fi
  unset CTX_ANALYTICS_OFF CTX_DISABLE_ANALYTICS CTX_INSTALL_DIAGNOSTICS_OFF
  unset CTX_DAEMON_OFF CTX_DISABLE_DAEMON CTX_UPGRADE_OFF CTX_DISABLE_AUTO_UPGRADE
  if [ -n "$deprecated_control_warning" ]; then
    log "warning: deprecated environment variables detected: $deprecated_control_warning. Update your environment to use the replacements."
  fi
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

append_skill_agent() {
  agent="$1"
  test -n "$agent" || fail "--skill-agent requires a value"
  case "$agent" in
    *'
'*) fail "invalid skill agent: $agent" ;;
  esac
  if [ -n "$skill_agents" ]; then
    skill_agents="$skill_agents
$agent"
  else
    skill_agents="$agent"
  fi
}

ci_environment() {
  ci_value="$(
    printf '%s' "\${CI-}" |
      LC_ALL=C sed 's/^[[:space:]]*//; s/[[:space:]]*$//'
  )"
  case "$ci_value" in
    1|[Tt][Rr][Uu][Ee]|[Yy][Ee][Ss]|[Oo][Nn]) return 0 ;;
    *) return 1 ;;
  esac
}

has_controlling_tty() {
  ( : </dev/tty ) 2>/dev/null && ( : >/dev/tty ) 2>/dev/null
}

release_functions_base="\${CTX_UPGRADE_FUNCTIONS_BASE:-${normalizedReleaseFunctionsBase}}"
install_telemetry_endpoint="${normalizedInstallTelemetryEndpoint}"
channel="\${CTX_UPGRADE_CHANNEL:-${normalizedChannel}}"
if [ "$channel" != "stable" ] &&
   [ "$release_functions_base" = "https://cli.ctx.rs/functions/v2" ] &&
   [ -z "\${CTX_UPGRADE_FUNCTIONS_BASE:-}" ]; then
  release_functions_base="https://cli.ctx.rs/functions/v1"
fi
install_attempt_id="${normalizedInstallAttemptId}"
if [ -n "\${CTX_INSTALL_ATTEMPT_ID-}" ] && valid_install_attempt_id "$CTX_INSTALL_ATTEMPT_ID"; then
  install_attempt_id="$CTX_INSTALL_ATTEMPT_ID"
fi
unset CTX_INSTALL_ATTEMPT_ID
explicit_metadata=0
if [ -n "\${CTX_RELEASE_METADATA_URL:-}\${CTX_RELEASE_METADATA_SIGNATURE_URL:-}\${CTX_UPGRADE_FUNCTIONS_BASE:-}" ]; then
  explicit_metadata=1
fi
metadata_url="\${CTX_RELEASE_METADATA_URL:-\${release_functions_base%/}/releases/$channel/ctx-release-metadata.env}"
metadata_signature_url="\${CTX_RELEASE_METADATA_SIGNATURE_URL:-$metadata_url.sig}"
bin_dir="\${CTX_BIN_DIR:-\${HOME:-}/.local/bin}"
man_dir="\${CTX_MAN_DIR:-\${HOME:-}/.local/share/man/man1}"
run_setup=1
setup_no_daemon=0
run_skill=1
modify_path=1
no_skill_requested=0
explicit_skill_request=0
all_skill_agents=0
skill_agents=
install_man=1
semantic_enabled=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --semantic)
      semantic_enabled=1
      ;;
    --no-setup)
      run_setup=0
      ;;
    --no-daemon)
      setup_no_daemon=1
      ;;
    --pro-trial|--no-pro-trial)
      # Published legacy tokens are inert.
      ;;
    --no-skill)
      run_skill=0
      no_skill_requested=1
      ;;
    --skill-agent)
      shift
      append_skill_agent "\${1:-}"
      explicit_skill_request=1
      ;;
    --all-skill-agents)
      all_skill_agents=1
      explicit_skill_request=1
      ;;
    --no-modify-path)
      modify_path=0
      ;;
    --no-man)
      install_man=0
      ;;
    --man-dir)
      shift
      man_dir="\${1:-}"
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
  shift
done

test -n "$bin_dir" || fail "CTX_BIN_DIR is empty and HOME is unavailable"
test -n "$man_dir" || fail "CTX_MAN_DIR is empty and --man-dir was not provided"
secure_bin_dir="\${bin_dir%/}"
case "$secure_bin_dir" in
  /*) ;;
  *) fail "ctx install directory must be an absolute path" ;;
esac
case "$secure_bin_dir$man_dir" in
  *[[:cntrl:]]*) fail "installer paths must not contain control characters" ;;
esac
[ ! -L "$secure_bin_dir" ] || fail "ctx install directory must not be a symlink"

need_cmd awk
need_cmd curl
need_cmd date
need_cmd id
need_cmd install
need_cmd ln
need_cmd mktemp
need_cmd openssl
need_cmd sort
need_cmd stat
need_cmd uname

`;
}
