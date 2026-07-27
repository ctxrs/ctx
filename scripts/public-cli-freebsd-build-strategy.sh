#!/usr/bin/env sh
set -eu

usage() {
  printf 'usage: %s HOST_SYSTEM HOST_ARCH\n' "$(basename "$0")" >&2
  exit 2
}

if [ "$#" -ne 2 ]; then
  usage
fi

host_system="$1"
host_arch="$2"
case "${host_system}:${host_arch}" in
  FreeBSD:amd64|FreeBSD:x86_64)
    printf 'native-freebsd\n'
    ;;
  Linux:*)
    printf 'linux-cross\n'
    ;;
  *)
    printf 'freebsd-x64 construction requires native x64 FreeBSD or a Linux cross host, got %s %s\n' \
      "${host_system}" "${host_arch}" >&2
    exit 1
    ;;
esac
