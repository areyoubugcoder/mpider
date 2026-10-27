"""使用虚构密钥验证协议认证、响应身份与单次请求预算；不连接网络。"""

import json
import tempfile
import unittest
import zlib
from pathlib import Path
from unittest.mock import patch

from cryptography.hazmat.primitives.ciphers.aead import AESGCM

from session_protocol import (build_request, decode_response, expand,
                            private_write, records, seal_record)



def protobuf(value):
    data = json.dumps(value).encode()
    size, varint = len(data), bytearray()
    while size >= 128:
        varint.append((size & 127) | 128)
        size >>= 7
    varint.append(size)
    return b"\x0a" + varint + data


def fixture():
    ch = bytearray(161)
    ch[:10] = bytes.fromhex("0000009d0104f10100a8")
    ch[42:46] = b"TIME"
    from session_protocol import varint
    nested = protobuf({"BizUserName": "synthetic-account", "ActionType": 0})
    request = b"\x12" + varint(len(nested)) + nested + b"\x3a\x0dunknown-field"
    prefix = b"\xbf\x00\xd0" + bytes(8)
    rest = varint(15158) + varint(len(request))*2 + b"\x00\x02" + varint(123456) + b"\xff" + bytes(9)
    header = prefix[:1] + bytes([((len(prefix)+len(rest)) << 2) | 2]) + prefix[2:] + rest
    return {"psk": bytes(range(32)).hex(), "inner_enc_key": bytes(range(24)).hex(),
            "inner_dec_key": bytes(range(24, 48)).hex(), "ch_template": ch.hex(),
            "hs_template": (bytes(16) + b"TIME").hex(), "alert_template": bytes(7).hex(),
            "req_template": request.hex(), "inner_body_header": header.hex(),
            "host": "extshort.weixin.qq.com",
            "path": "/cgi-bin/micromsg-bin/transfer/bizh5exttransfer"}, zlib.compress(request)


def server_response(payload, ch, account="synthetic-account", corrupt_inner=False, raw=None):
    value = {"BaseResponse": {"Ret": 0}, "AccountInfo": {"UserName": account},
             "MsgList": {"Msg": [{"AppMsg": {"DetailInfo": [{}, {}]}}]}}
    plain = zlib.compress(raw if raw is not None else protobuf(value))
    iv = bytes(range(12))
    cipher = AESGCM(bytes.fromhex(payload["inner_dec_key"])).encrypt(iv, plain, None)
    inner = bytes.fromhex(payload["inner_body_header"]) + cipher[:-16] + iv + cipher[-16:]
    if corrupt_inner:
        inner = inner[:-1] + bytes([inner[-1] ^ 1])
    sh = b"synthetic-server-hello"
    hs = bytearray.fromhex(payload["hs_template"])
    hs[16:20] = ch[42:46]
    key, outer_iv = expand(bytes.fromhex(payload["psk"]), b"handshake key expansion", ch + bytes(hs) + sh)
    half = len(inner) // 2
    return (bytes.fromhex("16f104") + len(sh).to_bytes(2, "big") + sh
            + seal_record("16f104", b"synthetic-finished", key, outer_iv, 1)
            + seal_record("17f104", inner[:half], key, outer_iv, 2)
            + seal_record("17f104", inner[half:], key, outer_iv, 3)
            + seal_record("15f104", bytes(7), key, outer_iv, 4))


