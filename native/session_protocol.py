#!/usr/bin/env python3
"""原生会话协议：模板验证、两层认证加解密及受限列表请求；不依赖微信进程。"""

import hashlib
import json
import os
import http.client
import re
import struct
import time
import zlib
from datetime import datetime, timezone

MAX_PAYLOAD = 8 * 1024 * 1024


def decompress(data):
    decoder = zlib.decompressobj()
    value = decoder.decompress(data, MAX_PAYLOAD + 1)
    if len(value) > MAX_PAYLOAD or not decoder.eof or decoder.unused_data:
        raise ValueError("压缩载荷超过上限或不完整")
    return value


def private_write(path, data):
    """敏感文件仅落到调用方指定的研究目录，不输出内容。"""
    data = data if isinstance(data, bytes) else json.dumps(data, ensure_ascii=False, indent=2).encode()
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as handle:
        handle.write(data)


def private_replace(path, data):
    temporary = path.with_name(path.name + "." + os.urandom(6).hex() + ".tmp")
    private_write(temporary, data)
    os.replace(temporary, path)


def records(data):
    result, offset = [], 0
    while offset < len(data):
        header = data[offset:offset + 5]
        if len(header) != 5 or header[:3] not in [bytes.fromhex(v) for v in
                                                 ["19f104", "16f104", "17f104", "15f104"]]:
            raise ValueError("记录头无效")
        size = int.from_bytes(header[3:5], "big")
        end = offset + 5 + size
        if end > len(data):
            raise ValueError("记录不完整")
        result.append((header, data[offset + 5:end]))
        offset = end
    return result


def expand(psk, label, transcript):
    from cryptography.hazmat.primitives import hashes
    from cryptography.hazmat.primitives.kdf.hkdf import HKDFExpand
    value = HKDFExpand(hashes.SHA256(), 28, label + hashlib.sha256(transcript).digest()).derive(psk)
    return value[:16], value[16:]


def nonce(iv, sequence):
    return (int.from_bytes(iv, "big") ^ sequence).to_bytes(12, "big")


def open_record(key, iv, sequence, record):
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    header, data = record
    return AESGCM(key).decrypt(nonce(iv, sequence), data, sequence.to_bytes(8, "big") + header)


def seal_record(kind, plaintext, key, iv, sequence):
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    header = bytes.fromhex(kind) + (len(plaintext) + 16).to_bytes(2, "big")
    return header + AESGCM(key).encrypt(nonce(iv, sequence), plaintext,
                                     sequence.to_bytes(8, "big") + header)


def inner_plaintext(packet, key):
    """只接受认证通过的内层载荷，识别失败时不返回猜测明文。"""
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    aes = AESGCM(key)
    for offset in range(max(0, min(513, len(packet) - 28))):
        try:
            plain = aes.decrypt(packet[-28:-16], packet[offset:-28] + packet[-16:], None)
        except Exception:
            continue
        # 本次模板只支持从起点开始的完整 zlib 流。
        decoded = decompress(plain)
        return offset, plain, decoded
    raise ValueError("内层载荷未通过认证")


def protobuf_fields(data):
    """读取通用 protobuf 字段，无需私有 schema；不解释身份字段。"""
    def varint(offset):
        value = 0
        for shift in range(0, 70, 7):
            if offset >= len(data):
                raise ValueError("变长整数截断")
            byte = data[offset]
            offset += 1
            value |= (byte & 127) << shift
            if byte < 128:
                return value, offset
        raise ValueError("变长整数过长")

    offset, result = 0, []
    while offset < len(data):
        tag, offset = varint(offset)
        field, wire = tag >> 3, tag & 7
        if field == 0:
            raise ValueError("字段编号无效")
        if wire == 0:
            value, offset = varint(offset)
        elif wire in (1, 5):
            size = 8 if wire == 1 else 4
            value, offset = data[offset:offset + size], offset + size
            if len(value) != size:
                raise ValueError("固定字段截断")
        elif wire == 2:
            size, offset = varint(offset)
            value, offset = data[offset:offset + size], offset + size
            if len(value) != size:
                raise ValueError("长度字段截断")
        else:
            raise ValueError("不支持的字段类型")
        result.append((field, wire, value))
    return result


def _error_fields(data, depth=0):
    """递归收集错误结构里的整数与文本字段；结构不完整时返回空，不猜含义。"""
    rets, texts = [], []
    try:
        fields = protobuf_fields(data)
    except ValueError:
        return rets, texts
    for _, wire, value in fields:
        if wire == 0:
            rets.append(value)
        elif wire == 2 and depth < 4:
            sub_rets, sub_texts = _error_fields(value, depth + 1)
            rets.extend(sub_rets)
            texts.extend(sub_texts)
            try:
                texts.append(value.decode("utf-8"))
            except UnicodeDecodeError:
                pass
    return rets, texts


