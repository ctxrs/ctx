pub(super) fn sandbox_mount_parent_chain_functions_script() -> &'static str {
    r#"
ensure_mount_parent_chain() {
  root="$1"
  target="$2"
  if [ -L "$root" ]; then
    printf 'attachment mount worktree root must not be a symlink: %s\n' "$root" >&2
    exit 2
  fi
  if [ ! -d "$root" ]; then
    printf 'attachment mount worktree root must be a directory: %s\n' "$root" >&2
    exit 2
  fi
  case "$target" in
    "$root"/*) rel="${target#"$root"/}" ;;
    *)
      printf 'attachment mount path escapes sandbox worktree: %s\n' "$target" >&2
      exit 2
      ;;
  esac
  parent="${rel%/*}"
  if [ "$parent" = "$rel" ]; then
    return 0
  fi
  current="$root"
  remaining="$parent"
  while [ -n "$remaining" ]; do
    segment="${remaining%%/*}"
    if [ "$remaining" = "$segment" ]; then
      remaining=""
    else
      remaining="${remaining#*/}"
    fi
    if [ -z "$segment" ] || [ "$segment" = "." ] || [ "$segment" = ".." ]; then
      printf 'attachment mount path contains unsupported component: %s\n' "$target" >&2
      exit 2
    fi
    current="$current/$segment"
    if [ -L "$current" ]; then
      printf 'attachment mount parent must not be a symlink: %s\n' "$current" >&2
      exit 2
    fi
    if [ ! -e "$current" ]; then
      mkdir "$current" 2>/dev/null || true
    fi
    if [ -L "$current" ]; then
      printf 'attachment mount parent must not be a symlink: %s\n' "$current" >&2
      exit 2
    fi
    if [ ! -d "$current" ]; then
      printf 'attachment mount parent must be a directory: %s\n' "$current" >&2
      exit 2
    fi
  done
}

remove_mount_path_if_parent_safe() {
  root="$1"
  target="$2"
  if [ -L "$root" ]; then
    printf 'attachment mount worktree root must not be a symlink: %s\n' "$root" >&2
    exit 2
  fi
  if [ ! -d "$root" ]; then
    exit 0
  fi
  case "$target" in
    "$root"/*) rel="${target#"$root"/}" ;;
    *)
      printf 'attachment mount path escapes sandbox worktree: %s\n' "$target" >&2
      exit 2
      ;;
  esac
  parent="${rel%/*}"
  if [ "$parent" != "$rel" ]; then
    current="$root"
    remaining="$parent"
    while [ -n "$remaining" ]; do
      segment="${remaining%%/*}"
      if [ "$remaining" = "$segment" ]; then
        remaining=""
      else
        remaining="${remaining#*/}"
      fi
      if [ -z "$segment" ] || [ "$segment" = "." ] || [ "$segment" = ".." ]; then
        printf 'attachment mount path contains unsupported component: %s\n' "$target" >&2
        exit 2
      fi
      current="$current/$segment"
      if [ -L "$current" ]; then
        printf 'attachment mount parent must not be a symlink: %s\n' "$current" >&2
        exit 2
      fi
      if [ ! -e "$current" ]; then
        exit 0
      fi
      if [ ! -d "$current" ]; then
        printf 'attachment mount parent must be a directory: %s\n' "$current" >&2
        exit 2
      fi
    done
  fi
  if [ -L "$target" ]; then
    :
  elif [ -e "$target" ]; then
    chmod -R u+w -- "$target"
  fi
  rm -rf -- "$target"
}
"#
}

#[cfg(test)]
pub(super) fn sandbox_mount_parent_chain_ensure_test_script() -> String {
    format!(
        "{}\nensure_mount_parent_chain \"$1\" \"$2\"\n",
        sandbox_mount_parent_chain_functions_script()
    )
}
