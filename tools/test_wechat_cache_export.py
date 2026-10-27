"""验证缓存版本选择、删除记录和多篇图文展开，夹具全部为假数据。"""

import copy
import json
import unittest

from wechat_cache_export import articles_from_profile, cached_profiles


def metadata(store="bizProfile"):
    return [
        {"sequence_number": 1, "key": {"__type__": "DatabaseNameKey", "database_name": "profileData"}, "value": 1},
        {"sequence_number": 2, "key": {"__type__": "ObjectStoreMetaDataKey", "metadata_type": 0,
                                     "object_store_id": 1, "key_prefix": {"database_id": 1}}, "value": store},
    ]


def record(sequence, title="示例", deleted=False):
    profile = {"MsgList": {"Msg": []}, "title": title}
    return {"sequence_number": sequence, "type": 0 if deleted else 1,
            "key": {"__type__": "ObjectStoreDataKey", "key_prefix": {"database_id": 1,
                    "object_store_id": 1, "index_id": 1, "offset": sequence * 10},
                    "encoded_user_key": {"offset": sequence * 20, "value": "虚构公众号"}},
            "value": {"value": {"name": "虚构公众号", "data": json.dumps(profile)}}}


class CacheExportTests(unittest.TestCase):
    def test_latest_revision_ignores_physical_offsets_and_input_order(self):
        rows = metadata() + [record(9, "更新版"), record(3, "旧版")]
        profiles = cached_profiles(rows)
        self.assertEqual(len(profiles), 1)
        self.assertEqual(profiles[0]["profile"]["title"], "更新版")

    def test_deleted_record_does_not_resurrect_cached_articles(self):
        self.assertEqual(cached_profiles(metadata() + [record(3), record(9, deleted=True)]), [])

    def test_other_object_stores_are_ignored(self):
        self.assertEqual(cached_profiles(metadata("其他缓存") + [record(3)]), [])

    def test_multi_article_featured_deduplication_and_publish_time(self):
        def article(index):
            return {"Title": "示例" + str(index), "ContentUrl":
                    "https://mp.weixin.qq.com/s?__biz=fake&amp;mid=123&amp;idx=" + str(index) + "&amp;sn=fake#rd"}

        message = {"BaseInfo": {"DateTime": 200}, "AppMsg": {"BaseInfo": {"CreateTime": 100},
                   "DetailInfo": [article(1), article(2), {"ContentUrl": "https://example.com/s"}]}}
        profile = {"MsgList": {"Msg": [message], "FeaturedList": [copy.deepcopy(message)]}}
        articles = articles_from_profile(profile)
        self.assertEqual(len(articles), 2)
        self.assertEqual({a["idx"] for a in articles}, {"1", "2"})
        self.assertTrue(all(a["published_at"] == 100 for a in articles))
        self.assertTrue(all("&amp;" not in a["url"] and a["url"].endswith("#rd") for a in articles))


if __name__ == "__main__":
    unittest.main()
