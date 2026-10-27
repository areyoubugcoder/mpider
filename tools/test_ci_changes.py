import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("ci_changes", Path(__file__).with_name("ci-changes.py"))
ci_changes = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ci_changes)


class CiChangesTests(unittest.TestCase):
    def test_docs_and_screenshots_skip_build(self):
        self.assertFalse(ci_changes.requires_build([
            "README.md", "CONTRIBUTING.md", "docs/how-it-works.md",
            "docs/screenshots/workbench-sources.png", ".github/release-notes.md",
        ]))

    def test_embedded_guides_are_application_inputs(self):
        for path in ci_changes.EMBEDDED_GUIDES:
            with self.subTest(path=path):
                self.assertTrue(ci_changes.requires_build([path]))

    def test_code_build_config_and_mixed_changes_run(self):
        for path in ["frontend/src/App.tsx", "src-tauri/src/lib.rs", "native/bridge.py",
                     "Cargo.lock", "pnpm-lock.yaml", "tools/package-windows-release.py",
                     ".github/workflows/ci.yml", ".github/workflows/release.yml"]:
            with self.subTest(path=path):
                self.assertTrue(ci_changes.requires_build(["README.md", path]))


if __name__ == "__main__":
    unittest.main()
