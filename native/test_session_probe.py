"""实验探针契约：固定输入、分层失败与无网络的旧游标保护迁移。"""
import hashlib
import json
import subprocess
import sys
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch
from cryptography.exceptions import InvalidTag
import session_probe
from test_session_protocol import fixture


class ProbeTests(unittest.TestCase):
    def test_standalone_probe_boots_with_isolated_python_module_search_path(self):
        # -I 移除脚本目录的隐式搜索，重现 Windows 嵌入式 Python 的导入条件。
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run([sys.executable, "-I", str(Path(session_probe.__file__).resolve())],
                                    input="{}\n", text=True, capture_output=True, cwd=directory, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["outcome"], "protocol_or_session_error")

    def test_sealing_legacy_cursor_does_not_fetch_or_invent_response(self):
        with patch.object(session_probe, "save") as save, patch.object(session_probe, "fetch_page") as fetch:
            result = session_probe.execute({"mode": "seal_legacy_cursor", "offset": "synthetic-offset",
                "native_job_id": 7, "observed_at": 1000, "evidence_file": "synthetic.dat"})
        self.assertEqual(result["outcome"], "cursor_sealed")
        self.assertEqual(result["hash"], hashlib.sha256(b"synthetic-offset").hexdigest())
        fetch.assert_not_called()
        data = save.call_args.args[1]
        self.assertNotIn("BaseResponse", data["body"])
        self.assertEqual(data["origin"], "legacy_native_checkpoint")

    def test_repeated_probe_keeps_original_cursor_and_session_files(self):
        payload, _ = fixture()
        original = {"body": {"MsgList": {"PagingInfo": {"Offset": "selected-cursor"}}}}
        body = {"BaseResponse": {"Ret": 0}, "MsgList": {"PagingInfo": {"Offset": "response-next"}}}
        checks = {"authenticated_outer_records": 4, "inner_authenticated": True}
        config = {"session_file": "fixed-session.dat", "cursor_file": "fixed-page.dat", "evidence_file": "probe.dat"}
        with patch.object(session_probe, "load", side_effect=[payload, original, payload, original]), patch.object(session_probe, "fetch_page", return_value=(checks, body)) as fetch, patch.object(session_probe, "save") as save:
            for _ in range(2):
                result = session_probe.execute(config)
                self.assertEqual(result["outcome"], "response")
            self.assertEqual([call.args[1] for call in fetch.call_args_list], ["selected-cursor"] * 2)
            self.assertTrue(all(call.args[0] == "probe.dat" for call in save.call_args_list))
        self.assertEqual(original["body"]["MsgList"]["PagingInfo"]["Offset"], "selected-cursor")

    def test_authentication_and_http_errors_do_not_become_cursor_expiry(self):
        payload, _ = fixture()
        for error, outcome in [(InvalidTag(), "authentication_failed"), (session_probe.HttpFailure(429), "http_error"), (TimeoutError(), "timeout")]:
            with patch.object(session_probe, "load", return_value=payload), patch.object(session_probe, "fetch_page", side_effect=error):
                result = session_probe.execute({"session_file": "fixed.dat", "evidence_file": "probe.dat"})
            self.assertEqual(result["outcome"], outcome)
            self.assertNotIn("expired", result["outcome"])
            if outcome == "http_error":
                self.assertEqual(result["http_status"], 429)


if __name__ == "__main__":
    unittest.main()
