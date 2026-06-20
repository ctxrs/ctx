#!/usr/bin/env bash

set -euo pipefail

ctx_bin="${1:?set ctx binary path}"
smoke_case="${2:?set smoke case}"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT
ctx_copy="$tmpdir/ctx"
cp "$ctx_bin" "$ctx_copy"
chmod +x "$ctx_copy"

case "$smoke_case" in
  root-help)
    root_help="$("$ctx_copy" --help)"
    printf '%s\n' "$root_help" | grep -F "ctx daemon and CLI" >/dev/null
    printf '%s\n' "$root_help" | grep -F "serve" >/dev/null
    printf '%s\n' "$root_help" | grep -F "init" >/dev/null
    printf '%s\n' "$root_help" | grep -F "setup" >/dev/null
    printf '%s\n' "$root_help" | grep -F "work" >/dev/null
    printf '%s\n' "$root_help" | grep -F "self-update" >/dev/null
    ;;
  agent-work-help)
    agent_work_help="$("$ctx_copy" work --help)"
    printf '%s\n' "$agent_work_help" | grep -F "schema" >/dev/null
    agent_work_alias_help="$("$ctx_copy" agent-work --help)"
    printf '%s\n' "$agent_work_alias_help" | grep -F "schema" >/dev/null
    ;;
  agent-work-schema)
    for kind in work-bundle agent-work change-set contribution events tool-call transcripts plugin-manifest; do
      "$ctx_copy" work schema --kind "$kind" > "$tmpdir/$kind.json"
      grep -E '^[[:space:]]*\{' "$tmpdir/$kind.json" >/dev/null
      grep -E '"\$schema"[[:space:]]*:' "$tmpdir/$kind.json" >/dev/null
    done
    "$ctx_copy" agent-work schema --kind agent-work > "$tmpdir/agent-work-alias.json"
    grep -E '"\$id"[[:space:]]*:[[:space:]]*"https://schemas.ctx.rs/work/bundle.v1.schema.json"' "$tmpdir/work-bundle.json" >/dev/null
    grep -E '"\$id"[[:space:]]*:[[:space:]]*"https://schemas.ctx.rs/agent-work/v1.schema.json"' "$tmpdir/agent-work.json" >/dev/null
    grep -E '"\$id"[[:space:]]*:[[:space:]]*"https://schemas.ctx.rs/agent-work/v1.schema.json"' "$tmpdir/agent-work-alias.json" >/dev/null
    grep -E '"title"[[:space:]]*:[[:space:]]*"ChangeSet"' "$tmpdir/change-set.json" >/dev/null
    grep -E '"title"[[:space:]]*:[[:space:]]*"Contribution"' "$tmpdir/contribution.json" >/dev/null
    grep -E '"title"[[:space:]]*:[[:space:]]*"TranscriptRecord"' "$tmpdir/transcripts.json" >/dev/null
    cat > "$tmpdir/bad-work-bundle.json" <<'JSON'
{
  "kind": "ctx.work.bundle",
  "schema_version": 1,
  "objects": [
    {
      "path": "../secret.json"
    }
  ]
}
JSON
    if "$ctx_copy" work validate --kind work-bundle "$tmpdir/bad-work-bundle.json" >"$tmpdir/bad-work-bundle.out" 2>&1; then
      echo "expected work-bundle traversal validation to fail" >&2
      exit 1
    fi
    grep -E 'traversal|dot-dot' "$tmpdir/bad-work-bundle.out" >/dev/null
    ;;
  setup-help)
    setup_help="$("$ctx_copy" setup --help)"
    printf '%s\n' "$setup_help" | grep -F "workspace" >/dev/null
    printf '%s\n' "$setup_help" | grep -F "scratch" >/dev/null
    printf '%s\n' "$setup_help" | grep -F "uninstall" >/dev/null
    ;;
  work-cli-flow)
    data_root="$tmpdir/data"
    repo="$tmpdir/repo"
    mkdir -p "$repo"
    git init --quiet "$repo"
    setup_out="$("$ctx_copy" setup workspace --data-dir "$data_root" --no-shims "$repo")"
    printf '%s\n' "$setup_out" | grep -F "workspace:" >/dev/null
    printf '%s\n' "$setup_out" | grep -F "shims: skipped" >/dev/null

    capture_out="$("$ctx_copy" work capture command --data-dir "$data_root" --cwd "$repo" --tool gh --exit-code 0 -- pr view 123 --repo ctxrs/ctx)"
    printf '%s\n' "$capture_out" | grep -F "captured:" >/dev/null

    link_out="$("$ctx_copy" work link-pr --data-dir "$data_root" --cwd "$repo" https://github.com/ctxrs/ctx/pull/123)"
    printf '%s\n' "$link_out" | grep -F "change_set:" >/dev/null
    printf '%s\n' "$link_out" | grep -F "pull_request: ctxrs/ctx/#123" >/dev/null

    recent_out="$("$ctx_copy" work recent --data-dir "$data_root")"
    printf '%s\n' "$recent_out" | grep -F "recent_change_sets:" >/dev/null
    printf '%s\n' "$recent_out" | grep -F "recent_contributions:" >/dev/null

    list_out="$("$ctx_copy" work list --data-dir "$data_root" --kind contribution)"
    printf '%s\n' "$list_out" | grep -F "contributions:" >/dev/null
    ;;
  serve-help)
    serve_help="$("$ctx_copy" serve --help)"
    printf '%s\n' "$serve_help" | grep -F -- "--bind" >/dev/null
    printf '%s\n' "$serve_help" | grep -F -- "--data-dir" >/dev/null
    ;;
  init-help)
    init_help="$("$ctx_copy" init --help)"
    printf '%s\n' "$init_help" | grep -F -- "--root" >/dev/null
    ;;
  self-update-help)
    self_update_help="$("$ctx_copy" self-update --help)"
    printf '%s\n' "$self_update_help" | grep -F -- "--channel" >/dev/null
    printf '%s\n' "$self_update_help" | grep -F -- "--check" >/dev/null
    printf '%s\n' "$self_update_help" | grep -F -- "--yes" >/dev/null
    ;;
  *)
    echo "unknown ctx bin smoke case: $smoke_case" >&2
    exit 2
    ;;
esac
