"""发行说明入口中的本地帮助链接应随安装包提供。"""
import ast
import posixpath
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


class ReleaseDocumentsTests(unittest.TestCase):
    def test_readme_and_faq_document_links_are_packaged(self):
        tree = ast.parse((ROOT / "tools/package-windows-release.py").read_text(encoding="utf-8"))
        public = next(ast.literal_eval(node.value) for node in tree.body
                      if isinstance(node, ast.Assign)
                      and any(isinstance(t, ast.Name) and t.id == "PUBLIC_DOCUMENTS" for t in node.targets))
        for entry in ["README.md", "docs/faq.md"]:
            text = (ROOT / entry).read_text(encoding="utf-8")
            for link in re.findall(r"\]\(([^)\s]+)\)", text):
                if ":" in link or link.startswith("#"):
                    continue
                target = posixpath.normpath(posixpath.join(posixpath.dirname(entry), link.split("#")[0]))
                if target.startswith("docs/"):
                    with self.subTest(entry=entry, target=target):
                        self.assertIn(target, public)
                        self.assertTrue((ROOT / target).is_file())


if __name__ == "__main__":
    unittest.main()
