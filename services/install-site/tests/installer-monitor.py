#!/usr/bin/env python3
"""Run the existing live smoke once, retain evidence, reject partial success.

Buildkite owns scheduling, logs, notification delivery and ephemeral teardown.
No automatic retry is deliberate policy. Preserve the first failed run;
any later diagnostic must be a separate build.
"""
import hashlib
import json
import math
import os
from pathlib import Path
import pwd
import re
import signal
import shutil
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[3]
CHECKS = ("core_installed", "managed", "setup", "daemon", "path", "search")
SHA256 = re.compile(r"[0-9a-f]{64}")


def read_result(path):
    if not path.is_file() or path.is_symlink() or path.stat().st_size > 65536:
        raise ValueError("live smoke did not retain a bounded result")
    result = json.loads(path.read_text())
    if not isinstance(result, dict):
        raise ValueError("invalid live smoke result")
    return result


def verify_result(path, exit_code):
    result = read_result(path)
    if exit_code != 0 or result.get("status") != "passed":
        raise ValueError(f"live installer smoke failed (exit {exit_code}); inspect first-attempt logs")
    if type(result.get("schema_version")) is not int or result["schema_version"] != 1 or result.get("mode") != "live":
        raise ValueError("monitor requires a live result using default public discovery")
    elapsed = result.get("elapsed_seconds")
    if (type(elapsed) not in (int, float) or not math.isfinite(elapsed) or not 0 <= elapsed <= 300
            or result.get("cleanup") is not True):
        raise ValueError("live smoke did not complete bounded cleanup")
    checks = result.get("checks")
    if not isinstance(checks, dict) or any(checks.get(key) is not True for key in CHECKS):
        raise ValueError("live smoke omitted or failed required installation checks")
    if not isinstance(result.get("version"), str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", result["version"]):
        raise ValueError("live smoke omitted installed version")
    unified = tuple(map(int, result["version"].split("."))) >= (1, 5, 0)
    if unified:
        if checks.get("single_binary") is not True or "pro_sha256" not in result or result["pro_sha256"] is not None or checks.get("pro_installed") is True:
            raise ValueError("1.5 live smoke requires a single binary without a companion receipt")
    elif checks.get("pro_installed") is not True:
        raise ValueError("legacy live smoke requires the installed companion")
    for key in (("installer_sha256", "core_sha256") if unified else ("installer_sha256", "core_sha256", "pro_sha256")):
        if not isinstance(result.get(key), str) or not SHA256.fullmatch(result[key]):
            raise ValueError(f"live smoke omitted {key}")
    return result


def clean_environment(home, result, user):
    # Do not pass CI credentials, release pins, fake transport, candidate mode,
    # provider history, or inherited ctx behavior overrides to installed code.
    return {
        "PATH": "/usr/local/bin:/usr/bin:/bin", "HOME": str(home),
        "USER": user, "LOGNAME": user, "LANG": "C.UTF-8", "CI": "1",
        "TMPDIR": str(home / "tmp"),
        "XDG_CONFIG_HOME": str(home / "config"),
        "XDG_DATA_HOME": str(home / "data"),
        "XDG_STATE_HOME": str(home / "state"),
        "XDG_CACHE_HOME": str(home / "cache"),
        "XDG_RUNTIME_DIR": str(home / "runtime"),
        "CTX_DATA_ROOT": str(home / "ctx-data"),
        "CODEX_HOME": str(home / "codex"),
        "CLAUDE_CONFIG_DIR": str(home / "claude"),
        "CTX_INSTALL_SMOKE_RESULT": str(result),
        "CTX_PUBLIC_CTX_REPO": str(home / "public-fixture"),
        "CTX_ANALYTICS_ENABLED": "false", "CTX_INSTALL_NO_PRO_TRIAL": "1",
    }


def run_smoke(command, env, log, timeout, identity=None):
    kwargs = {}
    if identity and os.geteuid() == 0:
        kwargs = {"user": identity.pw_uid, "group": identity.pw_gid, "extra_groups": []}
    with log.open("xb") as stream:
        child = subprocess.Popen(command, env=env, stdin=subprocess.DEVNULL,
                                 stdout=stream, stderr=subprocess.STDOUT,
                                 start_new_session=True, **kwargs)
        try:
            return child.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGTERM)
            try:
                child.wait(timeout=8)  # Smoke owns up to six seconds of descendant cleanup.
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait(timeout=2)
            finally:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            return 124


