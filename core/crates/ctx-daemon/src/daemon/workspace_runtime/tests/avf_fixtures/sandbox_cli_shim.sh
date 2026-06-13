#!/bin/sh
data_root="$1"
shift
vm_root="$data_root/managed/vms/avf-linux/__HOST_OS__/__HOST_ARCH__/shared"
containers_root="$vm_root/test-containers"
volumes_root="$vm_root/test-volumes"
images_root="$vm_root/test-images"
log_path="$vm_root/sandbox-cli-invocations.log"
mkdir -p "$containers_root" "$volumes_root" "$images_root" "$(dirname "$log_path")"
printf '%s\n' "$*" >> "$log_path"

container_dir() {
  printf '%s' "$containers_root/$1"
}

container_rootfs() {
  printf '%s' "$(container_dir "$1")/rootfs"
}

container_mounts_file() {
  printf '%s' "$(container_dir "$1")/mounts"
}

container_state_file() {
  printf '%s' "$(container_dir "$1")/state"
}

ensure_container_dir() {
  mkdir -p "$(container_dir "$1")"
  mkdir -p "$(container_rootfs "$1")"
}

map_container_path() {
  container_name="$1"
  guest_path="$2"
  mounts_file="$(container_mounts_file "$container_name")"
  if [ ! -f "$mounts_file" ]; then
    printf '%s\n' "$guest_path"
    return 0
  fi
  while IFS='|' read -r mount_type mount_src mount_dst mount_mode; do
    [ -n "$mount_type" ] || continue
    host_src="$mount_src"
    if [ "$mount_type" = "volume" ]; then
      host_src="$volumes_root/$mount_src"
    fi
    case "$guest_path" in
      "$mount_dst")
        printf '%s\n' "$host_src"
        return 0
        ;;
      "$mount_dst"/*)
        rel=$(printf '%s' "$guest_path" | sed "s#^$mount_dst/##")
        printf '%s\n' "$host_src/$rel"
        return 0
        ;;
    esac
  done < "$mounts_file"
  printf '%s\n' "$guest_path"
}

map_non_shell_args() {
  container_name="$1"
  shift
  for arg in "$@"; do
    mapped="$arg"
    case "$arg" in
      /*) mapped="$(map_container_path "$container_name" "$arg")" ;;
    esac
    printf '%s\n' "$mapped"
  done
}

should_short_circuit_network_policy_script() {
  script="$1"
  case "$script" in
    *ctx-egress-proxy*|*iptables*|*pid_file=*|*CTX_CONTAINER_TERMINAL_USER*|*CTX_CONTAINER_TERMINAL_HOME*|*sudoers.d/*)
      return 0
      ;;
  esac
  return 1
}

run_exec() {
  pty=0
  workdir="/"
  while [ $# -gt 0 ]; do
    case "$1" in
      --interactive|--tty) shift ;;
      --user) shift 2 ;;
      --env)
        kv="$2"
        key=$(printf '%s' "$kv" | sed 's/=.*//')
        value=$(printf '%s' "$kv" | sed 's/^[^=]*=//')
        export "$key=$value"
        shift 2
        ;;
      --workdir) workdir="$2"; shift 2 ;;
      *)
        break
        ;;
    esac
  done
  container_name="$1"
  shift
  command_name="$1"
  shift
  host_cwd="$(map_container_path "$container_name" "$workdir")"
  mkdir -p "$host_cwd"
  case "$command_name" in
    sh|/bin/sh|bash|/bin/bash)
      if ( [ "$1" = "-c" ] || [ "$1" = "-lc" ] ) && should_short_circuit_network_policy_script "$2"; then
        exit 0
      fi
      (cd "$host_cwd" && exec "$command_name" "$@")
      ;;
    *)
      mapped_lines="$(map_non_shell_args "$container_name" "$@")"
      set --
      while IFS= read -r arg; do
        set -- "$@" "$arg"
      done <<EOF
$mapped_lines
EOF
      (cd "$host_cwd" && exec "$command_name" "$@")
      ;;
  esac
}

run_cp() {
  src="$1"
  dest_spec="$2"
  container_name=$(printf '%s' "$dest_spec" | sed 's/:.*$//')
  guest_dest=$(printf '%s' "$dest_spec" | sed 's/^[^:]*://')
  host_dest="$(map_container_path "$container_name" "$guest_dest")"
  mkdir -p "$host_dest"
  case "$src" in
    */.)
      src_dir=$(dirname "$src")
      cp -R "$src_dir"/. "$host_dest"
      ;;
    *)
      cp -R "$src" "$host_dest"
      ;;
  esac
}

