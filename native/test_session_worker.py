"""用真实子进程验证取样交付后的有界回收，不访问微信或网络。"""
import queue
import io
import subprocess
import sys
import tempfile
import time
import unittest
from argparse import Namespace
from pathlib import Path
from unittest.mock import patch

from session_worker import Stopped, capture_via_process, capture_window, run


class WindowSelectionTests(unittest.TestCase):
    def test_auto_selects_only_or_foreground_browser(self):
        one={"hwnd":"0x10","class_name":"Chrome_WidgetWin_0","title":"微信","foreground":False}
        two={**one,"hwnd":"0x20","foreground":True}
        self.assertEqual(capture_window([one],"")["hwnd"],"0x10")
        self.assertEqual(capture_window([one,two],"")["hwnd"],"0x20")
        with self.assertRaisesRegex(ValueError,"profile_window_not_unique"):
            capture_window([one,{**two,"foreground":False}],"")
        with self.assertRaisesRegex(ValueError,"profile_window_not_unique"):
            capture_window([],"")


class CaptureProcessTests(unittest.TestCase):
    def test_imported_session_delivers_protected_real_home_response_to_workbench(self):
        body = {"BaseResponse": {"Ret": 0}, "AccountInfo": {"NickName": "虚构公众号"},
                "MsgList": {"PagingInfo": {"Offset": "synthetic-offset"}}}
        checks = {"authenticated_outer_records": 2, "inner_authenticated": True}
        saved, events = {}, []
        with tempfile.TemporaryDirectory() as directory:
            args = Namespace(config=str(Path(directory)/"config.json"), session="synthetic")
            config = {"session_file": "synthetic.dat", "window_title": "虚构公众号", "response_timeout_seconds": 1}
            with patch("session_worker.sys.stdin", io.StringIO('{"action":"refresh"}\n{"action":"permit"}\n{"action":"stop"}\n')), \
                 patch("session_worker.load", return_value={"target_name": "虚构公众号"}), \
                 patch("session_worker.validate_payload"), \
                 patch("session_worker.request_parameters", return_value={"BizUserName": "synthetic"}), \
                 patch("session_worker.fetch_page", return_value=(checks, body)), \
                 patch("session_worker.save", side_effect=lambda path,value: saved.update({str(path):value})), \
                 patch("session_worker.emit", side_effect=lambda event,**value: events.append((event,value))):
                run(config, args)
            response = next(value for event,value in events if event == "response")
            self.assertEqual(saved[response["evidence_path"]]["body"], body)
            self.assertEqual(saved[response["evidence_path"]]["checks"], checks)
            self.assertEqual(saved[response["session_path"]]["target_name"], "虚构公众号")
            self.assertTrue(response["payload"]["inner_authenticated"])

    def exercise(self, stop):
        original_popen = subprocess.Popen
        processes = []
        source = '''import json,sys,time
print(json.dumps({"event":"ready","dll_sha256":"fake","version":"fake"}),flush=True)
sys.stdin.readline()
print(json.dumps({"event":"capture_complete","response":{"request_id":"fake"}}),flush=True)
time.sleep(30)
'''
        def spawn(_args, **kwargs):
            process = original_popen([sys.executable, "-u", "-c", source], **kwargs)
            processes.append(process)
            return process
        with tempfile.TemporaryDirectory() as directory:
            args = Namespace(config=str(Path(directory)/"config.json"),profile="fake",
                             agent="fake",ui_agent="fake",session="fake")
            commands = queue.Queue()
            commands.put({"action":"stop" if stop else "refresh"})
            started = time.monotonic()
            with patch("session_worker.subprocess.Popen", side_effect=spawn), \
                 patch("session_worker.load", return_value={"fake":"material"}), \
                 patch("session_worker.emit"):
                if stop:
                    with self.assertRaises(Stopped):
                        capture_via_process({},args,commands)
                else:
                    payload,response=capture_via_process({},args,commands)
                    self.assertEqual(payload,{"fake":"material"})
                    self.assertEqual(response["request_id"],"fake")
            self.assertLess(time.monotonic()-started,6)
            self.assertIsNotNone(processes[0].poll())

    def test_handoff_reaps_a_stuck_capture_process(self):
        self.exercise(False)

    def test_stop_reaps_capture_process(self):
        self.exercise(True)


if __name__ == "__main__":
    unittest.main()