def session_timeout_error(data):
    """会话过期响应：认证已通过但没有列表载荷，服务器只回错误结构。

    只认明文里可解出的 session timeout 文本；返回码按补码还原后原样带回，不解释含义。
    """
    rets, texts = _error_fields(data)
    message = next((t for t in texts if "session timeout" in t.lower()), None)
    if message is None:
        return None

    def signed(value):
        return value - 2 ** 64 if value >= 2 ** 63 else value

    ret = next((v for v in map(signed, rets) if -(2 ** 31) <= v < 2 ** 31), None)
    return {"Ret": ret, "ErrMsg": message}


def json_objects(data, depth=0):
    if depth > 6:
        return []
    try:
        value = json.loads(data)
        if isinstance(value, dict):
            return [value]
    except (ValueError, UnicodeDecodeError):
        pass
    result = []
    try:
        fields = protobuf_fields(data)
    except ValueError:
        return result
    for _, wire, value in fields:
        if wire == 2:
            result.extend(json_objects(value, depth + 1))
    return result


def envelope(packet):
    if len(packet) < 12 or int.from_bytes(packet[:4], "big") != len(packet) - 4:
        raise ValueError("业务外封装长度无效")
    path_len = int.from_bytes(packet[4:6], "big")
    at = 6 + path_len
    path = packet[6:at].decode("ascii")
    host_len = int.from_bytes(packet[at:at + 2], "big")
    host = packet[at + 2:at + 2 + host_len].decode("ascii")
    at += 2 + host_len
    body_len = int.from_bytes(packet[at:at + 4], "big")
    at += 4
    if body_len != len(packet) - at:
        raise ValueError("业务内封装长度无效")
    return path, host, at