subcmd="$1"
shift
case "$subcmd" in
  info)
    printf '{}\n'
    ;;
  image)
    image_cmd="$1"
    shift
    case "$image_cmd" in
      inspect)
        find "$images_root" -mindepth 1 -maxdepth 1 | grep -q .
        if [ $? -ne 0 ]; then
          exit 1
        fi
        printf '[]\n'
        ;;
      *)
        echo "unexpected sandbox CLI image command: $image_cmd $*" >&2
        exit 1
        ;;
    esac
    ;;
  load)
    if [ "$1" = "-i" ]; then
      shift 2
    fi
    image_key="default-image"
    : > "$images_root/$image_key"
    printf 'Loaded image: ctx-harness\n'
    ;;
  volume)
    volume_cmd="$1"
    shift
    case "$volume_cmd" in
      inspect)
        volume_name="$1"
        [ -d "$volumes_root/$volume_name" ] || exit 1
        printf '[]\n'
        ;;
      create)
        volume_name="$1"
        mkdir -p "$volumes_root/$volume_name"
        printf '%s\n' "$volume_name"
        ;;
      rm)
        [ "$1" = "-f" ] && shift
        volume_name="$1"
        rm -rf "$volumes_root/$volume_name"
        ;;
      *)
        echo "unexpected sandbox CLI volume command: $volume_cmd $*" >&2
        exit 1
        ;;
    esac
    ;;
  container)
    container_cmd="$1"
    shift
    case "$container_cmd" in
      exists)
        container_name="$1"
        [ -d "$(container_dir "$container_name")" ]
        ;;
      inspect)
        if [ "$1" = "--format" ] && [ "$2" = "{{.State.Running}}" ]; then
          container_name="$3"
          [ -d "$(container_dir "$container_name")" ] || exit 1
          state=$(cat "$(container_state_file "$container_name")" 2>/dev/null || printf 'false')
          printf '%s\n' "$state"
        elif [ $# -eq 1 ]; then
          container_name="$1"
          [ -d "$(container_dir "$container_name")" ] || exit 1
          printf '[{}]\n'
        else
          echo "unexpected sandbox CLI container inspect command: $*" >&2
          exit 1
        fi
        ;;
      *)
        echo "unexpected sandbox CLI container command: $container_cmd $*" >&2
        exit 1
        ;;
    esac
    ;;
  inspect)
    container_name="$1"
    mounts_file="$(container_mounts_file "$container_name")"
    [ -f "$mounts_file" ] || exit 1
    printf '['
    printf '{"Mounts":['
    first=1
    while IFS='|' read -r mount_type mount_src mount_dst mount_mode; do
      [ -n "$mount_type" ] || continue
      if [ "$first" -eq 0 ]; then
        printf ','
      fi
      if [ "$mount_type" = "volume" ]; then
        printf '{"Type":"volume","Name":"%s","Destination":"%s"}' "$mount_src" "$mount_dst"
      else
        printf '{"Type":"bind","Source":"%s","Destination":"%s"}' "$mount_src" "$mount_dst"
      fi
      first=0
    done < "$mounts_file"
    printf ']}]\n'
    ;;
  start)
    container_name="$1"
    ensure_container_dir "$container_name"
    printf 'true' > "$(container_state_file "$container_name")"
    ;;
  rm)
    [ "$1" = "-f" ] && shift
    container_name="$1"
    rm -rf "$(container_dir "$container_name")"
    ;;
  run)
    container_name=""
    mounts_file_tmp="$vm_root/run-mounts.$$"
    : > "$mounts_file_tmp"
    while [ $# -gt 0 ]; do
      case "$1" in
        -d) shift ;;
        --name) container_name="$2"; shift 2 ;;
        --hostname) shift 2 ;;
        --userns=*) shift ;;
        --user) shift 2 ;;
        --network) shift 2 ;;
        --cap-add) shift 2 ;;
        --add-host) shift 2 ;;
        --mount)
          printf '%s\n' "$2" >> "$mounts_file_tmp"
          shift 2
          ;;
        *)
          break
          ;;
      esac
    done
    [ -n "$container_name" ] || exit 1
    ensure_container_dir "$container_name"
    mounts_file="$(container_mounts_file "$container_name")"
    : > "$mounts_file"
    while IFS= read -r mount_entry; do
      [ -n "$mount_entry" ] || continue
      mount_type=$(printf '%s' "$mount_entry" | tr ',' '\n' | awk -F= '$1=="type"{print $2}')
      mount_src=$(printf '%s' "$mount_entry" | tr ',' '\n' | awk -F= '$1=="src"{print $2}')
      mount_dst=$(printf '%s' "$mount_entry" | tr ',' '\n' | awk -F= '$1=="dst"{print $2}')
      mount_mode=$(printf '%s' "$mount_entry" | tr ',' '\n' | awk 'NF==1{print $1}')
      [ -n "$mount_type" ] || continue
      printf '%s|%s|%s|%s\n' "$mount_type" "$mount_src" "$mount_dst" "$mount_mode" >> "$mounts_file"
      if [ "$mount_type" = "volume" ]; then
        mkdir -p "$volumes_root/$mount_src"
      elif [ "$mount_type" = "bind" ]; then
        mkdir -p "$mount_src"
      fi
    done < "$mounts_file_tmp"
    rm -f "$mounts_file_tmp"
    printf 'true' > "$(container_state_file "$container_name")"
    printf '%s\n' "$container_name"
    ;;
  exec)
    run_exec "$@"
    ;;
  cp)
    run_cp "$1" "$2"
    ;;
  *)
    echo "unexpected sandbox CLI command: $subcmd $*" >&2
    exit 1
    ;;
esac
