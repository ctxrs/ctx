resolve_ctx_bin() {
  local configured="${CTX_REAL_HARNESS_CTX_BIN:-}"
  if [[ -n "${configured}" ]]; then
    if [[ "${configured}" != /* ]]; then
      configured="${PWD}/${configured}"
    fi
    [[ -x "${configured}" ]] || fail "provided ctx binary not found or not executable at ${configured}"
    printf '%s\n' "${configured}"
    return 0
  fi

  export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-${CTX_RUST_TOOLCHAIN:-stable}}"
  run cargo build --locked -p ctx >&2
  configured="${CARGO_TARGET_DIR:-${PWD}/target}/debug/ctx"
  [[ -x "${configured}" ]] || fail "built ctx binary not found at ${configured}"
  printf '%s\n' "${configured}"
}
