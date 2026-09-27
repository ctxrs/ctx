#!/usr/bin/env python3
"""Opt-in Unix smoke of the published stable updater; no builds or feed writes.

From the repository root (Python 3, curl and OpenSSL required):
  python3 scripts/release/upgrade-walk.py 1.6.3 1.6.4 2.0.4 --timeout 240
Supply the exact intended sequence, including 1.6.3, for the feed under test.
The example is an expectation, not a claim that the live feed supports it.
The installer runs ONCE with signed versioned 1.6.3 metadata. Thereafter only
the installed `ctx upgrade --format json` may replace the executable. Its
compiled production URL decides each hop; no fake feed, channel override,
version forcing, reinstall, due-marker edits, or automatic-cadence claim.

Every subprocess has a timeout (seconds, default 240, maximum 900). A private
temporary directory retains result.json, command logs, signed metadata and the
isolated installation, including on failure. --output-dir selects a NEW directory.
Failed updater calls check the previous binary and searchable history separately,
recording retention failures without masking the original updater error.
No ambient environment is inherited. Analytics, automatic upgrades, persistent
daemons and semantic search are disabled; initial import may own a finite worker.
Only installer/metadata inputs are downloaded by this script: the installer and
real updater own artifact downloads and verification, with no duplicate binary
downloads or copies between hops. Shared-host users must use their build/resource
governor. Unit tests are offline; this manual smoke reads the live release feed.
"""

import argparse
import base64
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


ROOT = Path(__file__).resolve().parents[2]
INSTALL_URL = "https://ctx.rs/install"
METADATA_BASE = "https://cli.ctx.rs/functions/v1/releases/stable"
FIXTURE = ROOT / "tests/fixtures/custom-history-jsonl/basic.jsonl"
KEY_SOURCE = ROOT / "services/install-site/src/cli-install-script.js"
QUERY = "parser test"
TEXT = "Add a parser test."


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def platform_key():
    key = {
        ("Linux", "x86_64"): "linux_x64",
        ("Linux", "aarch64"): "linux_aarch64",
        ("Darwin", "x86_64"): "macos_x64",
        ("Darwin", "arm64"): "macos_arm64",
    }.get((platform.system(), platform.machine()))
    require(key, "upgrade walk supports native Linux and macOS release targets only")
    return key


def isolated_environment(root):
    home = root / "home"
    paths = {
        "HOME": home, "CTX_DATA_ROOT": root / "data",
        "XDG_CONFIG_HOME": home / ".config", "XDG_DATA_HOME": home / ".local/share",
        "XDG_STATE_HOME": home / ".local/state", "XDG_CACHE_HOME": home / ".cache",
        "XDG_RUNTIME_DIR": root / "runtime", "TMPDIR": root / "tmp",
        "CODEX_HOME": home / ".codex", "CLAUDE_CONFIG_DIR": home / ".claude",
        "COPILOT_HOME": home / ".copilot", "HF_HOME": home / ".huggingface",
    }
    for directory in paths.values():
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    env = {name: str(path) for name, path in paths.items()}
    env.update({
        "PATH": "/usr/bin:/bin", "SHELL": "/bin/sh", "LANG": "C.UTF-8",
        "LC_ALL": "C.UTF-8", "NO_COLOR": "1", "TERM": "dumb",
        "CTX_ANALYTICS_ENABLED": "false", "CTX_UPGRADE_AUTO": "off",
        "CTX_DAEMON_ENABLED": "false", "CTX_DAEMON_AUTOSTART_OFF": "1",
        "CTX_SEARCH_SEMANTIC": "false", "HF_HUB_OFFLINE": "1",
        "DBUS_SESSION_BUS_ADDRESS": f"unix:path={root}/runtime/bus",
    })
    return env


def parse_metadata(text):
    values = {}
    for line in text.splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        key, separator, value = line.partition("=")
        require(separator and re.fullmatch(r"[A-Za-z0-9_]+", key)
                and value == value.strip() and key not in values,
                "invalid or duplicate signed metadata field")
        values[key] = value
    return values


