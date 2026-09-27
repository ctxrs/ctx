#!/usr/bin/env python3
"""Small, offline Git histories reproduce the dropped-release-branch failure."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "continuity", ROOT / "scripts/release/released-source-continuity.py")
continuity = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(continuity)


class ContinuityTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.repo = Path(self.temporary.name)
        self.git("init", "-b", "main")
        self.git("config", "user.name", "Release fixture")
        self.git("config", "user.email", "release@example.invalid")
        (self.repo / "Cargo.toml").write_text('[workspace.package]\nversion = "1.4.1"\n')
        (self.repo / "client").write_text("no final recovery\n")
        self.base = self.commit("base")
        self.git("checkout", "-b", "bridge")
        (self.repo / "client").write_text("report final recovery\n")
        (self.repo / "test").write_text("assert final recovery after restart\n")
        self.fix = self.commit("fix and regression test")
        self.tips = {"refs/tags/v1.3.2": self.fix}
        self.policy = {"minimum_release": "1.3.2", "required_patches": [self.fix], "dispositions": {}}
        self.git("checkout", "main")
        (self.repo / "unrelated").write_text("new release\n")
        self.candidate = self.commit("new release without the bridge fix")

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.repo), *args],
                                       stderr=subprocess.DEVNULL, text=True).strip()

    def commit(self, subject):
        self.git("add", ".")
        self.git("commit", "-m", subject)
        return self.git("rev-parse", "HEAD")

    def check(self):
        return continuity.check_continuity(self.repo, self.candidate, self.tips, self.policy,
                                           getattr(self, "tag_objects", None))

    def carry_fix(self):
        self.git("cherry-pick", self.fix)
        self.candidate = self.git("rev-parse", "HEAD")

    def test_released_branch_omission_is_rejected(self):
        with self.assertRaisesRegex(ValueError, self.fix):
            self.check()

    def test_cherry_pick_is_present_without_requiring_ancestry_or_mutating_index(self):
        self.carry_fix()
        index = (self.repo / ".git/index").read_bytes()
        result = self.check()
        self.assertEqual(result["non_ancestor_or_required_changes"][0]["disposition"], "patch_present")
        self.assertEqual((self.repo / ".git/index").read_bytes(), index)
        self.assertEqual(self.git("status", "--porcelain"), "")

    def test_squashed_equivalent_patch_is_accepted(self):
        self.git("cherry-pick", "--no-commit", self.fix)
        (self.repo / "unrelated").write_text("combined release\n")
        self.candidate = self.commit("squash fix with other work")
        self.check()

    def test_removing_production_fix_or_its_test_blocks(self):
        for removed in ("client", "test"):
            with self.subTest(removed=removed):
                self.git("checkout", "-B", "mutation-" + removed, self.candidate)
                self.carry_fix()
                (self.repo / removed).write_text("regression\n")
                self.candidate = self.commit("remove " + removed)
                with self.assertRaisesRegex(ValueError, self.fix):
                    self.check()
                self.candidate = self.git("rev-parse", "main")

    def test_new_release_branch_fix_cannot_hide_behind_old_dispositions(self):
        self.carry_fix()
        self.git("checkout", "-b", "other-release", self.fix)
        (self.repo / "new-fix").write_text("another released repair\n")
        additional = self.commit("another release branch fix")
        self.tips["refs/tags/v1.4.0"] = additional
        with self.assertRaisesRegex(ValueError, additional):
            self.check()
        self.policy["dispositions"][additional] = "Explicitly retired old-protocol-only behavior."
        self.check()

    def test_required_regression_cannot_be_exempted(self):
        self.policy["dispositions"][self.fix] = "skip"
        with self.assertRaisesRegex(ValueError, "cannot be waived"):
            self.check()

    def test_refactored_patch_context_needs_an_exact_reviewed_disposition(self):
        self.carry_fix()
        # The released line remains, but adjacent refactored context makes a
        # textual reverse-apply inconclusive, as with the Cargo test move.
        (self.repo / "client").write_text("new adjacent context\nreport final recovery\n")
        self.candidate = self.commit("refactor surrounding context")
        self.policy["required_patches"] = []
        with self.assertRaisesRegex(ValueError, self.fix):
            self.check()
        self.policy["dispositions"][self.fix] = "Reviewed: released fix and test remain; only adjacent context changed."
        result = self.check()
        self.assertEqual(result["non_ancestor_or_required_changes"][0]["disposition"], "reviewed")

    def test_older_candidate_is_rejected(self):
        self.tips["refs/tags/v1.5.0"] = self.fix
        with self.assertRaisesRegex(ValueError, "older than published"):
            self.check()

    def maintenance_bridge(self):
        (self.repo / "Cargo.toml").write_text('[workspace.package]\nversion = "2.0.4"\n')
        self.newer = self.commit("published 2.x")
        self.git("checkout", "bridge")
        (self.repo / "Cargo.toml").write_text('[workspace.package]\nversion = "1.6.4"\n')
        base = self.commit("published bridge base")
        (self.repo / "Cargo.toml").write_text('[workspace.package]\nversion = "1.6.5"\n')
        self.candidate = self.commit("reviewed maintenance bridge")
        self.tips.update({"refs/tags/v1.6.4": base, "refs/tags/v2.0.4": self.newer})
        self.tag_objects = {}
        for ref, tip in self.tips.items():
            self.git("tag", "-a", ref.removeprefix("refs/tags/"), tip, "-m", "release fixture")
            self.tag_objects[ref] = self.git("rev-parse", ref)
        self.policy["maintenance_bridge"] = {
            "version": "1.6.5", "candidate": self.candidate, "base": base,
            "base_tag": "refs/tags/v1.6.4", "base_tag_object": self.tag_objects["refs/tags/v1.6.4"],
        }

    def test_checked_in_bridge_is_the_single_reviewed_identity(self):
        policy = json.loads(continuity.POLICY.read_bytes())
        self.assertEqual(policy["maintenance_bridge"], {
            "version": "1.6.5", "candidate": "ebb7a3ecfe47f43213622953c838f8a98d05703e",
            "base": "9623f15c06a61735739194b463c74c51b97892ad", "base_tag": "refs/tags/v1.6.4",
            "base_tag_object": "adf753882f00a00813a12714b4a49afa7655ef81",
        })
        self.assertEqual(policy["required_patches"], ["22ae223ebb4a0c909861e3a7e1e98d4bd5b2d523"])
        self.assertIn(policy["maintenance_bridge"]["candidate"], policy["dispositions"])
        self.assertFalse(set(policy["required_patches"]) & policy["dispositions"].keys())

    def test_exact_bridge_accounts_for_1x_and_required_patches_and_receipts_exclusions(self):
        self.maintenance_bridge()
        for already_published in (False, True):
            with self.subTest(already_published=already_published):
                if already_published:
                    self.tips["refs/tags/v1.6.5"] = self.candidate
                result = self.check()
                self.assertEqual(result["bridge_admission"], self.policy["maintenance_bridge"])
                self.assertEqual(result["excluded_published_releases"], {"refs/tags/v2.0.4": self.newer})
                self.assertEqual(result["published_releases"], self.tips)
                self.assertEqual(result["non_ancestor_or_required_changes"], [
                    {"commit": self.fix, "disposition": "patch_present"}])

    def test_bridge_wrong_source_is_an_ordinary_older_candidate(self):
        self.maintenance_bridge()
        (self.repo / "other").write_text("not the reviewed candidate\n")
        self.candidate = self.commit("unreviewed bridge change")
        with self.assertRaisesRegex(ValueError, "older than published release refs/tags/v2.0.4"):
            self.check()

    def test_bridge_wrong_source_version_is_rejected(self):
        self.maintenance_bridge()
        (self.repo / "Cargo.toml").write_text('[workspace.package]\nversion = "1.6.6"\n')
        self.candidate = self.commit("different version")
        self.policy["maintenance_bridge"]["candidate"] = self.candidate
        with self.assertRaisesRegex(ValueError, "bridge version differs"):
            self.check()

    def test_bridge_requires_exact_remote_annotated_base(self):
        self.maintenance_bridge()
        ref = "refs/tags/v1.6.4"
        for objects, tips in (
                (None, self.tips),
                ({}, self.tips),
                ({**self.tag_objects, ref: self.tag_objects["refs/tags/v2.0.4"]}, self.tips),
                (self.tag_objects, {key: value for key, value in self.tips.items() if key != ref}),
                (self.tag_objects, {**self.tips, ref: self.fix})):
            with self.subTest(objects=objects, tips=tips):
                with self.assertRaisesRegex(ValueError, "exact published annotated base tag"):
                    continuity.check_continuity(self.repo, self.candidate, tips, self.policy, objects)

    def test_bridge_base_must_be_an_ancestor(self):
        self.maintenance_bridge()
        self.policy["maintenance_bridge"].update(base=self.newer,
            base_tag_object=self.tag_objects["refs/tags/v2.0.4"])
        self.tag_objects["refs/tags/v1.6.4"] = self.tag_objects["refs/tags/v2.0.4"]
        self.tips["refs/tags/v1.6.4"] = self.newer
        with self.assertRaisesRegex(ValueError, "base must be an ancestor"):
            self.check()

    def test_bridge_conflicting_publication_is_rejected(self):
        self.maintenance_bridge()
        self.tips["refs/tags/v1.6.5"] = self.fix
        with self.assertRaisesRegex(ValueError, "published maintenance bridge differs"):
            self.check()

    def test_bridge_does_not_exclude_newer_1x_or_other_majors(self):
        self.maintenance_bridge()
        for version in ("1.6.6", "1.7.0", "3.0.0"):
            ref = f"refs/tags/v{version}"
            with self.subTest(version=version):
                self.tips[ref] = self.newer
                with self.assertRaisesRegex(ValueError, f"older than published release {ref}"):
                    self.check()
                del self.tips[ref]

    def test_bridge_still_accounts_for_all_1x_changes_and_dispositions(self):
        self.maintenance_bridge()
        self.git("checkout", "-b", "other-1x", self.fix)
        (self.repo / "another-fix").write_text("released 1.x repair\n")
        other = self.commit("other 1.x repair")
        self.tips["refs/tags/v1.6.3"] = other
        with self.assertRaisesRegex(ValueError, other):
            self.check()
        self.policy["dispositions"][other] = "Reviewed retirement of this exact fixture repair."
        result = self.check()
        self.assertIn({"commit": other, "disposition": "reviewed",
                       "reason": self.policy["dispositions"][other]}, result["non_ancestor_or_required_changes"])

    def test_bridge_required_patch_cannot_be_missing_or_waived(self):
        self.maintenance_bridge()
        # Even a required patch originating on the excluded 2.x branch is required.
        self.policy["required_patches"].append(self.newer)
        with self.assertRaisesRegex(ValueError, self.newer):
            self.check()
        self.policy["dispositions"][self.newer] = "attempted waiver"
        with self.assertRaisesRegex(ValueError, "cannot be waived"):
            self.check()

    def test_bridge_ancestry_does_not_excuse_removing_a_required_fix(self):
        self.maintenance_bridge()
        (self.repo / "client").write_text("no final recovery\n")
        self.candidate = self.commit("regress required repair on bridge")
        self.policy["maintenance_bridge"]["candidate"] = self.candidate
        with self.assertRaisesRegex(ValueError, self.fix):
            self.check()

    def test_every_remote_tag_is_authenticated_before_bridge_exclusion(self):
        self.maintenance_bridge()
        real_git = continuity.git

        def advertised(objects, tips):
            lines = [f"{value}\t{ref}" for ref, value in objects.items()]
            lines += [f"{value}\t{ref}^{{}}" for ref, value in tips.items()]
            return ("\n".join(lines) + "\n").encode()

        def remote(*args, **kwargs):
            if args[1] == "ls-remote":
                self.assertEqual(args[1:], ("ls-remote", "--tags", "https://github.com/ctxrs/ctx.git"))
                return subprocess.CompletedProcess(args, 0, listing, b"")
            return real_git(*args, **kwargs)

        listing = advertised(self.tag_objects, self.tips)
        with mock.patch.object(continuity, "git", side_effect=remote):
            objects, tips = continuity.stable_tips(self.repo, (1, 3, 2))
            continuity.check_continuity(self.repo, self.candidate, tips, self.policy, objects)
            # A bad 2.x peel must fail even though the admitted bridge would exclude it.
            listing = advertised(self.tag_objects, {**self.tips, "refs/tags/v2.0.4": self.fix})
            with self.assertRaisesRegex(ValueError, "published tag identity differs: refs/tags/v2.0.4"):
                continuity.stable_tips(self.repo, (1, 3, 2))
            # Nor may a fabricated peeled line turn a lightweight ref into an annotated tag.
            listing = advertised({**self.tag_objects, "refs/tags/v2.0.4": self.newer}, self.tips)
            with self.assertRaisesRegex(ValueError, "not annotated: refs/tags/v2.0.4"):
                continuity.stable_tips(self.repo, (1, 3, 2))
            listing = advertised(self.tag_objects, {key: value for key, value in self.tips.items()
                                                    if key != "refs/tags/v2.0.4"})
            with self.assertRaisesRegex(ValueError, "missing or not annotated"):
                continuity.stable_tips(self.repo, (1, 3, 2))

    def test_later_2x_accounts_for_future_bridge_tip_with_exact_disposition(self):
        self.maintenance_bridge()
        bridge = self.candidate
        self.git("checkout", "-b", "next-2x", self.policy["maintenance_bridge"]["base"])
        (self.repo / "Cargo.toml").write_text('[workspace.package]\nversion = "2.0.5"\n')
        self.candidate = self.commit("ordinary current release")
        self.tips["refs/tags/v2.0.4"] = self.policy["maintenance_bridge"]["base"]
        self.tips["refs/tags/v1.6.5"] = bridge
        with self.assertRaisesRegex(ValueError, bridge):
            self.check()
        self.policy["dispositions"][bridge] = "Current version supersedes the exact maintenance version."
        result = self.check()
        self.assertNotIn("bridge_admission", result)
        self.assertNotIn("excluded_published_releases", result)
        self.assertEqual({row["commit"]: row["disposition"] for row in result["non_ancestor_or_required_changes"]},
                         {self.fix: "patch_present", bridge: "reviewed"})

    def test_release_entry_points_enforce_check_before_signing(self):
        constructor = (ROOT / "scripts/release/release-manifest.mjs").read_text()
        main = constructor.split("async function main() {", 1)[1]
        self.assertLess(main.index("prepareManagedPairRelease("), main.index("await readPrivateKey()"))
        loader = (ROOT / "scripts/release/unified-release-inputs.mjs").read_text()
        self.assertIn('run("release/released-source-continuity.py", ["--public-repo", sourceRepo, "--source-commit", sourceCommit])', loader)
        self.assertLess(loader.index('run("release/released-source-continuity.py"'),
                        loader.index("return projectUnifiedReleaseInputs("))
        publisher = (ROOT / "scripts/release/publish-hosted-managed-pair-stable.sh").read_text()
        self.assertLess(publisher.index('"${prepare_args[@]}"'), publisher.index('metadata_key="$(secret'))
        verifier = (ROOT / "scripts/release/release-candidate-manifest-contract.cjs").read_text()
        self.assertIn('path.join(__dirname, "released-source-continuity.py")', verifier)
        self.assertIn('"--source-commit", sourceCommit', verifier)



if __name__ == "__main__":
    unittest.main()