def payload_from_capture(state, candidates, captured_ts):
    raw_records = records(bytes.fromhex(state["post_body"]))
    if len(raw_records) != 4 or [x[0][:3].hex() for x in raw_records] != [
            "19f104", "19f104", "17f104", "15f104"]:
        raise ValueError("捕获不是完整的短连接恢复请求")
    ch = raw_records[0][1]
    key, iv = expand(bytes.fromhex(state["psk"]), b"early data key expansion", ch)
    hs = open_record(key, iv, 1, raw_records[1])
    alert = open_record(key, iv, 3, raw_records[3])
    if len(hs) != 20 or len(alert) != 7:
        raise ValueError("不支持的握手模板")
    # 对完整外层数据记录也执行认证，独立于第三方探针的就绪提示。
    open_record(key, iv, 2, raw_records[2])
    selected = None
    for key_hex in state.get("inner_enc_keys", [state["inner_enc_key"]]):
        for raw in candidates:
            packet = bytes.fromhex(raw)
            try:
                offset, compressed, decoded = inner_plaintext(packet, bytes.fromhex(key_hex))
            except (ValueError, zlib.error):
                continue
            objects = [v for v in json_objects(decoded) if "BizUserName" in v]
            if b"/cgi-bin/mmbiz-bin/bizattr/bizprofilev2h5" in decoded and len(objects) == 1:
                selected = packet, offset, compressed, decoded, objects[0], key_hex
                break
        if selected:
            break
    if selected is None:
        raise ValueError("没有通过认证与业务校验的列表模板")
    packet, offset, compressed, decoded, request, key_hex = selected
    path, host, start = envelope(packet)
    if not start < offset or packet[start] != 0xbf or packet[start + 1] >> 2 != offset - start:
        raise ValueError("内层头部不受支持")
    payload = {"psk": state["psk"], "inner_enc_key": key_hex,
               "inner_dec_key": state["inner_dec_key"], "ch_template": ch.hex(),
               "hs_template": hs.hex(), "alert_template": alert.hex(),
               "req_template": decoded.hex(), "inner_body_header": packet[start:offset].hex(),
               "host": host, "path": path,
               "extracted_at": datetime.fromtimestamp(captured_ts, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
               "extracted_ts": captured_ts}
    payload["inner_dec_candidates"] = state.get("inner_dec_keys", [state["inner_dec_key"]])
    return payload


def build_request(payload, compressed):
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    if decompress(compressed).hex() != payload["req_template"]:
        raise ValueError("压缩模板与明文模板不匹配")
    ch = bytearray.fromhex(payload["ch_template"])
    hs = bytearray.fromhex(payload["hs_template"])
    if len(ch) != 161 or ch[4:10] != bytes.fromhex("0104f10100a8") or len(hs) != 20:
        raise ValueError("当前只支持已验证的 Windows 短连接模板")
    if ch[42:46] != hs[16:20]:
        raise ValueError("握手时间戳不匹配")
    ch[10:42] = os.urandom(32)
    ch[42:46] = hs[16:20] = struct.pack(">I", int(time.time()))
    inner_nonce = os.urandom(12)
    cipher = AESGCM(bytes.fromhex(payload["inner_enc_key"])).encrypt(inner_nonce, compressed, None)
    inner = bytes.fromhex(payload["inner_body_header"]) + cipher[:-16] + inner_nonce + cipher[-16:]
    path, host = payload["path"].encode("ascii"), payload["host"].encode("ascii")
    body = struct.pack(">H", len(path)) + path + struct.pack(">H", len(host)) + host
    body += struct.pack(">I", len(inner)) + inner
    app = struct.pack(">I", len(body)) + body
    key, iv = expand(bytes.fromhex(payload["psk"]), b"early data key expansion", bytes(ch))
    data = bytes.fromhex("19f104") + len(ch).to_bytes(2, "big") + ch
    data += seal_record("19f104", bytes(hs), key, iv, 1)
    data += seal_record("17f104", app, key, iv, 2)
    data += seal_record("15f104", bytes.fromhex(payload["alert_template"]), key, iv, 3)
    # 新建请求先认证回解，防止将模板拼接错误发到服务器。
    parsed = records(data)
    if open_record(key, iv, 1, parsed[1]) != bytes(hs):
        raise ValueError("新建握手记录回解不匹配")
    check = open_record(key, iv, 2, parsed[2])
    if inner_plaintext(check, bytes.fromhex(payload["inner_enc_key"]))[2].hex() != payload["req_template"]:
        raise ValueError("新建业务记录回解不匹配")
    return data, bytes(ch)


def decode_response(data, ch, payload):
    parsed = records(data)
    if len(parsed) < 2 or parsed[0][0][:3].hex() != "16f104":
        raise ValueError("服务器未返回可验证的恢复握手")
    hs = bytearray.fromhex(payload["hs_template"])
    hs[16:20] = ch[42:46]
    # Windows 实况的握手摘要还包含客户端的加密扩展明文。
    key, iv = expand(bytes.fromhex(payload["psk"]), b"handshake key expansion",
                     ch + bytes(hs) + parsed[0][1])
    opened = [open_record(key, iv, seq, item) for seq, item in enumerate(parsed[1:], 1)]
    apps = [pt for pt, (header, _) in zip(opened, parsed[1:]) if header[0] == 0x17]
    if not apps:
        raise ValueError("已认证响应没有应用数据")
    joined = b"".join(apps)
    decoded = None
    keys = payload.get("inner_dec_candidates", [payload["inner_dec_key"]])
    if not isinstance(keys, list) or not 1 <= len(keys) <= 8:
        raise ValueError("接收密钥候选数量无效")
    for key_hex in keys:
        try:
            _, _, decoded = inner_plaintext(joined, bytes.fromhex(key_hex))
            payload["inner_dec_key"] = key_hex
            break
        except (ValueError, zlib.error):
            continue
    if decoded is None:
        raise ValueError("内层响应未通过认证")
    objects = [v for v in json_objects(decoded) if "BaseResponse" in v]
    if len(objects) != 1:
        error = session_timeout_error(decoded)
        if error is None:
            raise ValueError("已认证响应未包含唯一列表")
        # 会话过期是服务端的业务结论，不是协议故障；带回返回码供上层单独分类。
        return ({"authenticated_outer_records": len(opened), "inner_authenticated": True,
                 "business_ret": error["Ret"], "session_timeout": True},
                {"BaseResponse": error}, decoded)
    result = objects[0]
    request = [v for v in json_objects(bytes.fromhex(payload["req_template"])) if "BizUserName" in v][0]
    account = result.get("AccountInfo") or {}
    messages = (result.get("MsgList") or {}).get("Msg") or []
    summary = {"authenticated_outer_records": len(opened), "inner_authenticated": True,
               "business_ret": (result.get("BaseResponse") or {}).get("Ret"),
               "account_matches": account.get("UserName") == request["BizUserName"],
               "message_groups": len(messages),
               "article_details": sum(len((v.get("AppMsg") or {}).get("DetailInfo") or []) for v in messages)}
    summary["list_success"] = summary["business_ret"] == 0 and summary["account_matches"] and isinstance(result.get("MsgList"), dict)
    return summary, result, decoded


def varint(value):
    data = bytearray()
    while value >= 128:
        data.append((value & 127) | 128)
        value >>= 7
    return bytes(data + bytes([value]))


def request_parameters(payload):
    objects = [value for value in json_objects(bytes.fromhex(payload["req_template"]))
               if "BizUserName" in value]
    if len(objects) != 1 or not isinstance(objects[0]["BizUserName"], str):
        raise ValueError("请求没有唯一公众号身份")
    return objects[0]


def validate_payload(payload):
    lengths = {"psk": 32, "inner_enc_key": 24, "inner_dec_key": 24,
               "ch_template": 161, "hs_template": 20, "alert_template": 7}
    for name, length in lengths.items():
        if len(bytes.fromhex(payload[name])) != length:
            raise ValueError("会话材料长度无效")
    if not re.fullmatch(r"[a-z0-9.-]+\.weixin\.qq\.com", payload["host"]) or payload["path"] != \
            "/cgi-bin/micromsg-bin/transfer/bizh5exttransfer":
        raise ValueError("会话路由未通过校验")
    if len(bytes.fromhex(payload["req_template"])) > 65536:
        raise ValueError("请求模板超过上限")
    header = bytes.fromhex(payload["inner_body_header"])
    if len(header) < 12 or header[0] != 0xbf or header[1] >> 2 != len(header) or header[2] >> 4 != 13:
        raise ValueError("只支持已验证的 GCM 业务包头")
    request_parameters(payload)


def page_payload(original, cursor=None):
    """保留未知 protobuf 字段，只修改列表 JSON 和对应包头长度。"""
    if cursor is None:
        return original, zlib.compress(bytes.fromhex(original["req_template"]))
    if not isinstance(cursor, str) or not cursor or len(cursor) > 32768:
        raise ValueError("翻页游标无效")
    def rewrite(data, depth=0):
        if depth > 6:
            return data, 0
        try:
            obj = json.loads(data)
        except (ValueError, UnicodeDecodeError):
            obj = None
        if isinstance(obj, dict) and "BizUserName" in obj:
            obj.update(ActionType=1, Offset=cursor)
            return json.dumps(obj, ensure_ascii=False, separators=(",", ":")).encode(), 1
        try:
            fields = protobuf_fields(data)
        except ValueError:
            return data, 0
        result, changed = bytearray(), 0
        for field, wire, value in fields:
            if wire == 2:
                value, count = rewrite(value, depth + 1)
                changed += count
            result += varint((field << 3) | wire)
            if wire == 0:
                result += varint(value)
            elif wire == 2:
                result += varint(len(value)) + value
            else:
                result += value
        return (bytes(result), changed) if changed else (data, 0)
    data, changed = rewrite(bytes.fromhex(original["req_template"]))
    if changed != 1:
        raise ValueError("无法唯一修改列表参数")
    header = bytes.fromhex(original["inner_body_header"])
    offset, slots = 11 + (header[2] & 15), []
    for index in range(6):
        begin = offset
        while offset < len(header) and header[offset] & 128:
            offset += 1
            if offset - begin > 9:
                raise ValueError("包头变长整数过长")
        if offset >= len(header):
            raise ValueError("包头字段不完整")
        offset += 1
        slots.append(varint(len(data)) if index in (1, 2) else header[begin:offset])
    fixed = header[:11 + (header[2] & 15)] + b"".join(slots) + header[offset:]
    if len(fixed) > 63:
        raise ValueError("包头超过长度上限")
    fixed = fixed[:1] + bytes([(len(fixed) << 2) | (header[1] & 3)]) + fixed[2:]
    result = {**original, "req_template": bytes(data).hex(), "inner_body_header": fixed.hex()}
    return result, zlib.compress(data)


class HttpFailure(ValueError):
    """仅记录状态码，不把错误页面当成可解密业务响应。"""
    def __init__(self, status):
        super().__init__("会话接口传输异常")
        self.status = status


def fetch_page(payload, cursor=None, timeout=45, connection_factory=None, evidence=None):
    validate_payload(payload)
    updated, compressed = page_payload(payload, cursor)
    data, ch = build_request(updated, compressed)
    factory = connection_factory or http.client.HTTPConnection
    connection = factory(payload["host"], 80, timeout=timeout)
    try:
        connection.request("POST", "/mmtls/" + os.urandom(4).hex(), body=data,
                           headers={"Content-Type": "application/octet-stream", "Connection": "close"})
        response = connection.getresponse()
        if response.status != 200:
            raise HttpFailure(response.status)
        raw = response.read(MAX_PAYLOAD + 1)
        if len(raw) > MAX_PAYLOAD:
            raise ValueError("响应超过上限")
        if evidence is not None:
            evidence["wire_response_hex"] = raw.hex()
        return decode_response(raw, ch, updated)[:2]
    finally:
        connection.close()
