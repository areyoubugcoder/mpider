import importlib.util
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("ci_changes", Path(__file__).with_name("ci-changes.py"))
ci_changes = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ci_changes)


class CiChangesTests(unittest.TestCase):
    def assert_checks(self, paths, *expected):
        result = ci_changes.checks_for_paths(paths)
        self.assertEqual(set(result), set(ci_changes.CHECKS))
        self.assertEqual({name for name, enabled in result.items() if enabled}, set(expected))

    def test_public_material_and_unused_assets_skip_application_checks(self):
        self.assert_checks([
            "README.md", "CONTRIBUTING.md", "LICENSE", "docs/how-it-works.md",
            "docs/screenshots/workbench-sources.png", ".github/release-notes.md",
            ".gitignore", ".github/PULL_REQUEST_TEMPLATE.md",
            ".github/ISSUE_TEMPLATE/bug_report.yml",
            "postman/mocks/updater-release/default.js",
            "postman/simulations/updater-download.sim.yaml",
            "src-tauri/icons-v2/android/mipmap-hdpi/ic_launcher.png",
            "src-tauri/icons-v2/ios/AppIcon-20x20@1x.png",
        ])

    def test_frontend_inputs_do_not_compile_rust(self):
        for path in ["CHANGELOG.md", "docs/faq.md", "docs/laboratory.md", "docs/request-policy.md",
                     "frontend/src/index.css", "frontend/src/components/Sidebar.tsx",
                     "frontend/src/assets/app-icon.png", "vite.config.ts", "tailwind.config.cjs",
                     "config/advertisements.json", "tools/test_home_sync.ts", "tools/release-updater.mjs"]:
            with self.subTest(path=path):
                self.assert_checks([path], "frontend")

    def test_markdown_imported_by_frontend_is_checked(self):
        root = Path(__file__).resolve().parent.parent
        imports = []
        for source in (root / "frontend/src").rglob("*.tsx"):
            for relative in re.findall(r'''from\s+["']([^"']+\.md)\?raw["']''', source.read_text()):
                path = (source.parent / relative).resolve().relative_to(root).as_posix()
                imports.append(path)
                with self.subTest(path=path):
                    self.assert_checks([path], "frontend")
        self.assertIn("CHANGELOG.md", imports)

    def test_rust_inputs_do_not_build_frontend(self):
        for path in ["crates/article-md/src/text.rs", "crates/core/src/ratelimit.rs", "Cargo.lock",
                     "src-tauri/Cargo.toml", "src-tauri/icons-v2/icon.ico",
                     "src-tauri/fixtures/updater/package.bin", "tools/run-windows-test.ps1"]:
            with self.subTest(path=path):
                self.assert_checks([path], "rust")

    def test_native_workers_are_checked_on_both_sides(self):
        for path in ["native/session_protocol.py", "native/session_agent.js",
                     "native/profiles/4.1.15.56.json", "tools/native_bridge_probe.js"]:
            with self.subTest(path=path):
                self.assert_checks([path], "rust", "native")
        root = Path(__file__).resolve().parent.parent
        source = root / "crates/core/src/native/runtime.rs"
        for relative in re.findall(r'include_str!\("([^"]+)"\)', source.read_text()):
            path = (source.parent / relative).resolve().relative_to(root).as_posix()
            with self.subTest(embedded=path):
                self.assert_checks([path], "rust", "native")

    def test_native_test_only_changes_do_not_compile_application(self):
        for path in ["native/test_session_protocol.py", "tools/test_session_agent.mjs",
                     "tools/test_native_bridge_filter.mjs"]:
            with self.subTest(path=path):
                self.assert_checks([path], "native")

    def test_shared_contracts_link_relevant_checks(self):
        for path in ["frontend/src/lib/tauri.ts", "frontend/src/lib/workbench.ts",
                     "src-tauri/src/lib.rs", "src-tauri/src/workbench.rs",
                     "crates/core/src/native/protocol.rs", "crates/core/src/model.rs"]:
            with self.subTest(path=path):
                self.assert_checks([path], "frontend", "rust", "native")
        for path in ["Cargo.toml", "src-tauri/tauri.conf.json"]:
            with self.subTest(version_input=path):
                self.assert_checks([path], "frontend", "rust")

    def test_filter_and_summary_script_changes_only_validate_ci(self):
        for path in ["tools/ci-changes.py", "tools/test_ci_changes.py",
                     "tools/check-ci-results.py", "tools/test_ci_results.py"]:
            with self.subTest(path=path):
                self.assert_checks([path], "ci")

    def test_workflows_dependencies_and_unknown_inputs_run_all_checks(self):
        for path in [".github/workflows/ci.yml", ".github/workflows/release.yml",
                     "package.json", "pnpm-lock.yaml", ".gitattributes",
                     "tools/package-windows-release.py", "new-build-config.json"]:
            with self.subTest(path=path):
                self.assert_checks([path], *ci_changes.CHECKS)

    def test_mixed_changes_take_union(self):
        self.assert_checks(["README.md", "frontend/src/index.css"], "frontend")
        self.assert_checks(["CHANGELOG.md", "crates/article-md/src/text.rs"], "frontend", "rust")
        self.assert_checks(["tools/ci-changes.py", "native/session_worker.py"], "ci", "native", "rust")
        self.assert_checks([])

    def test_manual_runs_and_diff_errors_do_not_skip_checks(self):
        expected = dict.fromkeys(ci_changes.CHECKS, True)
        self.assertEqual(ci_changes.checks_for_event({}, "workflow_dispatch"), expected)
        with patch.object(ci_changes, "changed_paths", side_effect=subprocess.CalledProcessError(1, "git")):
            self.assertEqual(ci_changes.checks_for_event({}, "pull_request"), expected)

    def test_real_pr_diff_includes_all_commits_deletions_and_renames(self):
        with tempfile.TemporaryDirectory() as directory:
            env = {**os.environ, "GIT_AUTHOR_NAME": "CI Fixture", "GIT_COMMITTER_NAME": "CI Fixture",
                   "GIT_AUTHOR_EMAIL": "ci@example.invalid", "GIT_COMMITTER_EMAIL": "ci@example.invalid"}

            def git(*args):
                return subprocess.check_output(["git", "-c", "commit.gpgsign=false", *args],
                                               cwd=directory, env=env, text=True).strip()

            def commit():
                git("add", "-A")
                git("commit", "-qm", "fixture")
                return git("rev-parse", "HEAD")

            root = Path(directory)
            git("init", "-q")
            (root / "native").mkdir()
            (root / "native/worker.py").write_text("fixture\n")
            (root / "README.md").write_text("before\n")
            base = commit()
            (root / "README.md").write_text("after\n")
            commit()
            (root / "docs").mkdir()
            (root / "native/worker.py").rename(root / "docs/worker-example.py")
            head = commit()
            event = {"pull_request": {"base": {"sha": base}, "head": {"sha": head}}}
            paths = ci_changes.changed_paths(event, cwd=directory)
            self.assertEqual(set(paths), {"README.md", "native/worker.py", "docs/worker-example.py"})
            self.assert_checks(paths, "rust", "native")


if __name__ == "__main__":
    unittest.main()
