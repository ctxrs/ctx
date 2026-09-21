#!/usr/bin/env python3
"""Read-only activation planner/readback for the dedicated installer monitor.

No API write method is implemented. `plan --commit REVIEWED_SHA` emits concrete
REST requests for a parent/operator to review and execute once. Repeating plan
reads current state and emits no duplicate pipeline or schedule creation.

Activation: create the planned pipeline, run the planned one-off probe, check
its ordinary-user setup/daemon/search evidence and <=5 minute duration, verify
existing failure/recovery subscriptions cover this pipeline, then apply the
planned schedule request with --enabled. Run readback afterward and whenever
checking monitor health. A missing/stale/skipped run is not healthy.

Buildkite bounds running jobs, not total queue delay. Job expiration is scanned
hourly; neither this tool nor a five-minute step timeout can promise a five-
minute notification deadline when hosted capacity or Buildkite is unavailable.
See https://buildkite.com/docs/pipelines/configure/build-timeouts
"""
import argparse
import base64
from datetime import datetime, timedelta, timezone
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import sys
import subprocess
import tarfile
import urllib.error
import urllib.request


ROOT = Path(__file__).resolve().parents[3]
ORG = "luca-king"
SLUG = "ctx-installer-monitor"
CLUSTER = "387604ea-e17d-4a1b-8f01-9c76840de548"
QUEUE = "dee34e18-109b-49b3-8eb8-50d10a347833"
BASE = f"https://api.buildkite.com/v2/organizations/{ORG}"
PIPELINE = f"/pipelines/{SLUG}"
LABEL = "Public Linux installer every six hours"
EVENT_TRIGGERS = ("build_branches", "build_tags", "build_pull_requests", "build_check_run_completed",
                  "build_create_event", "build_deployment_status_created", "build_issue_comment_created",
                  "build_issues", "build_merge_group_checks_requested", "build_pull_request_review_comment_created",
                  "build_release_created", "build_release_published", "build_release_released")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None  # Never forward the API credential elsewhere.


class API:
    def __init__(self):
        self.token = os.environ.get("BUILDKITE_API_ACCESS_TOKEN")
        if not self.token:
            raise ValueError("BUILDKITE_API_ACCESS_TOKEN is required (read_pipelines, read_builds, read_clusters)")

    def get(self, path):
        request = urllib.request.Request(BASE + path, headers={"Authorization": "Bearer " + self.token})
        try:
            with urllib.request.build_opener(NoRedirect()).open(request, timeout=15) as response:
                data = response.read(2 * 1024 * 1024 + 1)
                if len(data) > 2 * 1024 * 1024:
                    raise ValueError("Buildkite readback exceeded bounded response size")
                return json.loads(data)
        except urllib.error.HTTPError as error:
            if error.code == 404:
                return None
            raise ValueError(f"Buildkite readback failed: HTTP {error.code}") from None


def pipeline_body():
    return {
        "name": "ctx installer monitor", "slug": SLUG, "cluster_id": CLUSTER,
        "repository": "git@github.com:ctxrs/ctx.git", "visibility": "private",
        "default_branch": "main", "configuration": (ROOT / "services/install-site/tests/installer-monitor.yml").read_text(),
        "default_command_step_timeout": 5, "maximum_command_step_timeout": 5,
        "skip_queued_branch_builds": False, "cancel_running_branch_builds": False,
        "provider_settings": dict.fromkeys((*EVENT_TRIGGERS, "publish_commit_status"), False),
    }


def schedule_body(commit, enabled):
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("commit must be a reviewed full lowercase public Git SHA")
    return {"label": LABEL, "cronline": "17 */6 * * * UTC", "branch": "main",
            "commit": commit, "message": LABEL, "env": {}, "enabled": enabled}


def check_queue(queue):
    shape = (queue.get("hosted_agents") or {}).get("instance_shape", {})
    if (queue.get("key") != "default" or queue.get("dispatch_paused") is not False
            or shape.get("name") != "LINUX_AMD64_2X4"
            or shape.get("cpu") != 2 or shape.get("memory") != 4):
        raise ValueError("default queue is not active Small hosted Linux (2 vCPU / 4 GB)")


