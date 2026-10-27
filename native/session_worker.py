"""会话采集工作进程：首屏自动取样，此后接受父进程许可逐页独立请求。"""
import hashlib
import json
import os
import queue
import sys
import subprocess
import threading
import time
from pathlib import Path

from session_protocol import fetch_page, payload_from_capture, request_parameters, validate_payload, private_replace
from session_vault import load, save
from worker import Messages, emit, select_profile


class Stopped(Exception):
    pass


class CaptureAudit:
    """仅诊断构建启用：记录采集阶段与桥接元数据，不保存请求正文或密钥。"""
    def __init__(self, args):
        self.path = (Path(args.config).parent / ("capture-audit-" + args.session + ".json")
                     if os.environ.get("MPIDER_CAPTURE_DIAGNOSTICS") == "1" else None)
        self.data = {"session": args.session, "started_at": time.time(), "events": []}

    def note(self, stage, **fields):
        if self.path is None:
            return
        self.data["events"].append({"ts": time.time(), "stage": stage, **fields})
        self.data["events"] = self.data["events"][-1000:]
        try:
            private_replace(self.path, self.data)
        except OSError:
            pass

    def bridge(self, observation):
        if self.path is None:
            return
        try:
            value = json.loads(observation.get("text", ""))
            params = value.get("params") or value.get("__params") or {}
            request = params.get("req_json") or {}
            request = json.loads(request) if isinstance(request, str) else request
            body = (params.get("jsapi_resp") or {}).get("resp_json") or {}
            body = json.loads(body) if isinstance(body, str) else body
            body = body if isinstance(body, dict) else {}
            self.note("bridge", phase=observation.get("phase"), slot=observation.get("slot"),
                      func=value.get("func"), msg_type=value.get("__msg_type"),
                      callback_id=str(value.get("__callback_id")), cgi=params.get("cgi_cmdid"),
                      hwnd=params.get("hwnd"), action=request.get("ActionType"),
                      has_msg_list="MsgList" in body,
                      nickname=(body.get("AccountInfo") or {}).get("NickName"))
        except (ValueError, TypeError, AttributeError):
            self.note("bridge_unparsed", phase=observation.get("phase"), slot=observation.get("slot"))


def command_reader(commands):
    for line in sys.stdin:
        try:
            value = json.loads(line)
        except ValueError:
            value = {"action": "invalid"}
        commands.put(value)
        if value.get("action") == "stop":
            return
    commands.put({"action": "stop"})


def next_command(commands, timeout=0.1):
    value = commands.get(timeout=timeout)
    if value.get("action") == "stop":
        raise Stopped()
    return value.get("action")


def capture_window(windows, name):
    """旧任务优先按名称匹配；新任务只使用前台或唯一的微信浏览器。"""
    browsers = [w for w in windows if w["class_name"] == "Chrome_WidgetWin_0"]
    targets = [w for w in browsers if name and w["title"] == name]
    if not targets:
        targets = [w for w in browsers if w["foreground"]]
    if not targets:
        targets = browsers
    if len(targets) != 1:
        raise ValueError("profile_window_not_unique")
    return targets[0]


def prepare_capture(config, args):
    import frida
    device = frida.get_local_device()
    processes = device.enumerate_processes()
    allowed = [p.pid for p in processes if p.name.lower() in ("weixin.exe", "wechatappex.exe")]
    profiles = json.loads(Path(args.profile).read_text(encoding="utf-8-sig"))
    source = "\n".join(Path(name).read_text(encoding="utf8") for name in
                       [args.agent, args.ui_agent, str(Path(args.config).parent / "session_agent.js")])
    for process in processes:
        if process.name.lower() != "weixin.exe":
            continue
        session, script = None, None
        selected = False
        try:
            session = device.attach(process.pid)
            guard = session.create_script("rpc.exports={module(){const m=Process.findModuleByName('Weixin.dll');return m?{path:m.path}:null;}}")
            guard.load()
            try:
                module = guard.exports_sync.module()
            finally:
                guard.unload()
            profile = select_profile(profiles, hashlib.sha256(Path(module["path"]).read_bytes()).hexdigest()) if module else None
            if not profile or "session_psk" not in profile:
                continue
            inbox = queue.Queue(maxsize=512)
            script = session.create_script(source)
            def receive(message, data):
                try:
                    inbox.put_nowait(message)
                except queue.Full:
                    # 溢出后失败关闭，不继续取样或产生网络请求。
                    overflow.set()
            overflow = threading.Event()
            script.on("message", receive)
            script.load()
            windows = script.exports_sync.windows(allowed)
            args.capture_audit.note("windows", pid=process.pid, windows=windows)
            target = capture_window(windows, config["window_title"])
            args.capture_audit.note("selected_window", target=target)
            script.exports_sync.session_start(profile["session_psk"])
            selected = True
            return session, script, inbox, overflow, profile, target
        except ValueError as error:
            if str(error) == "profile_window_not_unique":
                raise
        except Exception:
            continue
        finally:
            if not selected:
                if script is not None:
                    script.unload()
                if session is not None:
                    session.detach()
    raise ValueError("session_capture_environment")


