//! Read article interactions with the reader's ephemeral, article-bound session.
use super::{
    article_session,
    comments::{self, ArticleComment},
    interactions::ArticleInteractions,
};
use crate::{model::ArticleRow, Store};
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use std::{collections::HashSet, time::Duration};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Collection {
    Interactions,
    Comments,
}
impl Collection {
    fn purpose(self) -> &'static str {
        match self {
            Self::Interactions => "article_interactions",
            Self::Comments => "article_comments",
        }
    }
}

/// Display recovery steps at every article entry point, including batch failures.
pub(super) fn access_error(store: &Store, article: &ArticleRow, reason: &str) -> anyhow::Error {
    let nickname = store
        .wb_source(&article.biz)
        .ok()
        .flatten()
        .map(|source| source.nickname)
        .unwrap_or_default();
    let publisher = if nickname.trim().is_empty() {
        "该公众号".to_owned()
    } else {
        format!("「{}」公众号", nickname.trim())
    };
    anyhow::anyhow!("{reason}。请在专机微信中打开{publisher}的一篇文章，等待页面加载完成后，回到 MPider 重试；如果文章已打开，请刷新或重新进入。仍无法获取时，请打开当前要采集的文章后重试。")
}

const LIMIT: usize = 100;
const MAX_PAGES: usize = 100;
fn number(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str()?.parse().ok())
        .filter(|n| *n >= 0)
}
fn text(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::to_owned)
        .or_else(|| v.as_u64().map(|n| n.to_string()))
}
fn literal(html: &str, name: &str) -> Option<String> {
    let re = regex::Regex::new(&format!(
        r#"(?:var\s+|window\.){}\s*=\s*["']([^"'\r\n]*)["']"#,
        regex::escape(name)
    ))
    .ok()?;
    let values: HashSet<_> = re
        .captures_iter(html)
        .map(|m| m[1].to_owned())
        .filter(|value| !value.is_empty())
        .collect();
    (values.len() == 1).then(|| values.into_iter().next().unwrap())
}
pub(super) fn stats(v: &Value) -> Result<ArticleInteractions> {
    let s = v.get("appmsgstat").context("微信未返回互动统计")?;
    ensure!(
        s.get("ret").and_then(number) == Some(0)
            && s.get("is_login").and_then(Value::as_bool) == Some(true),
        "微信文章会话已失效"
    );
    let count = |key: &str| s.get(key).and_then(number);
    let raw = count("read_num");
    let real = count("real_read_num").filter(|n| *n > 0);
    let capped = real.is_none() && raw.is_some_and(|n| n >= 100001);
    Ok(ArticleInteractions {
        read_count: if capped { None } else { real.or(raw) },
        read_display: capped.then(|| "10万+".into()),
        like_count: count("old_like_num"),
        watching_count: count("like_num"),
        share_count: count("share_num"),
        comment_count: v.get("comment_count").and_then(number),
    })
}
async fn json(
    store: &Store,
    req: reqwest::RequestBuilder,
    article: &ArticleRow,
    budget: &mut usize,
    purpose: &str,
) -> Result<Value> {
    ensure!(*budget > 0, "本次补采达到请求上限，已保留已采集结果");
    *budget -= 1;
    let bytes = comments::send_request(store, req, article, 4 * 1024 * 1024, true, purpose).await?;
    serde_json::from_slice(&bytes)
        .map_err(|_| access_error(store, article, "微信没有返回可读取的数据"))
}
fn buffer(v: &Value) -> Result<String> {
    let b = v.get("buffer").and_then(Value::as_str).unwrap_or("");
    ensure!(b.len() <= 65536, "微信分页标识过长");
    Ok(b.into())
}

