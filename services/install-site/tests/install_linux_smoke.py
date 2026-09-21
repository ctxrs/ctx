#!/usr/bin/env python3
"""Real Linux installation acceptance; fixture lifecycle mode lives in the shell driver.

CTX_INSTALL_SMOKE_SCRIPT: optional absolute proposed/read-back installer; otherwise
fetch https://ctx.rs/install. CTX_INSTALL_SMOKE_RESULT: optional absolute JSON path
(default: a retained temporary evidence directory). Expected VERSION, CORE_SHA256,
and PRO_SHA256 use the CTX_INSTALL_SMOKE_EXPECTED_ prefix. CTX_PUBLIC_CTX_REPO
supplies the existing public provider fixture, unchanged. No release overrides.

Uses the product-owned persistent daemon, including its supported detached
fallback when the isolated runtime/bus has no native manager. Use an isolated test environment; this command downloads and executes the
currently published release.
"""

import ctypes
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

URL = "https://ctx.rs/install"
SYSTEM_PATH = "/usr/bin:/bin"
CHECKS = ("core_installed", "managed", "setup", "daemon", "path", "search")
FIXTURE = Path("tests/fixtures/provider-history/codex-sessions/2026/06/23/codex-session-root.jsonl")
QUERY = "onboarding bug"
EXPECTED_TEXT = "Fix the onboarding bug and make sure local history search stays useful."


class SmokeFailure(Exception):
    pass


class Blocked(SmokeFailure):
    pass


def require(condition, message):
    if not condition:
        raise SmokeFailure(message)


def platform_id():
    system, machine = platform.system(), platform.machine().lower()
    return {
        ("Linux", "x86_64"): "linux-x64", ("Linux", "aarch64"): "linux-aarch64",
        ("Linux", "arm64"): "linux-aarch64", ("Darwin", "x86_64"): "macos-x64",
        ("Darwin", "arm64"): "macos-arm64",
    }.get((system, machine), "unsupported")


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def read_json(path):
    require(path.is_file() and not path.is_symlink(), f"missing regular JSON file: {path.name}")
    require(path.stat().st_size <= 4 * 1024 * 1024, f"oversized JSON: {path.name}")
    return json.loads(path.read_text())


def isolated_environment(root):
    # Start empty: ambient release overrides, credentials, provider roots, proxies,
    # BASH_ENV, and a user's bus must not reach the installer or any ctx process.
    home = root / "home with spaces"
    values = {
        "HOME": home, "CTX_DATA_ROOT": home / ".ctx",
        "XDG_CONFIG_HOME": home / ".config", "XDG_DATA_HOME": home / ".local/share",
        "XDG_STATE_HOME": home / ".local/state", "XDG_CACHE_HOME": home / ".cache",
        "XDG_RUNTIME_DIR": root / "runtime", "TMPDIR": root / "tmp",
        "CODEX_HOME": home / ".codex", "CLAUDE_CONFIG_DIR": home / ".claude",
        "COPILOT_HOME": home / ".copilot",
    }
    for directory in values.values():
        directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    env = {key: str(value) for key, value in values.items()}
    env.update({
        "PATH": SYSTEM_PATH, "SHELL": "/bin/bash", "LANG": "C.UTF-8",
        "LC_ALL": "C.UTF-8", "TERM": "dumb", "NO_COLOR": "1",
        "CTX_ANALYTICS_ENABLED": "false", "CTX_INSTALL_NO_PRO_TRIAL": "1",
        "CTX_UPGRADE_AUTO": "off", "CTX_SEARCH_SEMANTIC": "false",
        "DBUS_SESSION_BUS_ADDRESS": f"unix:path={root}/runtime/bus",
    })
    # Bash's normal interactive startup reads .bashrc. Start with a pristine
    # conventional profile so the installer, not this harness, must add PATH.
    (home / ".bashrc").touch()
    (home / ".profile").write_text('if [ -n "$BASH_VERSION" ]; then . "$HOME/.bashrc"; fi\n')
    return env


def cited_event(value):
    retrieval = value.get("retrieval", {})
    require(retrieval.get("requested_mode") == "lexical" and
            retrieval.get("effective_mode") == "lexical", "search did not remain lexical")
    for hit in value.get("results", []):
        if EXPECTED_TEXT not in hit.get("snippet", "") or hit.get("provider") != "codex":
            continue
        for citation in hit.get("citations", []):
            event = citation.get("ctx_event_id")
            session = citation.get("ctx_session_id")
            if (citation.get("provider") == "codex" and citation.get("target_type") == "event"
                    and isinstance(event, str) and event and isinstance(session, str) and session
                    and citation.get("item_id")):
                return event
    raise SmokeFailure("lexical search did not return the fixture text with an event citation")