def capture(config, args, prepared, commands):
    session, script, inbox, overflow, profile, target = prepared
    fields, candidates, responses, pending, history = {}, [], [], set(), {}
    def output(event, **value):
        if event == "fault":
            raise ValueError(value["reason"])
        if event == "response":
            args.capture_audit.note("accepted_response", request_id=value["request_id"])
            pending.discard(value["request_id"])
            raw = value["payload"].get("jsapi_resp", {}).get("resp_json")
            body = json.loads(raw) if isinstance(raw, str) else raw
            if isinstance(body, dict) and "MsgList" in body:
                responses.append(value)
                emit("target_detected", request_id=value["request_id"], payload=value["payload"])
                return
        if event == "request":
            args.capture_audit.note("accepted_request", cgi=value["request"]["cgi"], request_id=value["request"]["request_id"])
            if value["request"]["cgi"] == 5814 and config.get("_resume_cursor"):
                value["request"]["capture_only"] = True
            pending.add(value["request"]["request_id"])
        emit(event, **value)

    messages = Messages(args.session, target["hwnd"], config["window_title"],
                        config.get("expected_biz"), output=output)
    payload = None
    deadline = time.monotonic() + config["response_timeout_seconds"]
    try:
        script.exports_sync.configure(profile)
        script.exports_sync.session_arm()
        try:
            args.capture_audit.note("refresh_begin", hwnd=target["hwnd"], mode=profile["ui"].get("refresh_mode"))
            result = script.exports_sync.ui_action(target["hwnd"], "refresh", profile["ui"])
            args.capture_audit.note("refresh_returned", result=result)
        except Exception as error:
            raise ValueError("ui_action_failed") from error
        while time.monotonic() < deadline:
            try:
                if next_command(commands, timeout=0.001):
                    raise ValueError("invalid_control_command")
            except queue.Empty:
                pass
            if overflow.is_set():
                raise ValueError("session_capture_overflow")
            try:
                message = inbox.get(timeout=0.05)
            except queue.Empty:
                continue
            if message.get("type") != "send":
                raise ValueError("native_probe_error")
            value = message["payload"]
            if value.get("event") == "session_field":
                fields[value["name"]] = value["hex"]
                history.setdefault(value["name"], []).append(value["hex"])
                args.capture_audit.note("crypto_field", name=value["name"], bytes=len(value["hex"])//2)
            elif value.get("event") == "session_candidate":
                candidates.append(value["hex"])
                args.capture_audit.note("candidate", count=len(candidates))
            elif value.get("event") == "bridge":
                args.capture_audit.bridge(value)
                messages.accept(value)
            required = ["psk", "inner_enc_key", "inner_dec_key", "post_body"]
            private_replace(Path(args.config).parent / "session-status.json", {
                "fields": {k: len(v)//2 for k,v in fields.items()}, "candidates": len(candidates),
                "responses": len(responses), "pending": len(pending),
                "field_counts": {k: len(v) for k,v in history.items()}})
            if responses and candidates and all(fields.get(k) for k in required):
                for body in history["post_body"]:
                    for psk in history["psk"]:
                        try:
                            payload = payload_from_capture({**fields, "post_body": body, "psk": psk,
                                "inner_enc_keys": history["inner_enc_key"],
                                "inner_dec_keys": history["inner_dec_key"]}, candidates, time.time())
                            break
                        except Exception as error:
                            args.capture_audit.note("template_rejected", error_type=type(error).__name__)
                            private_replace(Path(args.config).parent / "session-template-status.json", {
                                "error_type": type(error).__name__,
                                "reason": str(error) if isinstance(error, ValueError) else "认证未通过"})
                    if payload:
                        break
                if payload is None:
                    continue
                if request_parameters(payload)["BizUserName"] != messages.user:
                    raise ValueError("target_changed")
                if pending:
                    continue
                break
        if payload is None or not responses:
            raise ValueError("session_capture_timeout")
        payload["module_sha256"] = profile["module_sha256"]
        payload["wechat_version"] = profile["wechat_version"]
        payload["target_name"] = messages.title
        save(Path(args.config).parent / "session-vault.dat", payload)
        args.capture_audit.note("capture_complete", responses=len(responses), candidates=len(candidates))
        emit("capture_complete", response=responses[0])
        # 取样结束后才交付本页，父进程可以安全调度独立翻页。
        return payload, responses[0]
    finally:
        args.capture_audit.note("capture_finish", field_counts={k:len(v) for k,v in history.items()}, candidates=len(candidates),responses=len(responses),pending=len(pending))
        # 卸载脚本自动移除全部 Interceptor，避免在热函数内逐个 detach 的同步等待。
        script.unload()
        session.detach()
        fields.clear()
        candidates.clear()


def capture_child(config, args):
    """只负责一次取样；父进程在材料交付后有界回收此进程。"""
    commands = queue.Queue()
    threading.Thread(target=command_reader, args=(commands,), daemon=True).start()
    prepared = None
    args.capture_audit = CaptureAudit(args)
    args.capture_audit.note("prepare_begin")
    try:
        prepared = prepare_capture(config, args)
        emit("ready", dll_sha256=prepared[4]["module_sha256"], version=prepared[4]["wechat_version"])
        while True:
            try:
                action = next_command(commands)
                break
            except queue.Empty:
                continue
        if action != "refresh":
            raise ValueError("invalid_control_command")
        current, prepared = prepared, None
        capture(config, args, current, commands)
    except Stopped:
        args.capture_audit.note("stopped")
        pass
    except Exception as error:
        reason = str(error)
        known = {"profile_window_not_unique", "target_name_mismatch", "target_changed", "session_capture_environment", "ui_action_failed", "session_capture_timeout", "session_capture_overflow", "native_probe_error", "invalid_control_command"}
        args.capture_audit.note("fault", reason=reason if reason in known else "other_error", error_type=type(error).__name__)
        emit("fault", reason=reason if reason in {"profile_window_not_unique", "target_name_mismatch", "target_changed", "session_capture_environment", "ui_action_failed"} else "session_capture_failed")
    finally:
        if prepared:
            prepared[1].unload()
            prepared[0].detach()


def capture_via_process(config, args, commands):
    arguments = [sys.executable, str(Path(__file__).with_name("worker.py"))]
    for name in ["config", "profile", "agent", "ui_agent", "session"]:
        arguments.extend(["--" + name.replace("_", "-"), str(getattr(args, name))])
    arguments.append("--capture-process")
    child = subprocess.Popen(arguments, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL, text=True, encoding="utf8",
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    observations = queue.Queue()
    def read():
        for line in child.stdout:
            try:
                observations.put(json.loads(line))
            except ValueError:
                observations.put({"event": "fault", "reason": "session_capture_failed"})
        observations.put(None)
    threading.Thread(target=read, daemon=True).start()
    try:
        while True:
            try:
                action = next_command(commands, timeout=0.001)
                child.stdin.write(json.dumps({"action": action}) + "\n")
                child.stdin.flush()
            except queue.Empty:
                pass
            try:
                observation = observations.get(timeout=0.05)
            except queue.Empty:
                continue
            if observation is None:
                raise ValueError("session_capture_failed")
            kind = observation.pop("event")
            if kind == "capture_complete":
                return load(Path(args.config).parent / "session-vault.dat"), observation["response"]
            if kind == "fault":
                raise ValueError(observation["reason"])
            emit(kind, **observation)
    finally:
        try:
            child.stdin.write('{"action":"stop"}\n')
            child.stdin.flush()
        except (BrokenPipeError, OSError):
            pass
        try:
            child.wait(timeout=2)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()
        child.stdin.close()
        child.stdout.close()


def run(config, args):
    # 依赖在任何页面操作前校验，缺包不会触发刷新。
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM  # noqa: F401
    commands = queue.Queue()
    threading.Thread(target=command_reader, args=(commands,), daemon=True).start()
    prepared = None
    payload = None
    cursor = config.get("_resume_cursor")
    sequence = 0
    try:
        if config.get("session_file"):
            payload = load(config["session_file"])
            validate_payload(payload)
            if config["window_title"] and payload.get("target_name") not in (None, config["window_title"]):
                raise ValueError("target_name_mismatch")
            save(Path(args.config).parent / "session-vault.dat", payload)
            emit("ready", dll_sha256=payload.get("module_sha256", "imported-session"),
                 version=payload.get("wechat_version", "已导入会话"))
        else:
            payload, response = capture_via_process(config, args, commands)
            raw = response["payload"]["jsapi_resp"]["resp_json"]
            body = json.loads(raw) if isinstance(raw, str) else raw
            config["window_title"] = payload["target_name"]
            cursor = config.get("_resume_cursor") or (body.get("MsgList", {}).get("PagingInfo") or {}).get("Offset")
            versions = Path(args.config).parent / "sessions"
            versions.mkdir(exist_ok=True)
            session_path = versions / (args.session + ".dat")
            evidence_path = versions / (args.session + "-home.dat")
            save(session_path, payload)
            save(evidence_path, {"body": body})
            emit("response", **response, session_path=str(session_path), evidence_path=str(evidence_path))
        while True:
            try:
                action = next_command(commands)
            except queue.Empty:
                continue
            if action not in ("refresh", "advance"):
                raise ValueError("invalid_control_command")
            if action == "advance" and not cursor:
                raise ValueError("session_cursor_missing")
            offset = cursor if action == "advance" else None
            sequence += 1
            request_id = f"{args.session}-session-{sequence}"
            request = {"request_id": request_id, "ts": time.time(), "cgi": 5814,
                       "action": 1 if offset else 0, "offset": offset,
                       "user_name": request_parameters(payload)["BizUserName"], "transport": "mmtls"}
            emit("request", request=request)
            # 网络请求必须等到父进程将预算和请求写入账本后才获准发送。
            if next_command(commands, timeout=config["response_timeout_seconds"]) != "permit":
                raise ValueError("invalid_control_command")
            try:
                evidence = {}
                checks, body = fetch_page(payload, offset, config["response_timeout_seconds"], evidence=evidence)
                if (body.get("BaseResponse") or {}).get("Ret") == 0:
                    name = (body.get("AccountInfo") or {}).get("NickName")
                    if not isinstance(name, str) or not name.strip() or (config["window_title"] and name != config["window_title"]):
                        raise ValueError("target_name_mismatch")
                    config["window_title"] = name
                cursor = (body.get("MsgList", {}).get("PagingInfo") or {}).get("Offset")
                protected = {}
                if not offset:
                    # 导入的有效会话同样保存本次真实首页证据，工作台不依赖重新点击微信。
                    versions = Path(args.config).parent / "sessions"
                    versions.mkdir(exist_ok=True)
                    session_path = versions / (args.session + ".dat")
                    evidence_path = versions / (request_id + "-home.dat")
                    save(session_path, payload)
                    save(evidence_path, {**evidence, "body": body, "checks": checks})
                    protected = {"session_path": str(session_path), "evidence_path": str(evidence_path)}
                emit("response", request_id=request_id, ts=time.time(), payload={
                    "transport": "mmtls", "authenticated_outer_records": checks["authenticated_outer_records"],
                    "inner_authenticated": checks["inner_authenticated"], "body": body}, **protected)
            except Exception:
                emit("request_failed", request_id=request_id, ts=time.time(), reason="session_request_failed")
                return
    except Stopped:
        pass
    except Exception as error:
        allowed = {"target_name_mismatch", "target_changed", "session_capture_timeout", "session_cursor_missing",
                   "session_capture_environment", "session_capture_overflow", "invalid_control_command",
                   "ui_action_failed", "native_probe_error", "session_capture_failed", "profile_window_not_unique"}
        emit("fault", reason=str(error) if str(error) in allowed else "session_environment_error")
    finally:
        if prepared is not None:
            prepared[1].unload()
            prepared[0].detach()
        emit("stopped")
