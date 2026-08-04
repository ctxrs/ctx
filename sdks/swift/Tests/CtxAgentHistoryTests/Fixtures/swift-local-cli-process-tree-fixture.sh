#!/bin/sh
set -eu

mode="$1"
root_pid_file="$2"
child_pid_file="$3"
alive_file="$4"

printf '%s\n' "$$" >"$root_pid_file"

spawn_ignored_term_child() {
  /bin/sh -c '
    trap "" TERM
    printf "%s\n" "$$" >"$1"
    sleep 1
    printf "alive\n" >"$2"
    sleep 5
  ' swift-fixture-child "$child_pid_file" "$alive_file" &

  while [ ! -s "$child_pid_file" ]; do
    sleep 0.01
  done
}

case "$mode" in
  large-output)
    awk 'BEGIN { for (i = 0; i < 262144; i++) printf "o" }'
    awk 'BEGIN { for (i = 0; i < 262144; i++) printf "e" }' >&2
    ;;
  large-stderr)
    printf 'ok\n'
    awk 'BEGIN { for (i = 0; i < 262144; i++) printf "e" }' >&2
    ;;
  max-mcp-json)
    printf '%s' '{"event":{"mcp_tool_call":{"server":"'
    awk 'BEGIN { for (i = 0; i < 65536; i++) printf "s" }'
    printf '%s' '","tool":"'
    awk 'BEGIN { for (i = 0; i < 65536; i++) printf "t" }'
    printf '%s\n' '"}},"events":[]}'
    ;;
  persistent-descendant)
    spawn_ignored_term_child
    exit 0
    ;;
  ignored-term-tree)
    trap '' TERM
    spawn_ignored_term_child
    wait
    ;;
  *)
    printf 'unknown fixture mode: %s\n' "$mode" >&2
    exit 64
    ;;
esac
