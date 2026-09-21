# Hosted ctx installer

This Worker serves the production shell and PowerShell installers and
uninstallers, plus the retained ADE installer routes. All implementation,
public verification keys, tests and deployment configuration are in this
repository. Cloudflare credentials and release signing keys are supplied by
the operator; they are not source inputs.

```sh
npm ci --prefix services/install-site
npm test --prefix services/install-site
scripts/bazelw test //services/install-site:install_site_test --config=ci
```

The installer authenticates signed release metadata before selecting an
artifact. Versions before 1.5 retain their signed pair installation path.
Version 1.5 and later use the ordinary executable artifact and checksum, even
when the metadata includes a legacy pair projection for older updaters.
Setup, PATH, man pages, skills, optional Semantic assets and managed ownership
continue through their existing owners. Published trial switches are ignored
and hidden from help; the installer never starts or configures a trial.

Rerun the current installer to finish an interrupted installation or recover
from a cached script that attempted a retired commercial setup command. Old
commercial setup scripts are not supported installation clients for 1.5. Stock
1.4 updaters retain their signed bridge. On Unix, fixed legacy installation
files may remain until a subsequent explicit upgrade/installer reconciliation
can acquire the installation lock. Startup does not clean those files.

Uninstall removes the managed executable and verified installer-owned
integrations after native daemon teardown. Changed integrations are preserved.
For 1.5, no option or `--keep-data` (`-KeepData`) preserves Core history, the
attribution index and inert legacy data and keys. The retired `--delete-data`
(`-DeleteData`) option fails before uninstall; it formerly covered Pro-derived
data and never authorizes deleting all history. Rerun without it. Older
installed versions retain their original scoped lifecycle contract. Unmanaged,
Cargo and package-manager installations use their original removal method.

## Deployment

Prepare candidate scripts against a passed release-contract report for the
**currently published** stable v2 release:

```sh
node services/install-site/deploy.mjs prepare /path/to/new-evidence --release-evidence /path/to/current-release.json
node services/install-site/deploy.mjs check /path/to/new-evidence --native-results /path/to/native-results
node services/install-site/deploy.mjs apply /path/to/new-evidence --native-results /path/to/native-results
```

The gate checks the live signed feed against that report, executes separate
unpublished 1.5 fixtures, and requires real Linux installation and both Windows
PowerShell editions against the current feed. Candidate fixture execution
requires PowerShell; missing execution cannot qualify deployment. The Windows
report is `windows-x64.json`, produced by `tests/install_live_smoke.ps1` from
an isolated fixture packet and the exact proposed installer bytes.

The candidate source hash is independent of the released binary's source
commit. Changes to candidate source or scripts invalidate the candidate.
After deployment the gate compares served bytes, allowing only the per-request
attempt identifier to differ, and checks installation from those bytes. Keep
this readback before advancing the release pointer. A current-feed 1.4 result
is not live 1.5 evidence: after v2 exposes 1.5, run fresh/reinstall and stock
updater checks separately against that published feed. A failed Wrangler call
still triggers readback because it may already have changed served traffic.

Normal tests use authored fixtures and do not install into a user's home.
Live installation and deployment commands require an operator-selected isolated
environment and are not part of the normal test command.