class Acceptance:
    def __init__(self, inputs):
        self.inputs = inputs
        self.started = time.monotonic()
        result = inputs.get("CTX_INSTALL_SMOKE_RESULT")
        if result:
            require(Path(result).is_absolute(), "CTX_INSTALL_SMOKE_RESULT must be absolute")
            self.result_path = Path(result)
            self.result_path.parent.mkdir(parents=True, exist_ok=True)
            self.evidence = Path(tempfile.mkdtemp(prefix=self.result_path.stem + "-", dir=self.result_path.parent))
        else:
            self.evidence = Path(tempfile.mkdtemp(prefix="ctx-install-smoke-evidence-"))
            self.result_path = self.evidence / "result.json"
        self.root = Path(tempfile.mkdtemp(prefix="ctx-install-smoke-"))
        self.env = isolated_environment(self.root)
        self.log_path = self.evidence / "smoke.log"
        self.log = self.log_path.open("wb")
        self.children = []
        self.stage = "preflight"
        self.report = {
            "schema_version": 1, "status": "failed", "platform": platform_id(),
            "mode": "candidate" if inputs.get("CTX_INSTALL_SMOKE_SCRIPT") else "live",
            "installer_sha256": None, "version": None, "core_sha256": None, "pro_sha256": None,
            "checks": dict.fromkeys(CHECKS, False), "elapsed_seconds": 0,
            "installer_path": str(self.evidence / "install.sh"), "log_path": str(self.log_path),
        }

    def spawn(self, args, env=None, stdout=None):
        self.log.write(("\n[" + self.stage + "] " + " ".join(map(str, args)) + "\n").encode())
        self.log.flush()
        process = subprocess.Popen(args, env=env or self.env, cwd=self.root,
                                   stdin=subprocess.DEVNULL, stdout=stdout or self.log,
                                   stderr=self.log, start_new_session=True)
        self.children.append(process)
        return process

    def run(self, args, timeout=30, capture=None):
        with (self.evidence / capture).open("wb") if capture else open(os.devnull, "wb") as output:
            process = self.spawn(args, stdout=output if capture else self.log)
            remaining = max(0.01, 290 - (time.monotonic() - self.started))
            try:
                code = process.wait(timeout=min(timeout, remaining))
            except subprocess.TimeoutExpired as error:
                raise SmokeFailure(f"command exceeded deadline at {self.stage}") from error
        require(code == 0, f"command exited {code} at {self.stage}; see smoke.log")
        if capture:
            return self.evidence / capture
        return None

    def preflight(self):
        if self.report["platform"] not in ("linux-x64", "linux-aarch64"):
            raise Blocked("this runner cannot isolate native supervision on this platform; a native acceptance owner is required")
        for command in ("bash", "sh", "curl", "openssl", "gzip", "tar", "sha256sum"):
            if not shutil.which(command, path=SYSTEM_PATH):
                raise Blocked(f"missing Linux prerequisite: {command}")
        require(not self.inputs.get("CTX_INSTALL_LIFECYCLE_CTX_BINARY"), "fixture mode cannot produce acceptance")
        for key in ("CORE_SHA256", "PRO_SHA256"):
            expected = self.inputs.get("CTX_INSTALL_SMOKE_EXPECTED_" + key)
            require(expected is None or re.fullmatch(r"[0-9a-f]{64}", expected), f"invalid expected {key}")
        public = self.inputs.get("CTX_PUBLIC_CTX_REPO")
        require(public and Path(public).is_absolute(), "set CTX_PUBLIC_CTX_REPO to the public checkout")
        fixture = Path(public) / FIXTURE
        require(fixture.is_file() and not fixture.is_symlink() and fixture.stat().st_size < 16384,
                "existing small Codex fixture is unavailable")
        require(EXPECTED_TEXT in fixture.read_text(), "unexpected Codex fixture content")
        destination = Path(self.env["CODEX_HOME"]) / "sessions/2026/06/23" / fixture.name
        destination.parent.mkdir(parents=True)
        shutil.copyfile(fixture, destination)
        self.report["fixture_sha256"] = sha256(destination)
        # Reap and kill even detached grandchildren; no shared daemon PIDs or
        # process-name matching. This harness must be its own process.
        if ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) != 0:
            raise Blocked("Linux child subreaper is unavailable")

    def installer(self):
        self.stage = "installer_download"
        path = Path(self.report["installer_path"])
        proposed = self.inputs.get("CTX_INSTALL_SMOKE_SCRIPT")
        if proposed:
            source = Path(proposed)
            require(source.is_absolute() and source.is_file() and not source.is_symlink(),
                    "CTX_INSTALL_SMOKE_SCRIPT must be an absolute regular local file")
            shutil.copyfile(source, path)
        else:
            self.run(["curl", "--fail", "--silent", "--show-error", "--location",
                      "--proto", "=https", "--proto-redir", "=https", "--connect-timeout", "10",
                      "--max-time", "30", "--output", str(path), URL])
        require(0 < path.stat().st_size < 4 * 1024 * 1024, "empty or oversized installer")
        self.report["installer_sha256"] = sha256(path)

    def accept(self):
        self.preflight()
        self.installer()
        self.stage = "install_and_default_setup"
        # No --no-setup, --no-daemon, metadata URL, binary or PATH substitution.
        self.run(["sh", self.report["installer_path"]], timeout=200)
        self.stage = "installed_pair"
        install_root = Path(self.env["HOME"]) / ".local"
        core, pro = install_root / "bin/ctx", install_root / "libexec/ctx-pro"
        output = self.run([str(core), "--version"], capture="version.txt").read_text().strip()
        match = re.fullmatch(r"ctx (\S+)", output)
        require(match, "unexpected ctx --version output")
        self.report["version"] = match[1]
        expected = self.inputs.get("CTX_INSTALL_SMOKE_EXPECTED_VERSION")
        require(expected is None or expected == match[1], "installed version mismatch")
        unified = tuple(map(int, match[1].split("."))) >= (1, 5, 0)
        for name, path in (("core", core),) if unified else (("core", core), ("pro", pro)):
            require(path.is_file() and not path.is_symlink() and os.access(path, os.X_OK),
                    f"missing installed {name} executable")
            self.report[name + "_sha256"] = sha256(path)
            self.report["checks"][name + "_installed"] = True
            expected = self.inputs.get("CTX_INSTALL_SMOKE_EXPECTED_" + name.upper() + "_SHA256")
            require(expected is None or expected == self.report[name + "_sha256"], f"installed {name} hash mismatch")
        if unified:
            require(not pro.exists(), "1.5 must install only one binary")
            self.report["checks"]["single_binary"] = True
        marker = read_json(core.with_name("ctx.install.json"))
        require(marker.get("schema_version") == 1 and marker.get("manager") == "ctx-hosted-installer"
                and marker.get("install_path") == str(core) and marker.get("version") == match[1]
                and marker.get("sha256") == self.report["core_sha256"], "managed marker does not match installed Core")
        envelope = install_root / "share/ctx/managed-pair-envelope.json"
        if not unified:
            require(bool(read_json(envelope)), "missing signed pair envelope")
        else:
            require(not envelope.exists() and marker.get("managed_pair") is not True, "unexpected legacy pair installation")
        # Core owns signature verification. Successful official installation and
        # its upgrade status supply managed authority; do not reimplement trust.
        upgrade = read_json(self.run([str(core), "upgrade", "status", "--format=json"], capture="upgrade.json"))
        require(upgrade.get("install", {}).get("managed") is True, "upgrade status does not report a managed install")
        if not unified:
            self.report["envelope_sha256"] = sha256(envelope)
        self.report["checks"]["managed"] = True
        self.stage = "new_shell_path"
        # /etc/bash.bashrc may print a banner. Compare inside the new shell,
        # without confusing startup stdout with the resolved executable path.
        self.run(["bash", "--noprofile", "-ic",
                  'resolved="$(command -v ctx)" && printf "%s\\n" "$resolved" && test "$resolved" = "$1"',
                  "ctx-install-smoke", str(core)], capture="path.txt")
        self.report["checks"]["path"] = True
        self.stage = "daemon"
        until = time.monotonic() + 45
        while True:
            daemon = read_json(self.run([str(core), "daemon", "status", "--format=json"], capture="daemon.json"))
            if (daemon.get("daemon", {}).get("enabled") is True and
                    daemon.get("daemon", {}).get("running") is True and
                    daemon.get("daemon", {}).get("core_refresh_endpoint", {}).get("available") is True and
                    daemon.get("daemon", {}).get("jobs", {}).get("core_refresh", {}).get("published_generation")):
                break
            require(time.monotonic() < until, "default setup did not start its daemon and publish initial history")
            time.sleep(0.25)
        require(isinstance(daemon["daemon"].get("live_pid"), int) and daemon["daemon"]["live_pid"] > 0,
                "daemon status has no live process identity")
        self.report["daemon_supervisor"] = daemon["daemon"].get("supervisor", {}).get("status")
        self.report["checks"]["daemon"] = True
        # Unattended setup may return while initial refresh is still running.
        # Read-only polling waits for that work; it cannot repair skipped setup.
        self.stage = "setup_and_cited_search"
        while True:
            search = read_json(self.run([str(core), "search", QUERY, "--backend", "lexical",
                                         "--refresh", "off", "--format", "json"], capture="search.json"))
            try:
                event = cited_event(search)
                break
            except SmokeFailure:
                if time.monotonic() >= until:
                    raise
                time.sleep(0.25)
        shown = read_json(self.run([str(core), "show", "event", event, "--format", "json"], capture="citation.json"))
        require(EXPECTED_TEXT in json.dumps(shown), "search citation did not resolve to the fixture event")
        self.stage = "daemon_after_search"
        after = read_json(self.run([str(core), "daemon", "status", "--format=json"], capture="daemon-after.json"))
        require(after.get("daemon", {}).get("running") is True and
                after["daemon"].get("live_pid") == daemon["daemon"].get("live_pid"),
                "default daemon did not remain alive across cited search")
        self.report["checks"]["setup"] = True
        self.report["checks"]["search"] = True

    def cleanup(self):
        # /proc children are this subreaper's descendants, including double forks.
        # Terminate original groups too, then reap adopted descendants repeatedly.
        for process in self.children:
            if process.poll() is not None:
                continue
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        until = time.monotonic() + 6
        children_path = Path(f"/proc/self/task/{os.getpid()}/children")
        while True:
            if not children_path.is_file() and not self.children:
                break  # Unsupported platform failed before launching children.
            children = [int(pid) for pid in children_path.read_text().split()]
            if not children:
                break
            for pid in children:
                try:
                    os.kill(pid, signal.SIGKILL if time.monotonic() > until - 4 else signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    os.waitpid(pid, os.WNOHANG)
                except ChildProcessError:
                    pass
            if time.monotonic() >= until:
                raise SmokeFailure("cleanup could not quiesce owned children")
            time.sleep(0.05)
        for process in self.children:
            process.poll()
        shutil.rmtree(self.root)
        self.log.close()

    def execute(self):
        def interrupted(signum, _frame):
            raise SmokeFailure(f"deadline or signal {signum} at {self.stage}")
        old = {sig: signal.signal(sig, interrupted) for sig in (signal.SIGALRM, signal.SIGTERM, signal.SIGINT)}
        signal.setitimer(signal.ITIMER_REAL, max(0.01, 290 - (time.monotonic() - self.started)))
        try:
            self.accept()
            self.report["status"] = "passed"
        except Exception as error:
            self.report.update(status="failed", failed_stage=self.stage, error=str(error)[:2000],
                               blocked=isinstance(error, Blocked))
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)
            # Allow bounded teardown to finish after timeout/cancellation.
            for sig in (signal.SIGTERM, signal.SIGINT):
                signal.signal(sig, signal.SIG_IGN)
            try:
                self.cleanup()
                self.report["cleanup"] = True
            except (OSError, SmokeFailure) as error:
                self.report.update(status="failed", cleanup=False, cleanup_error=str(error)[:1000])
                self.report.setdefault("failed_stage", "cleanup")
            self.report["elapsed_seconds"] = round(time.monotonic() - self.started, 3)
            pending = self.evidence / "result.pending.json"
            pending.write_text(json.dumps(self.report, indent=2) + "\n")
            os.replace(pending, self.result_path)
            for sig, handler in old.items():
                signal.signal(sig, handler)
        print(f"{self.report['status']}: Linux {self.report['mode']} installation acceptance; {self.result_path}")
        return 0 if self.report["status"] == "passed" else 1


if __name__ == "__main__":
    try:
        sys.exit(Acceptance(dict(os.environ)).execute())
    except (SmokeFailure, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        sys.exit(1)
