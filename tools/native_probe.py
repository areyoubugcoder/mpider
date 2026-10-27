#!/usr/bin/env python3
"""单个 Frida 会话的只读探针；原始桥消息仅落本机，终端只显示计数。"""

import argparse
import hashlib
import json
import os
import time
from pathlib import Path

import frida


def append_private_json(path, value):
    """原始采样仅写入调用者可读的本机文件。"""
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    try:
        if os.name != "nt":
            os.fchmod(fd, 0o600)
        with os.fdopen(fd, "a", encoding="utf-8") as handle:
            fd = -1
            handle.write(json.dumps(value, ensure_ascii=False) + "\n")
    finally:
        if fd != -1:
            os.close(fd)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pid", type=int, required=True)
    parser.add_argument("--script", required=True)
    parser.add_argument("--config", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--seconds", type=int, default=180)
    args = parser.parse_args()
    if not 1 <= args.seconds <= 600:
        parser.error("单次采样时间必须在 1 至 600 秒内")
    root = Path(args.out)
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    source = Path(args.script).read_text(encoding="utf-8")
    session = frida.attach(args.pid)
    script = None
    counts = {"bridge": 0, "error": 0}

    def on_message(message, data):
        if message.get("type") == "send":
            payload = message["payload"]
            append_private_json(root / "bridge.jsonl", {"time": time.time(), **payload})
            counts["bridge"] += 1
            print(json.dumps({"event": payload.get("event"), "rva": payload.get("rva"),
                              "bytes": len(payload.get("text", "")), "count": counts["bridge"]}), flush=True)
        else:
            counts["error"] += 1
            append_private_json(root / "errors.jsonl", message)
            print(json.dumps({"event": "agent_error", "count": counts["error"]}), flush=True)

    try:
        script = session.create_script(source)
        script.on("message", on_message)
        script.load()
        inventory = script.exports_sync.inventory()
        module_path = inventory.pop("module_path")
        module_hash = hashlib.sha256(Path(module_path).read_bytes()).hexdigest()
        print(json.dumps({"event": "inventory", **inventory, "module_sha256": module_hash}), flush=True)
        previous = None
        deadline = time.monotonic() + args.seconds
        next_status = 0
        while time.monotonic() < deadline:
            current = Path(args.config).read_text(encoding="utf-8-sig")
            if current != previous:
                config = json.loads(current)
                if config.get("stop"):
                    print(json.dumps({"event": "stop_requested"}), flush=True)
                    break
                if config.get("module_sha256") != module_hash:
                    raise ValueError("模块摘要不匹配，停止打桩")
                print(json.dumps({"event": "configured", **script.exports_sync.configure(config)}), flush=True)
                previous = current
            if time.monotonic() > next_status:
                print(json.dumps({"event": "status", **script.exports_sync.status()}), flush=True)
                next_status = time.monotonic() + 15
            time.sleep(1)
    finally:
        try:
            if script is not None:
                try:
                    script.exports_sync.stop()
                finally:
                    script.unload()
        finally:
            session.detach()
        print(json.dumps({"event": "detached", **counts}), flush=True)


if __name__ == "__main__":
    main()
