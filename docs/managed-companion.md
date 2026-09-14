---
title: "ctx Pro"
sidebarTitle: "ctx Pro"
---

# ctx Pro

Official managed ctx installations pair Apache-licensed Core with a separately
signed private companion. Core-only installation channels retain the OSS
commands. Paid routes return a typed companion-unavailable failure when the
companion is absent.

Companion installation and Pro activation are separate. Skipping the trial
with `--no-pro-trial`, PowerShell `-NoProTrial`, or
`CTX_INSTALL_NO_PRO_TRIAL=1` still installs the signed pair. CI, unattended,
and managed reinstall paths do not automatically start a trial. Use `ctx pro`
when you want to activate Pro or follow its access-recovery instructions.

Source builds and Core-only package-manager installs do not acquire a companion
when you activate Pro. To use the official paired distribution, follow the
[managed installation conversion procedure](unmanaged-installs.md#convert-an-unmanaged-install-to-a-managed-install).