pub(super) async fn collect(store: &Store, id: i64, collection: Collection) -> Result<()> {
    let _lock = comments::FETCH
        .try_lock()
        .map_err(|_| anyhow::anyhow!("正在获取文章数据，请稍后再试"))?;
    let (article, _) = store.get_article_md(id)?.context("文章不存在")?;
    ensure!(article.is_deleted == 0, "文章已删除");
    let mid = article.mid.clone().context("文章缺少消息 ID")?;
    let idx = article.idx.context("文章缺少序号")?;
    let biz = article.biz.clone();
    let scan_mid = mid.clone();
    let link = article.content_url.clone().context("文章缺少原文链接")?;
    let session =
        tokio::task::spawn_blocking(move || article_session::discover(&biz, &scan_mid, idx, &link))
            .await
            .context("读取微信文章会话失败")?
            .map_err(|error| access_error(store, &article, &error.to_string()))?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(18))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let target = attempt.url();
            let article_path = |u: &url::Url| u.path() == "/s" || u.path().starts_with("/s/");
            if target.scheme() == "https"
                && target.host_str() == Some("mp.weixin.qq.com")
                && target.username().is_empty()
                && target.password().is_none()
                && target.port().is_none_or(|p| p == 443)
                && article_path(target)
                && attempt.previous().first().is_some_and(article_path)
                && attempt.previous().len() < 4
            {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .user_agent(article_session::USER_AGENT)
        .build()?;
    let html = comments::send_request(
        store,
        client.get(session.page.clone()),
        &article,
        6 * 1024 * 1024,
        false,
        collection.purpose(),
    )
    .await?;
    let html = std::str::from_utf8(&html).context("微信文章编码异常")?;
    if literal(html, "biz").as_deref() != Some(article.biz.as_str())
        || literal(html, "mid").as_deref() != Some(mid.as_str())
        || literal(html, "idx").and_then(|s| s.parse::<i64>().ok()) != Some(idx)
    {
        return Err(access_error(
            store,
            &article,
            "微信未返回可核对身份的文章页面",
        ));
    }
    let token = session
        .fields
        .get("appmsg_token")
        .cloned()
        .or_else(|| literal(html, "appmsg_token"))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| access_error(store, &article, "微信未提供文章访问令牌"))?;
    let comment_id =
        comments::id_from_html(html).or_else(|| session.fields.get("comment_id").cloned());
    let mut budget = 100usize;
    if collection == Collection::Interactions {
        let stat_url = session.endpoint("/mp/getappmsgext", &article.biz, &token);
        let mut form = vec![
            ("__biz", article.biz.clone()),
            ("mid", mid.clone()),
            ("idx", idx.to_string()),
            ("sn", session.fields["sn"].clone()),
            ("comment_id", comment_id.clone().unwrap_or_default()),
            (
                "appmsg_type",
                literal(html, "appmsg_type").unwrap_or_else(|| "9".into()),
            ),
            ("is_need_ad", "0".into()),
            ("is_need_reward", "0".into()),
            ("is_need_ticket", "0".into()),
            ("is_only_read", "1".into()),
            ("appmsg_like_type", "1".into()),
            ("tmp_version", "1".into()),
        ];
        for k in ["devicetype", "version", "pass_ticket"] {
            if let Some(v) = session.fields.get(k) {
                form.push((k, v.clone()));
            }
        }
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();
        let payload = json(
            store,
            client
                .post(stat_url)
                .header("Referer", session.page.as_str())
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(body),
            &article,
            &mut budget,
            collection.purpose(),
        )
        .await?;
        let metrics =
            stats(&payload).map_err(|error| access_error(store, &article, &error.to_string()))?;
        return store.save_article_interactions(id, &metrics);
    }
    let mut metrics = ArticleInteractions::default();
    let comment_id = comment_id.context("原文没有可读取的留言标识")?;
    // The upstream buffer is session-bound: retries restart the listing and merge by ID.
    store.resume_or_refresh_comments(id)?;
    let previous = store.article_comment_position(id)?;
    let mut offset = 0usize;
    let mut cursor = String::new();
    let mut seen = HashSet::new();
    for _ in 0..MAX_PAGES {
        let mut u = session.endpoint("/mp/appmsg_comment", &article.biz, &token);
        u.query_pairs_mut()
            .append_pair("action", "getcomment")
            .append_pair("appmsgid", &mid)
            .append_pair("idx", &idx.to_string())
            .append_pair("comment_id", &comment_id)
            .append_pair("offset", &offset.to_string())
            .append_pair("limit", &LIMIT.to_string())
            .append_pair("buffer", &cursor);
        let payload = json(
            store,
            client.get(u).header("Referer", session.page.as_str()),
            &article,
            &mut budget,
            collection.purpose(),
        )
        .await?;
        metrics.comment_count = payload
            .get("total_count")
            .and_then(number)
            .or(metrics.comment_count);
        let raw = payload
            .get("elected_comment")
            .and_then(Value::as_array)
            .context("微信返回的留言列表格式无法识别")?;
        let mut rows = Vec::new();
        let mut fresh = 0;
        for value in raw {
            let Some(mut row) = comments::parse_comment(value, 0) else {
                store.save_article_interactions_and_comments(id, &metrics, &rows)?;
                anyhow::bail!("微信留言包含暂不支持的内容格式，已保留可识别的结果");
            };
            if seen.insert(row.id.clone()) {
                fresh += 1;
            }
            if let Err(e) = replies(
                ReplyRequest {
                    store,
                    client: &client,
                    session: &session,
                    token: &token,
                    article: &article,
                    comment_id: &comment_id,
                    budget: &mut budget,
                },
                value,
                &mut row,
            )
            .await
            {
                rows.push(row);
                store.save_article_interactions_and_comments(id, &metrics, &rows)?;
                return Err(e);
            }
            rows.push(row);
        }
        store.save_article_interactions_and_comments(id, &metrics, &rows)?;
        let more = payload
            .get("continue_flag")
            .and_then(Value::as_bool)
            .context("微信未返回留言分页状态")?;
        if !more {
            ensure!(
                payload
                    .get("elected_comment_total_cnt")
                    .and_then(number)
                    .is_none_or(|n| seen.len() >= n as usize),
                "精选留言返回不完整，已保留已采集结果"
            );
            return store.save_article_comment_batch(id, &metrics, &[], Some((previous, -1)));
        }
        ensure!(
            !raw.is_empty() && fresh > 0,
            "微信留言分页未推进，已保留已采集结果"
        );
        offset = offset.checked_add(LIMIT).context("留言分页超出范围")?;
        cursor = buffer(&payload)?;
    }
    anyhow::bail!("本次留言采集达到分页上限，已保留已采集结果")
}