class ProtocolTests(unittest.TestCase):
    def test_fresh_request_and_authenticated_response(self):
        payload, compressed = fixture()
        first, ch = build_request(payload, compressed)
        second, other_ch = build_request(payload, compressed)
        self.assertNotEqual(first, second)
        self.assertNotEqual(ch[10:42], other_ch[10:42])
        result, _, _ = decode_response(server_response(payload, ch), ch, payload)
        self.assertTrue(result["list_success"])
        self.assertEqual(result["article_details"], 2)
        self.assertEqual(result["authenticated_outer_records"], 4)

    def test_bad_outer_tag_is_rejected(self):
        payload, compressed = fixture()
        _, ch = build_request(payload, compressed)
        data = server_response(payload, ch)
        with self.assertRaises(Exception):
            decode_response(data[:-1] + bytes([data[-1] ^ 1]), ch, payload)

    def test_bad_inner_tag_is_rejected(self):
        payload, compressed = fixture()
        _, ch = build_request(payload, compressed)
        with self.assertRaises(ValueError):
            decode_response(server_response(payload, ch, corrupt_inner=True), ch, payload)

    def test_wrong_account_is_not_success(self):
        payload, compressed = fixture()
        _, ch = build_request(payload, compressed)
        result, _, _ = decode_response(server_response(payload, ch, account="another"), ch, payload)
        self.assertFalse(result["list_success"])

    def test_session_timeout_is_business_error_not_protocol_failure(self):
        # 真实服务端在会话过期时返回纯 protobuf 错误结构，没有 JSON 列表载荷。
        payload, compressed = fixture()
        _, ch = build_request(payload, compressed)
        raw = (b"\n!\x08\xfa\xfd\xff\xff\xff\xff\xff\xff\xff\x01\x12\x14\n\x12H5 Session Timeout"
               b"\x12\x1c\x08\xf8\xfd\xff\xff\xff\xff\xff\xff\xff\x01\x12\x0fsession timeout")
        result, body, _ = decode_response(server_response(payload, ch, raw=raw), ch, payload)
        self.assertTrue(result["inner_authenticated"])
        self.assertTrue(result["session_timeout"])
        self.assertEqual(body["BaseResponse"]["ErrMsg"], "H5 Session Timeout")
        self.assertTrue(-2 ** 31 <= body["BaseResponse"]["Ret"] < 2 ** 31)

    def test_unknown_error_payload_without_session_timeout_still_rejected(self):
        payload, compressed = fixture()
        _, ch = build_request(payload, compressed)
        with self.assertRaises(ValueError):
            decode_response(server_response(payload, ch, raw=b"\x08\x01not-a-known-error"), ch, payload)

    def test_mutation_preserves_unknown_fields_and_sets_cursor(self):
        from session_protocol import page_payload, request_parameters, protobuf_fields
        payload, _ = fixture()
        changed, compressed = page_payload(payload, "synthetic-next")
        self.assertEqual(request_parameters(changed)["ActionType"], 1)
        self.assertEqual(request_parameters(changed)["Offset"], "synthetic-next")
        self.assertEqual(changed["psk"], payload["psk"])
        self.assertEqual(zlib.decompress(compressed).hex(), changed["req_template"])
        fields = protobuf_fields(bytes.fromhex(changed["req_template"]))
        self.assertIn((7, 2, b"unknown-field"), fields)

    def test_real_http_client_and_no_retry(self):
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
        import http.client
        import threading
        from session_protocol import fetch_page, records, expand, open_record, inner_plaintext
        payload, _ = fixture()
        observed = []
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_): pass
            def do_POST(self):
                raw = self.rfile.read(int(self.headers["Content-Length"]))
                items = records(raw); ch = items[0][1]
                key, iv = expand(bytes.fromhex(payload["psk"]), b"early data key expansion", ch)
                _, _, decoded = inner_plaintext(open_record(key, iv, 2, items[2]), bytes.fromhex(payload["inner_enc_key"]))
                from session_protocol import json_objects
                observed.extend(v for v in json_objects(decoded) if "BizUserName" in v)
                response = server_response(payload, ch)
                self.send_response(200);self.send_header("Content-Length", str(len(response)));self.end_headers();self.wfile.write(response)
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True);thread.start()
        def factory(_host, _port, timeout):
            return http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=timeout)
        try:
            checks, _ = fetch_page(payload, "synthetic-next", connection_factory=factory)
            self.assertTrue(checks["list_success"])
            self.assertEqual(len(observed), 1)
            self.assertEqual(observed[0]["Offset"], "synthetic-next")
        finally:
            server.shutdown();server.server_close();thread.join()

if __name__ == "__main__":
    unittest.main()
