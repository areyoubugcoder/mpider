"""工作台单次请求：读取受保护会话和游标证据，保存响应，绝不更改生产检查点。"""
import json
import hashlib
import socket
import sys
from pathlib import Path

# Windows 嵌入式 Python 的 ._pth 默认不加入脚本目录，必须显式加载同目录协议模块。
sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(sys.executable).parent / "session-deps"))
from cryptography.exceptions import InvalidTag
from session_protocol import fetch_page, request_parameters, HttpFailure
from session_vault import load, save


def execute(config):
    evidence = {}
    try:
        if config.get("mode") == "seal_legacy_cursor":
            offset = config["offset"]
            if not isinstance(offset, str) or not 0 < len(offset) <= 1024 * 1024:
                return {"outcome": "invalid_cursor_evidence"}
            save(config["evidence_file"], {"origin": "legacy_native_checkpoint",
                 "native_job_id": config["native_job_id"], "observed_at": config["observed_at"],
                 "body": {"MsgList": {"PagingInfo": {"Offset": offset}}}})
            return {"outcome": "cursor_sealed", "hash": hashlib.sha256(offset.encode()).hexdigest()}
        payload = load(config["session_file"])
        cursor = None
        if config.get("cursor_file"):
            previous = load(config["cursor_file"])
            cursor = previous["body"]["MsgList"]["PagingInfo"]["Offset"]
            if not isinstance(cursor, str) or not cursor:
                return {"outcome": "invalid_cursor_evidence"}
        checks, body = fetch_page(payload, cursor, config.get("timeout", 45), evidence=evidence)
        evidence.update(body=body, checks=checks)
        save(config["evidence_file"], evidence)
        return {"outcome": "response", "request_user": request_parameters(payload)["BizUserName"],
                "request_cursor_hash": hashlib.sha256(cursor.encode()).hexdigest() if cursor else None,
                "evidence_path": config["evidence_file"], "payload": {
                    "transport": "mmtls", "authenticated_outer_records": checks["authenticated_outer_records"],
                    "inner_authenticated": checks["inner_authenticated"], "body": body}}
    except HttpFailure as error:
        return {"outcome": "http_error", "http_status": error.status}
    except (TimeoutError, socket.timeout):
        return {"outcome": "timeout"}
    except InvalidTag:
        return {"outcome": "authentication_failed"}
    except OSError:
        return {"outcome": "network_or_storage_error"}
    except (ValueError, KeyError, TypeError, RuntimeError) as error:
        # 失败文本只允许协议内已知分类；不得输出文件内容、密钥或任意异常字符串。
        reason = str(error)
        outcome = "authentication_failed" if reason in {
            "内层响应未通过认证", "服务器未返回可验证的恢复握手"} else "protocol_or_session_error"
        return {"outcome": outcome}
    finally:
        if evidence.get("wire_response_hex") and "body" not in evidence:
            try:
                save(config["evidence_file"], evidence)
            except Exception:
                pass


if __name__ == "__main__":
    try:
        print(json.dumps(execute(json.loads(sys.stdin.readline())), ensure_ascii=True))
    except Exception:
        print('{"outcome":"worker_error"}')