struct ReplyRequest<'a> {
    store: &'a Store,
    client: &'a reqwest::Client,
    session: &'a article_session::Session,
    token: &'a str,
    article: &'a ArticleRow,
    comment_id: &'a str,
    budget: &'a mut usize,
}
async fn replies(request: ReplyRequest<'_>, raw: &Value, row: &mut ArticleComment) -> Result<()> {
    let ReplyRequest {
        store,
        client,
        session,
        token,
        article,
        comment_id,
        budget,
    } = request;
    let Some(reply) = raw.get("reply_new") else {
        return Ok(());
    };
    let total = reply.get("reply_total_cnt").and_then(number).unwrap_or(0) as usize;
    if row.replies.len() >= total {
        return Ok(());
    }
    let root_id = raw.get("id").and_then(text).context("留言回复缺少根标识")?;
    let mut offset = 0usize;
    let mut cursor = String::new();
    let mut max_id = reply.get("max_reply_id").and_then(text).unwrap_or_default();
    let filter = row
        .replies
        .iter()
        .map(|r| r.id.as_str())
        .collect::<Vec<_>>()
        .join(",");
    for _ in 0..MAX_PAGES {
        let mut u = session.endpoint("/mp/appmsg_comment", &article.biz, token);
        u.query_pairs_mut()
            .append_pair("action", "getcommentreply")
            .append_pair("appmsgid", article.mid.as_deref().unwrap())
            .append_pair("idx", &article.idx.unwrap().to_string())
            .append_pair("comment_id", comment_id)
            .append_pair("content_id", &row.id)
            .append_pair("id", &root_id)
            .append_pair("limit", "20")
            .append_pair("offset", &offset.to_string())
            .append_pair("buffer", &cursor)
            .append_pair("is_first", if offset == 0 { "1" } else { "0" })
            .append_pair("max_reply_id", &max_id)
            .append_pair("filter_reply_list", &filter);
        let v = json(
            store,
            client.get(u).header("Referer", session.page.as_str()),
            article,
            budget,
            Collection::Comments.purpose(),
        )
        .await?;
        let list = v
            .pointer("/reply_list/reply_list")
            .and_then(Value::as_array)
            .context("回复列表不完整，已保留已采集结果")?;
        let before = row.replies.len();
        for value in list {
            let r = comments::parse_comment(value, 1).context("微信返回了无法识别的回复")?;
            comments::merge_comments(&mut row.replies, [r]);
        }
        let fresh = row.replies.len() - before;
        let more = v
            .get("continue_flag")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if row.replies.len() >= total {
            return Ok(());
        }
        ensure!(more, "回复尚未补齐，已保留已采集结果，请刷新原文后重试");
        ensure!(fresh > 0, "回复分页未推进，已保留已采集结果");
        offset += fresh;
        cursor = buffer(&v)?;
        max_id = v
            .pointer("/reply_list/max_reply_id")
            .and_then(text)
            .unwrap_or_default();
    }
    anyhow::bail!("本次回复采集达到分页上限，已保留已采集结果")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn capped_read_count_is_not_an_exact_number() {
        let m=stats(&json!({"appmsgstat":{"ret":0,"is_login":true,"read_num":100001,"old_like_num":42,"like_num":7},"comment_count":52})).unwrap();
        assert_eq!(m.read_count, None);
        assert_eq!(m.read_display.as_deref(), Some("10万+"));
        assert_eq!(m.like_count, Some(42));
        assert_eq!(m.comment_count, Some(52));
    }
    #[test]
    fn invalid_session_and_missing_stats_fail() {
        assert!(stats(&json!({"base_resp":{"ret":0}})).is_err());
        assert!(stats(&json!({"appmsgstat":{"ret":0,"is_login":false,"read_num":0}})).is_err());
    }
    #[test]
    fn conflicting_article_globals_fail() {
        assert_eq!(
            literal("var mid='';window.mid='1';", "mid").as_deref(),
            Some("1")
        );
        assert_eq!(literal("var mid='1';window.mid='2';", "mid"), None);
        assert_eq!(
            literal("var mid='1';window.mid='1';", "mid").as_deref(),
            Some("1")
        );
    }
}
