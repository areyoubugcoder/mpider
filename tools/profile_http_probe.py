#!/usr/bin/env python3
"""串行比较登录文章、公开文章和主页；不发送 getmsg 请求，不打印凭证。"""

import argparse
import hashlib
import json
import ssl
from datetime import datetime, timezone
from pathlib import Path
from urllib.error import HTTPError
from urllib.parse import parse_qsl, urlencode, urlsplit
from urllib.request import HTTPSHandler, HTTPRedirectHandler, ProxyHandler, Request, build_opener


class SameHostRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        redirect = urlsplit(newurl)
        if redirect.scheme != "https" or redirect.hostname != "mp.weixin.qq.com":
            raise ValueError("重定向离开目标主机，已停止")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("profile")
    parser.add_argument("out")
    parser.add_argument("--only", nargs="+", help="只运行指定对照项", choices=(
        "article_authenticated", "article_public", "article_public_same_ua", "article_public_generic_ua",
        "article_url_only", "profile_home"))
    args = parser.parse_args()
    profile = json.loads(Path(args.profile).read_text(encoding="utf-8-sig"))
    if profile.get("Host") != "mp.weixin.qq.com":
        raise ValueError("目标主机不匹配")
    url = urlsplit(profile["RequestUrl"])
    if url.path != "/s" or (url.netloc and url.hostname != "mp.weixin.qq.com"):
        raise ValueError("不是预期的文章请求")
    captured = datetime.fromisoformat(profile["CaptureTime"].replace("Z", "+00:00"))
    age = (datetime.now(timezone.utc) - captured).total_seconds()
    auth_cases = {"article_authenticated", "article_url_only", "profile_home"}
    if not 0 <= age < 1800 and (not args.only or auth_cases.intersection(args.only)):
        raise ValueError("拒绝以超过 30 分钟的 profile 做新凭证验证")
    query = dict(parse_qsl(url.query))
    public_query = {key: value for key, value in query.items()
                    if key in ("__biz", "mid", "idx", "sn", "chksm")}
    auth_headers = {"User-Agent": profile["UserAgent"], "Cookie": profile.get("Cookie", ""),
                    "Accept": profile.get("Accept", "*/*"),
                    "Accept-Language": profile.get("AcceptLanguage", "zh-CN")}
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True, mode=0o700)
    opener = build_opener(ProxyHandler({}), SameHostRedirect(),
                          HTTPSHandler(context=ssl.create_default_context()))
    cases = [
        ("article_authenticated", "https://mp.weixin.qq.com/s?" + url.query, auth_headers),
        ("article_public", "https://mp.weixin.qq.com/s?" + urlencode(public_query),
         {"User-Agent": "Mozilla/5.0"}),
        ("article_public_same_ua", "https://mp.weixin.qq.com/s?" + urlencode(public_query),
         {key: value for key, value in auth_headers.items() if key != "Cookie"}),
        ("article_public_generic_ua", "https://mp.weixin.qq.com/s?" + urlencode(public_query),
         {**{key: value for key, value in auth_headers.items() if key != "Cookie"},
          "User-Agent": "Mozilla/5.0"}),
        ("article_url_only", "https://mp.weixin.qq.com/s?" + url.query,
         {key: value for key, value in auth_headers.items() if key != "Cookie"}),
        ("profile_home", "https://mp.weixin.qq.com/mp/profile_ext?" + urlencode({
            "action": "home", "__biz": query["__biz"], "scene": "124",
            **{key: query[key] for key in ("uin", "key", "pass_ticket") if key in query},
        }), auth_headers),
    ]
    for label, target, headers in cases:
        if args.only and label not in args.only:
            continue
        try:
            try:
                response = opener.open(Request(target, headers=headers), timeout=25)
            except HTTPError as error:
                response = error
            with response:
                body = response.read()
                status = response.status
            text = body.decode("utf-8", errors="replace")
            dest = out / (label + ".html")
            dest.write_bytes(body)
            dest.chmod(0o600)
            result = {"case": label, "http_status": status, "bytes": len(body),
                      "has_article_content": 'id="js_content"' in text,
                      "has_article_title": 'id="activity-name"' in text,
                      "has_msg_list_variable": "var msgList" in text,
                      "sha256": hashlib.sha256(body).hexdigest()}
        except Exception as error:
            result = {"case": label, "error_type": type(error).__name__}
        print(json.dumps(result, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
