#!/usr/bin/env bash
set -euo pipefail

# Compatibility hook for shared macOS Buildkite agents.
#
# Some ctx agents call this checkout-local script from a global pre-command
# hook before repository-owned Buildkite scripts run. The public release
# verification pipeline must not load signing material or publish artifacts, so
# this hook removes inherited desktop runtime and signing pollution, then exits.

scrub_names=(
  APPLE_API_ISSUER
  APPLE_API_KEY
  APPLE_API_KEY_ID
  APPLE_API_KEY_PATH
  APPLE_CERTIFICATE
  APPLE_CERTIFICATE_PASSWORD
  APPLE_SIGNING_IDENTITY
  CSC_KEY_PASSWORD
  CSC_LINK
  CTX_DESKTOP_EMBEDDED_UPDATER_PUBKEY_B64
  TAURI_KEY_PASSWORD
  TAURI_PRIVATE_KEY
  TAURI_SIGNING_PRIVATE_KEY
  TAURI_SIGNING_PRIVATE_KEY_PASSWORD
)

for scrub_name in "${scrub_names[@]}"; do
  unset "${scrub_name}"
done

unset CTX_APPIMAGE_PATH
unset CTX_WEB_DIST
unset CTX_BUNDLE_DIR
unset CTX_BUILD_IDENTITY_PATH
unset CTX_MCP_COMMAND
