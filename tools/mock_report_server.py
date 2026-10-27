#!/usr/bin/env python3
"""本地联调用的上报接收端：收 mpider 的 POST，打印并追加到 jsonl。

用法：
    python3 tools/mock_report_server.py --port 9000 [--token xxx] [--out reports.jsonl]

然后在 GUI「系统设置 → 数据上报」填 http://127.0.0.1:9000/report（token 可选）。
任何路径都收；token 给了就校验 Authorization: Bearer <token>，不对回 401。
"""

import argparse
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=9000)
    ap.add_argument("--token", default="")
    ap.add_argument("--out", default="")
    args = ap.parse_args()

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self) -> None:  # noqa: N802
            if args.token:
                auth = self.headers.get("Authorization", "")
                if auth != f"Bearer {args.token}":
                    self.send_response(401)
                    self.end_headers()
                    return
            n = int(self.headers.get("Content-Length") or 0)
            raw = self.rfile.read(n)
            try:
                body = json.loads(raw.decode("utf-8"))
            except Exception:  # noqa: BLE001
                self.send_response(400)
                self.end_headers()
                return
            line = json.dumps(body, ensure_ascii=False)
            print(f"[{self.path}] {line}", flush=True)
            if args.out:
                with open(args.out, "a", encoding="utf-8") as f:
                    f.write(line + "\n")
            self.send_response(204)
            self.end_headers()

        def log_message(self, *_a) -> None:  # 静音默认访问日志
            return

    srv = HTTPServer(("127.0.0.1", args.port), Handler)
    print(f"mock report server on http://127.0.0.1:{args.port}/ (token={'yes' if args.token else 'no'})", file=sys.stderr)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