def check_pipeline(current, desired):
    for key in ("slug", "repository", "cluster_id", "configuration", "visibility", "default_branch",
                "default_command_step_timeout", "maximum_command_step_timeout",
                "skip_queued_branch_builds", "cancel_running_branch_builds"):
        if current.get(key) != desired[key]:
            raise ValueError(f"dedicated monitor pipeline differs at {key}; refusing release-pipeline reuse/drift")
    settings = current.get("provider", {}).get("settings", {})
    for key in desired["provider_settings"]:
        if settings.get(key) is not False:
            raise ValueError(f"monitor must disable provider setting {key}")


def assert_public_source(commit, paths):
    """Bind the source-only probe to the selected public checkout, without builds."""
    def git(*args):
        return subprocess.check_output(["git", "-C", str(ROOT), *args], text=True, timeout=10).strip()
    if git("rev-parse", "--verify", "HEAD^{commit}") != commit or git(
            "status", "--porcelain=v1", "--untracked-files=all", "--", *paths):
        raise ValueError("probe requires clean public inputs at the exact public commit")


def probe_plan(commit):
    """Package only reviewed script inputs into a private, checkout-free step.

    The parent submits this to the same dedicated pipeline, never an existing
    release/repair pipeline. Replace with pipeline_body after source lands.
    No arbitrary archive input, source refs, upload service, or product build.
    """
    schedule_body(commit, False)  # Validate the source identity for the API build.
    paths = ["services/install-site/tests/installer-monitor.py",
             "services/install-site/tests/install_live_smoke.sh", "services/install-site/tests/install_linux_smoke.py"]
    assert_public_source(commit, paths)
    stream = io.BytesIO()
    hashes = {}
    with tarfile.open(fileobj=stream, mode="w") as archive:
        for relative in paths:
            path = ROOT / relative
            if path.is_symlink() or not path.is_file() or path.stat().st_size > 256 * 1024:
                raise ValueError("invalid probe script input: " + relative)
            content = path.read_bytes()
            hashes[relative] = hashlib.sha256(content).hexdigest()
            entry = tarfile.TarInfo(relative)
            entry.size, entry.mode = len(content), 0o644
            archive.addfile(entry, io.BytesIO(content))
    assert_public_source(commit, paths)
    payload = gzip.compress(stream.getvalue(), mtime=0)
    if len(payload) > 128 * 1024:
        raise ValueError("probe payload exceeded bounded script-only size")
    encoded = base64.b64encode(payload).decode()
    command = ("set -euo pipefail\numask 022\nmkdir monitor-source\n"
               f"printf '%s' '{encoded}' | base64 --decode | tar xz -C monitor-source\n"
               "cd monitor-source\nexec python3 -B services/install-site/tests/installer-monitor.py")
    body = pipeline_body()
    body["configuration"] = json.dumps({"steps": [{"label": "Linux installer hosted capability probe",
        "key": "installer-monitor-linux", "agents": {"queue": "default"}, "checkout": {"skip": True},
        "timeout_in_minutes": 5, "retry": {"automatic": False, "manual": {"allowed": False}},
        "command": command}]})
    return {"status": "plan-only", "source_file_sha256": hashes,
            "payload_sha256": hashlib.sha256(payload).hexdigest(),
            "create_pipeline": planned_request("POST", "/pipelines", body),
            "probe": planned_request("POST", PIPELINE + "/builds",
                {"commit": commit, "branch": "main", "message": "Unmerged installer monitor capability probe"}),
            "conditions": ["Confirm dedicated pipeline is absent before POST; never overwrite another pipeline.",
                           "Check organization-wide subscriptions before running: notify omission does not suppress them.",
                           "Private Buildkite receives these reviewed source bytes; no Git refs are published.",
                           "No schedule is created. Replace probe configuration after integration before enabling one."]}


def planned_request(method, path, body):
    return {"method": method, "url": BASE + path, "body": body}