def acquire_fixture(home, env):
    # The dedicated public-source pipeline/schedule supplies its exact checkout
    # commit. This pins fixture content only; it does not authorize a release.
    commit = os.environ.get("BUILDKITE_COMMIT", "")
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("one exact public source commit is required")
    relative = "tests/fixtures/provider-history/codex-sessions/2026/06/23/codex-session-root.jsonl"
    fixture = home / "public-fixture" / relative
    fixture.parent.mkdir(parents=True)
    subprocess.run(["curl", "--fail", "--silent", "--show-error", "--proto", "=https",
                    "--connect-timeout", "5", "--max-time", "15", "--max-filesize", "16384",
                    "--output", str(fixture), f"https://raw.githubusercontent.com/ctxrs/ctx/{commit}/{relative}"],
                   env=env, check=True, timeout=17)


def retain_smoke_evidence(result_path, evidence):
    result = read_result(result_path)
    # Retain the diagnostic independently before inspecting downloaded bytes:
    # a failed curl may leave a partial installer with no recorded digest.
    for key, name, limit in (("log_path", "smoke.log", 16 * 1024 * 1024),
                             ("installer_path", "served-installer.sh", 4 * 1024 * 1024)):
        source = Path(result.get(key, ""))
        if (not source.is_absolute() or source.is_symlink() or not source.is_file()
                or not source.resolve().is_relative_to(evidence.resolve())
                or source.resolve().is_relative_to((evidence / "home").resolve())
                or source.stat().st_size > limit):
            if result.get("status") == "passed":
                raise ValueError(f"missing or invalid retained {key}")
            continue
        if key == "installer_path" and hashlib.sha256(source.read_bytes()).hexdigest() != result.get("installer_sha256"):
            if result.get("status") == "passed":
                raise ValueError("retained installer does not match smoke report hash")
            continue  # Preserve the original failure, not a partial-download hash error.
        shutil.copyfile(source, evidence / name)


def main():
    started = time.monotonic()
    if sys.platform != "linux" or os.environ.get("BUILDKITE_PIPELINE_SLUG") != "ctx-installer-monitor":
        raise ValueError("run only in the dedicated Linux hosted monitor pipeline")
    if os.environ.get("BUILDKITE_AGENT_META_DATA_QUEUE") != "default":
        raise ValueError("monitor requires the verified Small hosted default queue")
    job = os.environ.get("BUILDKITE_JOB_ID", "")
    if not re.fullmatch(r"[0-9a-f-]{36}", job):
        raise ValueError("missing Buildkite job identity")
    evidence = ROOT / ".artifacts" / "installer-monitor" / job
    evidence.mkdir(parents=True, exist_ok=False)
    # Hosted images may launch commands as root. Use an existing unprivileged
    # account, never setup/daemon as root. No package/account provisioning.
    identity = pwd.getpwnam("nobody") if os.geteuid() == 0 else pwd.getpwuid(os.getuid())
    home = evidence / "home"
    home.mkdir(mode=0o700)
    for name in ("tmp", "runtime"):
        (home / name).mkdir(mode=0o700)
    if os.geteuid() == 0:
        for path in (evidence, home, home / "tmp", home / "runtime"):
            os.chown(path, identity.pw_uid, identity.pw_gid)
    result_path = evidence / "first-result.json"
    log = evidence / "first-attempt.log"
    summary = {"status": "failed", "attempts": 1, "uid": identity.pw_uid,
               "build_url": os.environ.get("BUILDKITE_BUILD_URL"), "job_id": job}
    code = 1
    try:
        env = clean_environment(home, result_path, identity.pw_name)
        acquire_fixture(home, env)
        code = run_smoke(["bash", str(ROOT / "services/install-site/tests/install_live_smoke.sh")],
                         env, log,
                         max(1, 260 - (time.monotonic() - started)), identity)
        retain_smoke_evidence(result_path, evidence)
        result = verify_result(result_path, code)
        summary.update(status="passed", version=result["version"])
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        summary["error"] = str(error)
        code = code or 1
    summary.update(exit_code=code, elapsed_seconds=round(time.monotonic() - started, 2))
    (evidence / "monitor.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary), flush=True)
    # Logs remain in Buildkite even when artifact transport fails. Only this
    # job's evidence is uploaded, never isolated HOME, binaries or history.
    for path in (log, evidence / "smoke.log"):
        if path.exists():
            with path.open("rb") as stream:
                stream.seek(max(0, path.stat().st_size - 65536))
                print(stream.read(65536).decode(errors="replace"), flush=True)
    try:
        relative = evidence.relative_to(ROOT)
        subprocess.run(["buildkite-agent", "artifact", "upload",
                        f"{relative}/*.json;{relative}/*.log;{relative}/*.sh"],
                       cwd=ROOT, check=True, timeout=max(1, 290 - (time.monotonic() - started)))
    except (OSError, subprocess.SubprocessError):
        print("error: failed to retain monitor artifacts", file=sys.stderr)
        return 1
    return 0 if summary["status"] == "passed" else (code or 1)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, OSError, KeyError) as error:
        sys.exit(f"installer monitor: {error}")
