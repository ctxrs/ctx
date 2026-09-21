#!/usr/bin/env python3
"""Small, offline Git histories reproduce the dropped-release-branch failure."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

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
        return continuity.check_continuity(self.repo, self.candidate, self.tips, self.policy)

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

    def test_release_entry_points_enforce_check_before_signing(self):
        constructor = (ROOT / "scripts/release/release-manifest.mjs").read_text()
        main = constructor.split("async function main() {", 1)[1]
        self.assertLess(main.index("prepareManagedPairRelease("), main.index("await readPrivateKey()"))
        loader = (ROOT / "scripts/release/unified-release-inputs.mjs").read_text()
        self.assertIn('run("release/released-source-continuity.py", ["--public-repo", ROOT, "--source-commit", sourceCommit])', loader)
        self.assertLess(loader.index('run("release/released-source-continuity.py"'),
                        loader.index("return projectUnifiedReleaseInputs("))
        publisher = (ROOT / "scripts/release/publish-hosted-managed-pair-stable.sh").read_text()
        self.assertLess(publisher.index('"${prepare_args[@]}"'), publisher.index('metadata_key="$(secret'))
        verifier = (ROOT / "scripts/release/release-candidate-manifest-contract.cjs").read_text()
        self.assertIn('path.join(__dirname, "released-source-continuity.py")', verifier)
        self.assertIn('"--source-commit", sourceCommit', verifier)



if __name__ == "__main__":
    unittest.main()
