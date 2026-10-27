#!/usr/bin/env python3
"""离线检查微信请求配置；仅输出字段、长度与比较结果，不输出凭证。"""

import argparse
import hashlib
import json
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import parse_qs, unquote, urlsplit


def summarize(path):
    raw = Path(path).read_bytes()
    profile = json.loads(raw.decode("utf-8-sig"))
    url = urlsplit(profile.get("RequestUrl", ""))
    query = parse_qs(url.query, keep_blank_values=True)
    cookies = {}
    for part in profile.get("Cookie", "").split(";"):
        name, sep, value = part.strip().partition("=")
        if sep:
            cookies[name] = value
    captured = profile.get("CaptureTime")
    age = None
    if captured:
        try:
            dt = datetime.fromisoformat(captured.replace("Z", "+00:00"))
            if dt.tzinfo:
                age = round((datetime.now(timezone.utc) - dt).total_seconds())
        except (TypeError, ValueError):
            pass
    ticket = query.get("pass_ticket", [""])[0]
    cookie_ticket = cookies.get("pass_ticket", "")
    return {
        "sha256": hashlib.sha256(raw).hexdigest(),
        "capture_time": captured,
        "age_seconds": age,
        "request_path": url.path,
        "query_fields": sorted(query),
        "cookie_fields": sorted(cookies),
        "credentials": {
            "query": {k: {"present": bool(query.get(k, [""])[0]),
                           "length": len(query.get(k, [""])[0])}
                      for k in ("key", "uin", "pass_ticket", "exportkey")},
            "cookie": {k: {"present": bool(cookies.get(k)),
                            "length": len(cookies.get(k, ""))}
                       for k in ("appmsg_token", "wap_sid2", "wxtokenkey", "pass_ticket")},
        },
        "pass_ticket_comparison": {
            "both_present": bool(ticket and cookie_ticket),
            "equal": bool(ticket and cookie_ticket) and ticket == cookie_ticket,
            "equal_after_cookie_url_decode": bool(ticket and cookie_ticket)
            and ticket == unquote(cookie_ticket),
        },
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("profiles", nargs="+")
    args = parser.parse_args()
    for filename in args.profiles:
        print(json.dumps(summarize(filename), ensure_ascii=False, indent=2))
