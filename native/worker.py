#!/usr/bin/env python3
"""Windows 原生采集工作进程：Frida 只读采样、校验目标窗口、接受父进程逐次页面指令。"""

import argparse
import hashlib
import json
import queue
import sys
import threading
import time
from pathlib import Path
from urllib.parse import parse_qs, urlparse

sys.path.insert(0, str(Path(__file__).resolve().parent))


def emit(event, **fields):
    print(json.dumps({"event": event, **fields}, ensure_ascii=True), flush=True)


def select_profile(document, module_hash):
    """只按精确模块摘要选择唯一适配；旧版单文件研究配置仍可使用。"""
    profiles = document.get("profiles", [document])
    matches = [p for p in profiles if p.get("module_sha256") == module_hash]
    return matches[0] if len(matches) == 1 else None


def same_hwnd(left, right):
    try:
        def convert(value):
            value = str(value)
            return int(value, 16 if value.lower().startswith("0x") else 10)
        return convert(left) == convert(right)
    except (ValueError, TypeError):
        return False


class Messages:
    """只处理规范参数槽；按目标窗口、请求与载荷身份筛选，编号复用不等于同一响应。"""
    def __init__(self, session_id, hwnd, title, expected_biz, output=emit):
        self.output = output
        self.session_id, self.hwnd, self.title = session_id, hwnd, title
        self.biz, self.user = expected_biz, None
        self.pending = {}
        self.sequence = 0

    def accept(self, observation):
        text = observation.get("text")
        if not isinstance(text, str):
            return
        try:
            message = json.loads(text)
        except (ValueError, TypeError):
            return
        if not isinstance(message, dict):
            return
        callback = str(message.get("__callback_id"))
        if observation.get("phase") == "enter" and observation.get("slot") == 0 and message.get("func") in ("H5ExtTransfer", "finderH5ExtTransfer"):
            params = message.get("params") or {}
            if not isinstance(params, dict):
                return
            try:
                cgi = int(params.get("cgi_cmdid", 0))
                request = params.get("req_json") or {}
                request = json.loads(request) if isinstance(request, str) else request
            except (ValueError, TypeError):
                return
            if not isinstance(request,dict):
                return
            if cgi == 5814:
                if not same_hwnd(params.get("hwnd"), self.hwnd):
                    return
                user = request.get("BizUserName")
                if not user or (self.user and user != self.user):
                    self.output("fault", reason="target_changed")
                    return
                self.user = user
            elif cgi == 26365:
                urls = request.get("ReqInfoList") or []
                if not self.biz or not urls or any(parse_qs(urlparse(x.get("Url", "")).query).get("__biz", [None])[0] != self.biz for x in urls):
                    return
            else:
                return
            self.sequence += 1
            identity = f"{self.session_id}-{self.sequence}"
            item = {"request_id": identity, "ts": time.time(), "cgi": cgi,
                    "action": request.get("ActionType"), "offset": request.get("Offset"),
                    "user_name": request.get("BizUserName")}
            self.pending.setdefault(callback, []).append(item)
            self.output("request", request=item)
        elif observation.get("phase") == "frame" and message.get("__msg_type") == "callback":
            pending = self.pending.get(callback) or []
            params = message.get("__params") or {}
            if not isinstance(params, dict):
                return
            if not pending or not str(params.get("err_msg", "")).startswith(("H5ExtTransfer:", "finderH5ExtTransfer:")):
                return
            raw = (params.get("jsapi_resp") or {}).get("resp_json")
            try:
                body = json.loads(raw) if isinstance(raw, str) else raw
            except (ValueError, TypeError):
                body = None
            # 成功列表按公众号身份核对；其它 H5 CGI 复用编号时不能抢走列表请求。
            is_list = isinstance(body, dict) and "MsgList" in body
            is_aux = isinstance(body,dict) and "AppMsgContextList" in body
            eligible = [p for p in pending if (p["cgi"] == 5814 and is_list) or (p["cgi"] == 26365 and is_aux)]
            if is_list:
                account = body.get("AccountInfo") or {}
                eligible = [p for p in eligible if account.get("UserName") == p["user_name"]]
                # 只从唯一关联请求的响应识别名称，不能被复用回调编号的其它账号污染。
                if not eligible:
                    return
                name = account.get("NickName")
                if not isinstance(name, str) or not name.strip() or (self.title and name != self.title):
                    self.output("fault", reason="target_name_mismatch")
                    return
                for msg in (body.get("MsgList") or {}).get("Msg", []):
                    details = (msg.get("AppMsg") or {}).get("DetailInfo") or []
                    if details:
                        self.biz = parse_qs(urlparse(details[0].get("ContentUrl", "").replace("&amp;", "&")).query).get("__biz", [None])[0]
                        break
            if not eligible:
                # 错误/空载荷无法凭载荷确定 CGI，只有唯一在途候选才关联。
                failed = (params.get("base_resp") or {}).get("ret") not in (None, 0) or (params.get("jsapi_resp") or {}).get("ret") not in (None, 0) or (isinstance(body, dict) and (body.get("BaseResponse") or {}).get("Ret") not in (None, 0))
                if not is_list and len(pending) == 1 and (not raw or failed):
                    eligible = list(pending)
                else:
                    return
            if len(eligible) != 1:
                self.output("fault",reason="ambiguous_callback")
                return
            selected = eligible[0]
            if is_list:
                self.title = name
            pending.remove(selected)
            self.output("response", request_id=selected["request_id"], ts=time.time(), payload=params)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--agent", required=True)
    parser.add_argument("--ui-agent", required=True)
    parser.add_argument("--session", required=True)
    parser.add_argument("--inspect", action="store_true")
    parser.add_argument("--capture-process", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    config = json.loads(Path(args.config).read_text(encoding="utf-8-sig"))
    for deps in [Path(sys.executable).parent / "session-deps", Path(args.config).parent / "deps"]:
        if deps.is_dir():
            sys.path.insert(0, str(deps))
    if config.get("transport") == "session":
        from session_worker import run, capture_child
        (capture_child if args.capture_process else run)(config, args)
        return
    import frida
    profile_document = json.loads(Path(args.profile).read_text(encoding="utf-8-sig"))
    profile = None
    source = Path(args.agent).read_text(encoding="utf-8") + "\n" + Path(args.ui_agent).read_text(encoding="utf-8")
    device = frida.get_local_device()
    all_processes = device.enumerate_processes()
    allowed_pids = [p.pid for p in all_processes if p.name.lower() in ("weixin.exe", "wechatappex.exe")]
    processes = sorted([p for p in all_processes if p.name.lower() == "weixin.exe"], key=lambda p: p.pid)
    session, script = None, None
    try:
        for process in processes:
            candidate = None
            try:
                candidate = device.attach(process.pid)
                probe = candidate.create_script("rpc.exports={probe(){const m=Process.findModuleByName('Weixin.dll');return m?{path:m.path,size:m.size}:null;}};")
                probe.load()
                module = probe.exports_sync.probe()
                probe.unload()
                candidate_profile = select_profile(profile_document, hashlib.sha256(Path(module["path"]).read_bytes()).hexdigest()) if module else None
                if candidate_profile:
                    candidate_script = candidate.create_script(source)
                    candidate_script.load()
                    candidate_windows = candidate_script.exports_sync.windows(allowed_pids)
                    if args.inspect:
                        emit("inspection", pid=process.pid, windows=[{k: v for k, v in w.items() if k != "title"} |
                            {"title_matches": w["title"] == config["window_title"]} for w in candidate_windows])
                    candidate_script.unload()
                    if any(w["title"] == config["window_title"] or (w["foreground"] and w["class_name"] == "Chrome_WidgetWin_0") for w in candidate_windows):
                        session = candidate
                        profile = candidate_profile
                        break
            except Exception:
                pass
            finally:
                if candidate is not None and candidate is not session:
                    candidate.detach()
        if args.inspect:
            return
        if session is None:
            emit("fault", reason="unsupported_version_or_no_wechat")
            return
        script = session.create_script(source)
        inbox = queue.Queue()
        script.on("message", lambda message, data: inbox.put(message))
        script.load()
        windows = script.exports_sync.windows(allowed_pids)
        matches = [w for w in windows if w["title"] == config["window_title"] and w["class_name"] == "Chrome_WidgetWin_0"]
        if not matches:
            matches = [w for w in windows if w["foreground"] and w["class_name"] == "Chrome_WidgetWin_0"]
        if len(matches) != 1:
            emit("fault", reason="profile_window_not_unique")
            return
        target = matches[0]
        messages = Messages(args.session, target["hwnd"], config["window_title"], config.get("expected_biz"))
        script.exports_sync.configure(profile)
        commands = queue.Queue()

        def read_commands():
            for line in sys.stdin:
                try:
                    command=json.loads(line)
                    commands.put(command)
                    if command.get("action")=="stop":
                        return
                except ValueError:
                    commands.put({"action": "invalid"})
            commands.put({"action": "stop"})

        threading.Thread(target=read_commands, daemon=True).start()
        emit("ready", dll_sha256=profile["module_sha256"], version=profile["wechat_version"])
        while True:
            try:
                command = commands.get_nowait()
                action = command.get("action")
                if action == "stop":
                    break
                if action not in ("refresh", "advance"):
                    emit("fault", reason="invalid_control_command")
                    break
                try:
                    script.exports_sync.ui_action(target["hwnd"], action, profile["ui"])
                except Exception:
                    emit("fault", reason="ui_action_failed")
                    break
            except queue.Empty:
                pass
            try:
                message = inbox.get(timeout=0.1)
            except queue.Empty:
                continue
            if message.get("type") != "send":
                emit("fault", reason="native_probe_error")
                break
            try:
                messages.accept(message["payload"])
            except Exception as error:
                trace=error.__traceback__
                while trace.tb_next:trace=trace.tb_next
                emit("fault", reason=f"bridge_payload_error_{type(error).__name__}_{trace.tb_lineno}")
                break
    finally:
        if script is not None:
            try:
                script.exports_sync.stop()
            except Exception:
                pass
            try:
                script.unload()
            except Exception:
                pass
        if session is not None:
            session.detach()
        emit("stopped")


if __name__ == "__main__":
    try:
        main()
    except Exception:
        # 父进程接收机器可读的错误类别，路径、桥原文及登录信息不进终端日志。
        emit("fault", reason="worker_environment_error")
        sys.exit(1)
