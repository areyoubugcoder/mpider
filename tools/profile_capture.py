"""受控采样公众号请求：凭证仅落本机指定目录，终端日志只包含摘要。"""

import json
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

from mitmproxy import ctx, http


class ProfileCapture:
    def load(self, loader):
        loader.add_option("profile_out", str, "", "本机凭证与响应采样目录")

    def request(self, flow: http.HTTPFlow):
        if flow.request.pretty_host != "mp.weixin.qq.com":
            return
        if not ctx.options.profile_out:
            return
        out = Path(ctx.options.profile_out)
        out.mkdir(parents=True, exist_ok=True, mode=0o700)
        req = flow.request
        parsed = urlsplit(req.url)
        query = parse_qs(parsed.query)
        stamp = datetime.now(timezone.utc).isoformat()
        event = {"time": stamp, "event": "request", "path": parsed.path,
                 "method": req.method, "query_fields": sorted(query)}
        self.record(out, event)
        if parsed.path == "/s" and query.get("key"):
            profile = {
                "Host": req.pretty_host,
                "UserAgent": req.headers.get("User-Agent", ""),
                "Accept": req.headers.get("Accept", ""),
                "AcceptLanguage": req.headers.get("Accept-Language", ""),
                "AcceptEncoding": req.headers.get("Accept-Encoding", ""),
                "AcceptLanguageName": "Accept-Language",
                "AcceptEncodingName": "Accept-Encoding",
                # HTTP/2 可将 Cookie 拆为多个头，必须以分号合并，不能使用逗号折叠。
                "Cookie": "; ".join(req.headers.get_all("Cookie")),
                "RequestUrl": req.path,
                "CaptureTime": stamp,
            }
            dest = out / "wechat_profile.json"
            dest.write_text(json.dumps(profile, ensure_ascii=False, indent=2))
            dest.chmod(0o600)
            snapshot = out / f"profile-{flow.id}.json"
            snapshot.write_text(json.dumps(profile, ensure_ascii=False, indent=2))
            snapshot.chmod(0o600)

    def response(self, flow: http.HTTPFlow):
        if flow.request.pretty_host != "mp.weixin.qq.com" or not ctx.options.profile_out:
            return
        out = Path(ctx.options.profile_out)
        out.mkdir(parents=True, exist_ok=True, mode=0o700)
        parsed = urlsplit(flow.request.url)
        data = flow.response.content or b""
        event = {"time": datetime.now(timezone.utc).isoformat(), "event": "response",
                 "path": parsed.path, "status": flow.response.status_code,
                 "bytes": len(data), "content_type": flow.response.headers.get("content-type", "")}
        event["set_cookie_fields"] = [line.partition("=")[0]
                                      for line in flow.response.headers.get_all("Set-Cookie")]
        if parsed.path in ("/s", "/mp/profile_ext", "/mp/getappmsgext", "/mp/getappmsglist"):
            name = f"response-{flow.id}.bin"
            dest = out / name
            dest.write_bytes(data)
            dest.chmod(0o600)
            event["body_file"] = name
        self.record(out, event)

    @staticmethod
    def record(out, event):
        dest = out / "events.jsonl"
        with dest.open("a") as handle:
            handle.write(json.dumps(event, ensure_ascii=False) + "\n")
        dest.chmod(0o600)


addons = [ProfileCapture()]
