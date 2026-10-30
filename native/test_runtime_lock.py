import hashlib
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parent.parent


class RuntimeLockTests(unittest.TestCase):
    def test_vendored_licenses_match_locked_digests(self):
        # 在 Windows 检出上提前发现换行被转换，避免等到长构建后的组装阶段才失败。
        lock = json.loads((ROOT / "tools/windows-runtime.lock.json").read_text(encoding="utf-8"))
        for item in lock["licenses"]:
            data = (ROOT / item["path"]).read_bytes()
            self.assertEqual(hashlib.sha256(data).hexdigest(), item["sha256"], item["filename"])


if __name__ == "__main__":
    unittest.main()
