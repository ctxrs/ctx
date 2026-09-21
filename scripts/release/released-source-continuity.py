#!/usr/bin/env python3
"""Reject unaccounted changes dropped from published stable release branches.

This is source accounting, not behavioral or platform qualification. A patch
that was refactored or deliberately retired needs a reviewed source disposition.
No checkout, real Git index, release ref, or remote object is changed.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import tomllib

POLICY = Path(__file__).with_name("released-source-continuity.json")
COMMIT = re.compile(r"[0-9a-f]{40}\Z")
TAG = re.compile(r"refs/tags/v(\d+)\.(\d+)\.(\d+)(\^\{\})?\Z")


def git(repo, *args, index=None, data=None, check=True):
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith("GIT_")}
    environment.update(GIT_TERMINAL_PROMPT="0", GIT_CONFIG_NOSYSTEM="1",
                       GIT_CONFIG_GLOBAL=os.devnull)
    if index is not None:
        environment["GIT_INDEX_FILE"] = str(index)
    result = subprocess.run(["git", "-C", str(repo), *args], input=data,
                            capture_output=True, env=environment, timeout=60)
    if check and result.returncode:
        raise ValueError(f"release continuity Git check failed: {args[0]}; fetch published tags and retry")
    return result


def stable_tips(repo, minimum):
    lines = git(repo, "ls-remote", "--tags", "https://github.com/ctxrs/ctx.git").stdout.decode().splitlines()
    tags, peeled = {}, {}
    for line in lines:
        commit, ref = line.split("\t")
        match = TAG.fullmatch(ref)
        if match and tuple(map(int, match.groups()[:3])) >= minimum:
            if not COMMIT.fullmatch(commit):
                raise ValueError("published release has an invalid Git identity")
            (peeled if match[4] else tags)[ref.removesuffix("^{}")] = commit
    if not tags or tags.keys() != peeled.keys():
        raise ValueError("published stable release tags are missing or not annotated")
    for ref, tip in peeled.items():
        # Resolve the remote object itself; stale local tags cannot hide a fix.
        actual = git(repo, "rev-parse", "--verify", f"{tags[ref]}^{{commit}}").stdout.decode().strip()
        if actual != tip:
            raise ValueError(f"published tag identity differs: {ref}")
    return peeled


def check_continuity(repo, commit, tips, policy):
    if not COMMIT.fullmatch(commit):
        raise ValueError("candidate source must be an exact commit")
    if set(policy) != {"minimum_release", "required_patches", "dispositions"}:
        raise ValueError("release continuity policy has unexpected fields")
    exceptions = policy["dispositions"]
    required = policy["required_patches"]
    if (not isinstance(exceptions, dict) or not isinstance(required, list)
            or any(not COMMIT.fullmatch(key) or not isinstance(reason, str) or not reason.strip()
                   for key, reason in exceptions.items())
            or any(not isinstance(key, str) or not COMMIT.fullmatch(key) for key in required)
            or set(required) & exceptions.keys()):
        raise ValueError("release dispositions must name exact commits and reasons; required patches cannot be waived")
    version_text = tomllib.loads(git(repo, "show", f"{commit}:Cargo.toml").stdout.decode())["workspace"]["package"]["version"]
    if not re.fullmatch(r"\d+\.\d+\.\d+", version_text):
        raise ValueError("release continuity requires a stable candidate version")
    version = tuple(map(int, version_text.split(".")))
    missing = set(required)
    for ref, tip in tips.items():
        match = TAG.fullmatch(ref)
        if match is None or not COMMIT.fullmatch(tip):
            raise ValueError("published release input is invalid")
        if tuple(map(int, match.groups()[:3])) > version:
            raise ValueError(f"candidate is older than published release {ref}")
        missing.update(git(repo, "rev-list", f"{commit}..{tip}").stdout.decode().splitlines())
    results, failures = [], []
    with tempfile.TemporaryDirectory(prefix="ctx-release-continuity-") as temporary:
        index = Path(temporary) / "index"
        git(repo, "read-tree", commit, index=index)
        for released in sorted(missing):
            parents = git(repo, "show", "-s", "--format=%P", released).stdout.decode().split()
            present = False
            if len(parents) == 1:
                patch = git(repo, "diff", "--binary", "--no-ext-diff", parents[0], released).stdout
                present = bool(patch) and git(
                    repo, "apply", "--cached", "--reverse", "--check", "--whitespace=nowarn",
                    index=index, data=patch, check=False).returncode == 0
            if present:
                results.append({"commit": released, "disposition": "patch_present"})
            elif released in exceptions:
                results.append({"commit": released, "disposition": "reviewed", "reason": exceptions[released]})
            else:
                subject = git(repo, "show", "-s", "--format=%s", released).stdout.decode().strip()
                failures.append(f"{released} {subject}")
    if failures:
        raise ValueError("released changes are absent without a reviewed disposition:\n" + "\n".join(failures))
    return {"candidate": commit, "version": version_text, "published_releases": tips,
            "non_ancestor_or_required_changes": results}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--public-repo", type=Path, required=True)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--source-commit")
    source.add_argument("--paired-candidate", type=Path)
    args = parser.parse_args()
    commit = args.source_commit
    if args.paired_candidate:
        commit = json.loads(args.paired_candidate.read_bytes())["public_source_commit"]
    head = git(args.public_repo, "rev-parse", "HEAD").stdout.decode().strip()
    if head != commit or git(args.public_repo, "status", "--porcelain=v1", "--untracked-files=all").stdout:
        raise ValueError("release continuity requires the clean exact public candidate checkout")
    policy = json.loads(POLICY.read_bytes())
    minimum = tuple(map(int, policy["minimum_release"].split(".")))
    tips = stable_tips(args.public_repo, minimum)
    print(json.dumps(check_continuity(args.public_repo, commit, tips, policy), indent=2))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