class Walk:
    def __init__(self, root, versions, timeout):
        self.root, self.versions, self.timeout = root, versions, timeout
        self.env = isolated_environment(root)
        # 1.6.3 does not forward daemon-enabled environment overrides to its
        # finite import worker. Persist the same configuration for both processes.
        (Path(self.env["CTX_DATA_ROOT"]) / "config.toml").write_text(
            '[analytics]\nenabled = false\n[upgrade]\nauto = "off"\n'
            '[search]\nsemantic = false\n'
            '[daemon]\nenabled = false\nmode = "source-refresh-only"\n'
        )
        self.binary = Path(self.env["HOME"]) / ".local/bin/ctx"
        self.key = root / "metadata-public.pem"
        self.platform = platform_key()
        self.metadata = {}
        self.report = {
            "status": "failed", "kind": "manual-released-upgrade-walk",
            "expected_versions": versions, "observed_versions": [],
            "platform": self.platform, "automatic_cadence_tested": False,
            "commands": [], "snapshots": [],
        }

    def run(self, label, args, env=None):
        self.report["stage"] = label
        record = {"label": label, "argv": [str(arg) for arg in args]}
        self.report["commands"].append(record)
        stdout = self.root / f"{len(self.report['commands']):02d}-{label}.stdout"
        stderr = stdout.with_suffix(".stderr")
        record.update(stdout=stdout.name, stderr=stderr.name)
        print(label, flush=True)
        with stdout.open("wb") as out, stderr.open("wb") as err:
            process = subprocess.Popen(
                args, cwd=self.root, env=self.env if env is None else env,
                stdin=subprocess.DEVNULL, stdout=out, stderr=err, start_new_session=True,
            )
            try:
                record["returncode"] = process.wait(timeout=self.timeout)
            except subprocess.TimeoutExpired:
                record["timed_out"] = True
                raise
            finally:
                # Reap this command group; import workers have a separate group
                # and are stopped through ctx's isolated lifecycle below.
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    pass
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait(timeout=3)
        require(record["returncode"] == 0,
                f"{label} exited {record['returncode']}; see {stderr.name}")
        return stdout.read_text()

    def download(self, label, url, path):
        self.run(label, ["curl", "--disable", "--fail", "--silent", "--show-error",
                         "--location", "--proto", "=https", "--proto-redir", "=https",
                         "--max-time", str(self.timeout), "--output", str(path), url])

    def release(self, version):
        if version in self.metadata:
            return self.metadata[version]
        url = f"{METADATA_BASE}/{version}/ctx-release-metadata.env"
        path = self.root / f"{version}.env"
        signature = path.with_suffix(".env.sig")
        self.download(f"metadata-{version}", url, path)
        self.download(f"signature-{version}", url + ".sig", signature)
        decoded = path.with_suffix(".sig.bin")
        decoded.write_bytes(base64.b64decode(signature.read_bytes().strip(), validate=True))
        self.run(f"verify-{version}", ["openssl", "dgst", "-sha256", "-verify",
                                       str(self.key), "-signature", str(decoded), str(path)])
        values = parse_metadata(path.read_text())
        require(values.get("CTX_RELEASE_VERSION") == version, "versioned metadata version mismatch")
        require(values.get("CTX_RELEASE_SCHEMA_VERSION") == "1", "unsupported metadata schema")
        require(values.get("CTX_RELEASE_CHANNEL", "stable") == "stable", "metadata channel mismatch")
        checksum = values.get(f"CTX_RELEASE_SHA256_{self.platform}", "").lower()
        require(re.fullmatch(r"[0-9a-f]{64}", checksum) and checksum != "0" * 64,
                "missing published artifact checksum for this platform")
        result = {"url": url, "sha256": checksum, "metadata_sha256": sha256(path)}
        self.metadata[version] = result
        return result

    def snapshot(self, expected, label):
        actual = self.run(label + "-version", [str(self.binary), "--version"]).strip()
        self.report["observed_versions"].append(actual)
        require(actual == f"ctx {expected}", f"expected ctx {expected}, got {actual}; upgrade sequence differs")
        release = self.release(expected)
        digest = sha256(self.binary)
        require(digest == release["sha256"], f"installed ctx {expected} checksum differs from signed metadata")
        self.report["snapshots"].append({"version": expected, **release})
        return digest

    def failed_hop_retention(self, version, digest):
        # Diagnostics must not replace the original updater failure, even when
        # retention is broken too. Try history independently of binary identity.
        stage = self.report["stage"]
        retention = {"status": "failed", "expected_version": version}
        self.report["failed_hop_retention"] = retention
        try:
            retention["sha256"] = sha256(self.binary)
            require(retention["sha256"] == digest, "failed upgrade changed executable bytes")
            self.snapshot(version, "retained")
            retention["binary"] = "passed"
        except Exception as error:
            retention["binary_error"] = str(error)
        try:
            self.search("retained-search")
            retention["history"] = "passed"
        except Exception as error:
            retention["history_error"] = str(error)
        if retention.get("binary") == retention.get("history") == "passed":
            retention["status"] = "passed"
        self.report["stage"] = stage

    def search(self, label):
        output = json.loads(self.run(label, [str(self.binary), "search", QUERY,
                            "--backend", "lexical", "--refresh", "off", "--format", "json"]))
        retrieval = output.get("retrieval", {})
        require(retrieval.get("requested_mode") == "lexical"
                and retrieval.get("effective_mode") == "lexical", "search did not remain lexical")
        require(any(TEXT in hit.get("snippet", "") for hit in output.get("results", [])),
                "imported synthetic history is no longer searchable")

    def import_history(self, fixture):
        try:
            self.run("import", [str(self.binary), "import", "--input-format", "ctx-history-jsonl-v2",
                                "--path", str(fixture), "--format", "json", "--progress", "none"],
                     env={**self.env, "CTX_DAEMON_AUTOSTART_OFF": "0"})
        finally:
            failed = sys.exc_info()[0] is not None
            stage = self.report.get("stage")
            try:
                # The custom data root cannot own the user's native supervisor.
                # ctx checks its worker identity and stops it even after an import timeout.
                result = json.loads(self.run("stop-import-worker", [str(self.binary), "daemon",
                                          "disable", "--format", "json"]))
                require(result.get("running") is False, "isolated import worker is still running")
            except Exception as error:
                self.report["worker_cleanup_error"] = str(error)
                if not failed:
                    raise
            finally:
                if failed:
                    self.report["stage"] = stage

    def execute(self):
        match = re.search(r"DEFAULT_METADATA_PUBLIC_KEY_PEM\s*=\s*`([\s\S]*?)`;", KEY_SOURCE.read_text())
        require(match, "could not locate the installer's metadata public key")
        self.key.write_text(match[1].strip() + "\n")
        initial = self.release(self.versions[0])
        installer = self.root / "install.sh"
        self.download("download-installer", INSTALL_URL, installer)
        self.report["installer_sha256"] = sha256(installer)
        # Scope versioned metadata to the bootstrap shell only. Released ctx
        # compiles out qualification-only feed overrides; never pass them to it.
        self.run("install-1.6.3", ["sh", str(installer), "--no-setup", "--no-skill",
                                  "--no-man", "--no-modify-path"],
                 env={**self.env, "CTX_RELEASE_METADATA_URL": initial["url"]})
        self.snapshot(self.versions[0], "initial")
        fixture = self.root / "history.jsonl"
        shutil.copyfile(FIXTURE, fixture)
        # Manual indexing owns a finite worker. --no-daemon would suppress that
        # worker in 1.6.3 and prevent the initial import from publishing history.
        # Lift the spawn veto only here; persistent indexing remains disabled.
        self.import_history(fixture)
        self.search("initial-search")
        for number, version in enumerate(self.versions[1:], 1):
            label = f"hop-{number}"
            # Apply before checking the expected next version: a real download
            # limit failure must not be hidden behind a harness feed preflight.
            before = sha256(self.binary)
            try:
                outcome = json.loads(self.run(label, [str(self.binary), "upgrade", "--format", "json"]))
            except Exception:
                self.failed_hop_retention(self.versions[number - 1], before)
                raise
            self.snapshot(version, label)
            require(outcome.get("ok") is True and outcome.get("applied") is True,
                    f"{label} did not report an applied upgrade")
            self.search(label + "-search")
        before = sha256(self.binary)
        outcome = json.loads(self.run("terminal", [str(self.binary), "upgrade", "--format", "json"]))
        require(outcome.get("ok") is True and outcome.get("status") == "up_to_date"
                and outcome.get("applied") is False and outcome.get("update_available") is False,
                "terminal upgrade did not report up_to_date without applying")
        require(self.snapshot(self.versions[-1], "terminal") == before,
                "terminal upgrade changed the executable")
        self.search("terminal-search")
        self.report["status"] = "passed"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("versions", nargs="+", help="exact ordered stable versions, starting with 1.6.3")
    parser.add_argument("--timeout", type=int, default=240, help="per-command timeout in seconds (1..900)")
    parser.add_argument("--output-dir", type=Path, help="new private evidence/install directory (retained)")
    args = parser.parse_args(argv)
    if (len(args.versions) < 2 or args.versions[0] != "1.6.3"
            or any(not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", v)
                   for v in args.versions)
            or len(set(args.versions)) != len(args.versions)):
        parser.error("supply distinct stable versions in expected order, beginning with 1.6.3")
    if not 1 <= args.timeout <= 900:
        parser.error("--timeout must be between 1 and 900 seconds")
    os.umask(0o077)
    if args.output_dir:
        args.output_dir.mkdir(mode=0o700)
        root = args.output_dir.resolve()
    else:
        root = Path(tempfile.mkdtemp(prefix="ctx-upgrade-walk-")).resolve()
    walk = Walk(root, args.versions, args.timeout)
    print(f"Evidence and isolated installation: {root}", flush=True)
    try:
        walk.execute()
    except (Exception, KeyboardInterrupt) as error:
        walk.report["error"] = str(error) or type(error).__name__
        print(f"Upgrade walk failed: {walk.report['error']}", file=sys.stderr)
    finally:
        (root / "result.json").write_text(json.dumps(walk.report, indent=2) + "\n")
    return 0 if walk.report["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
