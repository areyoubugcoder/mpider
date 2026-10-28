import importlib.util
from pathlib import Path
import re
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

    def test_markdown_imported_by_frontend_requires_build(self):
        root = Path(__file__).resolve().parent.parent
        imports = []
        for source in (root / "frontend/src").rglob("*.tsx"):
            for relative in re.findall(r'from\s+["\x27]([^"\x27]+\.md)\?raw["\x27]', source.read_text()):
                path = (source.parent / relative).resolve().relative_to(root).as_posix()
                imports.append(path)
                with self.subTest(path=path):
                    self.assertTrue(ci_changes.requires_build([path]))
        self.assertIn("CHANGELOG.md", imports)

    def test_repository_metadata_local_simulations_and_mobile_icons_skip_build(self):
        paths = [
            ".gitignore", ".github/PULL_REQUEST_TEMPLATE.md",
            ".github/ISSUE_TEMPLATE/bug_report.yml",
            "postman/mocks/updater-release/config.yaml",
            "postman/mocks/updater-release/default.js",
            "postman/simulations/updater-download.sim.yaml",
            "src-tauri/icons-v2/android/mipmap-hdpi/ic_launcher.png",
            "src-tauri/icons-v2/ios/AppIcon-20x20@1x.png",
        ]
        for path in paths:
            with self.subTest(path=path):
                self.assertFalse(ci_changes.requires_build([path]))
                self.assertTrue(ci_changes.requires_build([path, "frontend/src/App.tsx"]))
        self.assertFalse(ci_changes.requires_build(paths))

    def test_code_build_config_and_mixed_changes_run(self):
        for path in ["frontend/src/App.tsx", "src-tauri/src/lib.rs", "native/bridge.py",
                     "Cargo.lock", "pnpm-lock.yaml", "tools/package-windows-release.py",
                     ".github/workflows/ci.yml", ".github/workflows/release.yml",
                     ".gitattributes", "config/advertisements.json",
                     "src-tauri/icons-v2/icon.ico", "src-tauri/fixtures/updater/package.bin",
                     "new-build-config.json"]:
            with self.subTest(path=path):
                self.assertTrue(ci_changes.requires_build(["README.md", path]))


if __name__ == "__main__":
    unittest.main()
