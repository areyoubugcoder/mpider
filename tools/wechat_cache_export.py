#!/usr/bin/env python3
"""从 dfindexeddb 的 JSONL 离线导出公众号 profileData 缓存，不访问微信数据库原件。"""

import argparse
import html
import json
import os
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import parse_qs, urlparse


def cached_profiles(rows):
    """限定 profileData/bizProfile，按 LevelDB 序号保留每个键的最后记录。"""
    databases = {}
    stores = {}
    latest = {}
    def without_offsets(value):
        if isinstance(value, dict):
            return {k: without_offsets(v) for k, v in value.items() if k != "offset"}
        if isinstance(value, list):
            return [without_offsets(v) for v in value]
        return value

    for row in sorted(rows, key=lambda r: r.get("sequence_number", -1)):
        key = row.get("key") or {}
        prefix = key.get("key_prefix") or {}
        kind = key.get("__type__")
        if kind == "DatabaseNameKey" and key.get("database_name") == "profileData":
            databases[row.get("value")] = True
        elif kind == "ObjectStoreMetaDataKey" and key.get("metadata_type") == 0:
            stores[(prefix.get("database_id"), key.get("object_store_id"))] = row.get("value")
        elif kind == "ObjectStoreDataKey":
            identity = json.dumps(without_offsets([prefix, key.get("encoded_user_key")]), sort_keys=True)
            if row.get("sequence_number", -1) >= latest.get(identity, {}).get("sequence_number", -1):
                latest[identity] = row
    result = []
    for row in latest.values():
        if row.get("type") != 1:
            continue
        key = row["key"]["key_prefix"]
        db, store = key.get("database_id"), key.get("object_store_id")
        if not databases.get(db) or stores.get((db, store)) != "bizProfile":
            continue
        value = row.get("value") or {}
        value = value.get("value") or {}
        if not isinstance(value, dict) or not isinstance(value.get("data"), str):
            continue
        try:
            profile = json.loads(value["data"])
        except (json.JSONDecodeError, TypeError):
            continue
        if not isinstance(profile, dict) or not isinstance(profile.get("MsgList"), dict):
            continue
        result.append({"profile": profile, "sequence": row.get("sequence_number"),
                       "user_name": value.get("name")})
    return result


def articles_from_profile(profile):
    """展开一组消息中的多篇图文，保留完整链接及服务端发布时间。"""
    articles = {}
    lists = profile.get("MsgList") or {}
    for message in lists.get("Msg", []) + lists.get("FeaturedList", []):
        app = message.get("AppMsg") or {}
        timestamp = (app.get("BaseInfo") or {}).get("CreateTime") or (message.get("BaseInfo") or {}).get("DateTime")
        for detail in app.get("DetailInfo", []):
            url = html.unescape(detail.get("ContentUrl") or "")
            parsed = urlparse(url)
            if parsed.hostname != "mp.weixin.qq.com" or parsed.path != "/s":
                continue
            query = parse_qs(parsed.query)
            identity = tuple(query.get(name, [""])[0] for name in ("__biz", "mid", "idx"))
            if not all(identity):
                continue
            articles[identity] = {"biz": identity[0], "mid": identity[1], "idx": identity[2],
                                  "title": detail.get("Title", ""), "published_at": timestamp,
                                  "url": url}
    return list(articles.values())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("records", nargs="+", type=Path, help="dfindexeddb db -o jsonl 的输出文件")
    parser.add_argument("--out", required=True, type=Path, help="仅本机保存的文章与游标文件")
    args = parser.parse_args()
    output = []
    for path in args.records:
        rows = [json.loads(line) for line in path.read_text(encoding="utf-8-sig").splitlines() if line.strip()]
        for item in cached_profiles(rows):
            profile = item["profile"]
            output.append({"source": path.name, "sequence": item["sequence"],
                           "user_name": item["user_name"], "account": profile.get("AccountInfo"),
                           "cached_at": profile.get("updateTime"),
                           "base_response": profile.get("BaseResponse"),
                           "paging": profile["MsgList"].get("PagingInfo"),
                           "articles": articles_from_profile(profile)})
    args.out.parent.mkdir(parents=True, exist_ok=True)
    # 内容可能带有页面会话参数；不把 URL 或游标写入终端。
    fd = os.open(args.out, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    try:
        if os.name != "nt":
            os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            fd = -1
            json.dump({"exported_at": datetime.now(timezone.utc).isoformat(), "profiles": output},
                      handle, ensure_ascii=False, indent=2)
    finally:
        if fd != -1:
            os.close(fd)
    print(json.dumps({"公众号缓存数": len(output), "文章数": sum(len(p["articles"]) for p in output)}, ensure_ascii=False))


if __name__ == "__main__":
    main()
