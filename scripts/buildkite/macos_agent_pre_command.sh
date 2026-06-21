#!/usr/bin/env bash
set -euo pipefail

# Compatibility hook for shared macOS Buildkite agents.
#
# Some ctx agents call this checkout-local script from a global pre-command
# hook before repository-owned Buildkite scripts run. The public release
# verification pipeline must not load signing material or publish artifacts, so
# this hook only removes inherited desktop runtime pollution and exits.

unset CTX_APPIMAGE_PATH
unset CTX_WEB_DIST
unset CTX_BUNDLE_DIR
unset CTX_BUILD_IDENTITY_PATH
unset CTX_MCP_COMMAND