def inspect(api, commit, enabled, readback=False, now=None):
    desired = pipeline_body()
    schedule = schedule_body(commit, enabled)
    check_queue(api.get(f"/clusters/{CLUSTER}/queues/{QUEUE}") or {})
    current = api.get(PIPELINE)
    requests = []
    if current is None:
        if readback:
            raise ValueError("monitor pipeline is missing")
        requests.append(planned_request("POST", "/pipelines", desired))
        schedules = []
    else:
        check_pipeline(current, desired)
        schedules = api.get(PIPELINE + "/schedules?per_page=100")
        if not isinstance(schedules, list) or len(schedules) >= 100:
            raise ValueError("schedule inventory is missing or exceeds bounded page")
    if len(schedules) > 1 or (schedules and schedules[0].get("label") != LABEL):
        raise ValueError("unexpected/duplicate schedules; reconcile manually before activation")
    actual = schedules[0] if schedules else None
    if not actual:
        if readback:
            raise ValueError("monitor schedule is missing")
        requests.append(planned_request("POST", PIPELINE + "/schedules", schedule))
    elif any(actual.get(k) != v for k, v in schedule.items()):
        if readback:
            raise ValueError("monitor schedule differs from requested state")
        requests.append(planned_request("PUT", PIPELINE + "/schedules/" + actual["id"], schedule))
    if readback:
        if not enabled:
            return {"status": "disabled", "healthy": False}
        if actual.get("failed_at") or actual.get("failed_message") or not actual.get("next_build_at"):
            raise ValueError("scheduler reports failure or no next build")
        builds = api.get(PIPELINE + "/builds?per_page=20&include_retried_jobs=true&exclude_pipeline=true")
        if not isinstance(builds, list) or not builds:
            raise ValueError("no scheduled monitor run exists")
        build = next((build for build in builds if build.get("source") == "schedule"), None)
        if build is None:
            raise ValueError("no scheduled run in the latest 20 builds")
        jobs = build.get("jobs", [])
        if (build.get("state") != "passed" or build.get("commit") != commit
                or build.get("source") != "schedule" or build.get("branch") != "main"
                or len(jobs) != 1 or jobs[0].get("step_key") != "installer-monitor-linux"
                or jobs[0].get("state") != "passed" or jobs[0].get("exit_status") != 0
                or jobs[0].get("retries_count") not in (None, 0) or jobs[0].get("retry_source")):
            raise ValueError("latest scheduled monitor run is not a complete first-attempt success")
        created = datetime.fromisoformat(build["created_at"].replace("Z", "+00:00"))
        finished = datetime.fromisoformat(build["finished_at"].replace("Z", "+00:00"))
        age = (now or datetime.now(timezone.utc)) - created
        if not timedelta(0) <= age <= timedelta(hours=6, minutes=5):
            raise ValueError("latest scheduled monitor run is stale")
        if not timedelta(0) <= finished - created <= timedelta(minutes=5):
            raise ValueError("monitor missed the five-minute build deadline")
        return {"status": "passed", "healthy": True, "last_success_at": build["finished_at"],
                "build_url": build.get("web_url"), "next_build_at": actual["next_build_at"]}
    return {
        "status": "plan-only", "requests": requests,
        "probe": planned_request("POST", PIPELINE + "/builds",
                                 {"commit": commit, "branch": "main", "message": "Bounded installer monitor qualification"}),
        "activation_checks": ["Reviewed source must be available to hosted checkout.",
                              "Verify one-off hosted non-root setup/daemon/search proof within five minutes.",
                              "Verify existing notification services cover failure and recovery on this pipeline.",
                              "Enable only after review; readback must show a fresh scheduled run."],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("plan", "readback", "probe-plan"))
    parser.add_argument("--commit", required=True)
    parser.add_argument("--enabled", action="store_true", help="plan/check enabled state; never sends API writes")
    args = parser.parse_args()
    result = probe_plan(args.commit) if args.mode == "probe-plan" else inspect(
        API(), args.commit, args.enabled, args.mode == "readback")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError, TypeError) as error:
        sys.exit(f"installer monitor: {error}")
