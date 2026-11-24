//! Public article comments and the local page returned to the workbench.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ArticleComment {
    pub id: String,
    pub author: String,
    pub content: String,
    pub likes: Option<i64>,
    pub created_at: Option<i64>,
    pub is_author: bool,
    pub reply_to: Option<String>,
    #[serde(default)]
    pub replies: Vec<ArticleComment>,
}

#[derive(Debug, Serialize)]
pub struct ArticleCommentsPage {
    pub items: Vec<ArticleComment>,
    pub next_cursor: Option<i64>,
    pub cached_count: i64,
    pub comment_count: Option<i64>,
    pub can_fetch_more: bool,
}

const COMMENT_FIELDS: &[&str] = &[
    "elected_comment",
    "elected_comment_list",
    "comment_list",
    "commentList",
    "preload_comment_list",
    "preload_comment_item_list",
];

fn from_json(value: &Value) -> Vec<ArticleComment> {
    let mut records = Vec::new();
    collect_payload(&mut records, value, true, 0);
    records
}

fn collect_payload(out: &mut Vec<ArticleComment>, value: &Value, scoped: bool, depth: u8) {
    if depth == 6 {
        return;
    }
    if scoped {
        if let Some(rows) = value.as_array() {
            append_rows(out, rows);
            return;
        }
        if let Some(text) = value.as_str() {
            if let Some(value) = json_prefix(text) {
                collect_payload(out, &value, true, depth + 1);
            }
            return;
        }
        for name in ["list", "items", "comments", "comment_item_list"] {
            if let Some(value) = value.get(name) {
                collect_payload(out, value, true, depth + 1);
            }
        }
    }
    for name in COMMENT_FIELDS {
        if let Some(value) = value.get(name) {
            collect_payload(out, value, true, depth + 1);
        }
    }
    for name in ["data", "comment", "comment_data"] {
        if let Some(value) = value.get(name) {
            collect_payload(out, value, scoped, depth + 1);
        }
    }
}

fn from_html(html: &str) -> Vec<ArticleComment> {
    use std::sync::LazyLock;

    static SCRIPTS: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?is)<script\b[^>]*>(.*?)</script\s*>").unwrap());
    static FIELD: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(&format!(
            r#"\b(?:{})\b(?:\\?["'])?\s*[:=]\s*"#,
            COMMENT_FIELDS.join("|")
        ))
        .unwrap()
    });

    let mut records = Vec::new();
    for script in SCRIPTS.captures_iter(html) {
        let source = &script[1];
        for field in FIELD.find_iter(source) {
            let tail = &source[field.end()..];
            let value = match tail.as_bytes().first() {
                Some(b'\'' | b'"') => js_string_prefix(tail).map(Value::String),
                Some(b'[' | b'{') => json_prefix(tail),
                _ => None,
            };
            if let Some(value) = value {
                collect_payload(&mut records, &value, true, 0);
            }
        }
    }
    records
}

fn json_prefix(source: &str) -> Option<Value> {
    let source = source.trim_start();
    if !source.starts_with(['[', '{', '"']) {
        return None;
    }
    serde_json::Deserializer::from_str(source)
        .into_iter::<Value>()
        .next()?
        .ok()
}

fn id_from_html(html: &str) -> Option<String> {
    use std::sync::LazyLock;
    static ID: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"\bcomment_id\b(?:\\?["'])?\s*[:=]\s*\\?["']?([0-9]{6,24})\b"#).unwrap()
    });
    let mut values = ID
        .captures_iter(html)
        .map(|entry| entry.get(1).unwrap().as_str());
    let value = values.next()?;
    values.all(|other| other == value).then(|| value.to_owned())
}

/// Decode the literal emitted for a preloaded comment list without executing JavaScript.
fn js_string_prefix(source: &str) -> Option<String> {
    let mut chars = source.chars();
    let delimiter = chars.next()?;
    let mut output = Vec::new();
    while let Some(c) = chars.next() {
        if c == delimiter {
            return String::from_utf16(&output).ok();
        }
        if c != '\\' {
            output.extend_from_slice(c.encode_utf16(&mut [0; 2]));
            continue;
        }
        let escape = chars.next()?;
        match escape {
            '\n' => continue,
            '\r' => {
                if chars.as_str().starts_with('\n') {
                    chars.next();
                }
                continue;
            }
            'x' | 'u' => {
                let digits = if escape == 'x' { 2 } else { 4 };
                let mut n = 0u16;
                for _ in 0..digits {
                    n = n * 16 + chars.next()?.to_digit(16)? as u16;
                }
                output.push(n);
            }
            other => output.push(match other {
                'n' => b'\n' as u16,
                'r' => b'\r' as u16,
                't' => b'\t' as u16,
                'b' => 8,
                'f' => 12,
                'v' => 11,
                '0' => 0,
                value if value.is_ascii() => value as u16,
                _ => return None,
            }),
        }
    }
    None
}

fn append_rows(out: &mut Vec<ArticleComment>, rows: &[Value]) {
    merge_comments(out, rows.iter().filter_map(|row| parse_comment(row, 0)));
}

pub(crate) fn merge_comments(
    out: &mut Vec<ArticleComment>,
    rows: impl IntoIterator<Item = ArticleComment>,
) {
    for mut row in rows {
        if let Some(prior) = out.iter_mut().find(|prior| prior.id == row.id) {
            merge_comments(&mut prior.replies, std::mem::take(&mut row.replies));
            prior.likes = row.likes.or(prior.likes);
            prior.created_at = prior.created_at.or(row.created_at);
            prior.reply_to = prior.reply_to.take().or(row.reply_to);
            prior.is_author |= row.is_author;
            if prior.author.is_empty() {
                prior.author = row.author;
            }
        } else {
            out.push(row);
        }
    }
}

fn parse_comment(value: &Value, depth: u8) -> Option<ArticleComment> {
    let field = |names: &[&str]| names.iter().find_map(|name| value.get(*name));
    let text = |names: &[&str]| {
        field(names)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    };
    let number = |names: &[&str]| {
        let value = field(names)?;
        value
            .as_i64()
            .or_else(|| value.as_str()?.parse().ok())
            .filter(|n| *n >= 0)
    };
    let content = text(&["content", "comment_content", "text"])?;
    let author = text(&["nick_name", "nickname", "nickName", "author"]).unwrap_or_default();
    let created_at = number(&["create_time", "created_at", "createTime"]);
    let supplied_id =
        field(&["content_id", "contentId", "reply_id", "replyId", "id"]).and_then(|id| match id {
            Value::String(text) if !text.is_empty() && text.len() <= 128 => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        });
    let id = supplied_id.unwrap_or_else(|| {
        use sha2::{Digest, Sha256};
        let input =
            serde_json::to_vec(&(&author, &content, created_at)).expect("serializable values");
        format!("text:{:x}", Sha256::digest(input))
    });

    let mut replies = Vec::new();
    if depth < 12 {
        for list in [
            field(&["reply_list", "replies", "replyList"]).and_then(Value::as_array),
            value.pointer("/reply/reply_list").and_then(Value::as_array),
            value.get("reply").and_then(Value::as_array),
        ]
        .into_iter()
        .flatten()
        {
            merge_comments(
                &mut replies,
                list.iter()
                    .filter_map(|item| parse_comment(item, depth + 1)),
            );
        }
    }
    let is_author = field(&["is_author", "isAuthor", "is_owner"]).is_some_and(|flag| {
        flag.as_bool() == Some(true) || flag.as_i64() == Some(1) || flag.as_str() == Some("1")
    });
    Some(ArticleComment {
        id,
        author,
        content,
        likes: number(&["like_num", "like_count", "likeCount"]),
        created_at,
        is_author,
        reply_to: text(&[
            "reply_to_nick_name",
            "reply_to_nickname",
            "replyToNickName",
            "to_nick_name",
        ]),
        replies,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_embedded_comment_threads_without_evaluating_page_scripts() {
        let html = r#"<script>
            window.data = {"elected_comment":[{"content_id":"c1","nick_name":"访客","content":"留言",
              "reply":{"reply_list":[{"reply_id":"r1","content":"第一层","is_author":1,"replies":[{"id":"r2","content":"第二层"}]}]}}]};
            window.other = {"elected_comment":[{"content_id":"c1","content":"留言"}]};
            </script><p>comment_list: [{"content":"文章正文"}]</p>"#;
        let result = from_html(html);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].replies[0].replies[0].content, "第二层");
        assert!(result[0].replies[0].is_author);
    }

    #[test]
    fn extracts_wechat_preloaded_comment_literals() {
        let html = r#"<script>
            var preload_comment_item_list = [];
            var preload_comment_list = '{\x22elected_comment\x22:[{\x22content_id\x22:12,\x22content\x22:\x22留言\x22,
              \x22reply\x22:{\x22reply_list\x22:[{\x22content\x22:\x22回复\x22}]}}]}';
            </script>"#;
        let comments = from_html(html);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].id, "12");
        assert_eq!(comments[0].replies[0].content, "回复");
    }

    #[test]
    fn direct_pages_reject_auth_errors_and_invalid_or_repeated_cursors() {
        let response = serde_json::json!({
            "base_resp":{"ret":0},"elected_comment":[{"content_id":"1","content":"留言"}],
            "elected_comment_total_cnt":2,"next_offset":1
        });
        let (rows, next, total) = decode_page(&response, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(next, 1);
        assert_eq!(total, Some(2));
        let repeated = serde_json::json!({"elected_comment":[{"content":"第二条"}],"has_more":true,"next_offset":1});
        assert!(decode_page(&repeated, 1).is_err());
        assert!(decode_page(
            &serde_json::json!({"base_resp":{"ret":-3},"elected_comment":[]}),
            1
        )
        .is_err());
        assert!(decode_page(
            &serde_json::json!({"elected_comment":[{"content_id":"1"}]}),
            0
        )
        .is_err());
        assert!(decode_page(
            &serde_json::json!({"elected_comment":[],"has_more":true}),
            0
        )
        .is_err());
    }

    #[test]
    fn comment_pages_keep_order_and_merge_replies_from_later_observations() {
        let store = crate::Store::open_in_memory().unwrap();
        let article = store
            .upsert_article("B", "42", 1, &crate::ArticleFields::default())
            .unwrap();
        let rows = (1..=25)
            .map(|id| serde_json::json!({"content_id": id, "content": format!("第 {id} 条")}))
            .collect::<Vec<_>>();
        let original = from_json(&serde_json::json!({"elected_comment":rows}));
        let metrics = super::super::interactions::ArticleInteractions {
            comment_count: Some(25),
            ..Default::default()
        };
        store
            .save_article_interactions_and_comments(article, &metrics, &original)
            .unwrap();

        let first = store.article_comments_page(article, None, 20).unwrap();
        assert_eq!(first.items.len(), 20);
        assert_eq!(first.items[19].id, "20");
        assert_eq!(first.comment_count, Some(25));
        assert_eq!(first.cached_count, 25);

        for id in ["a", "b"] {
            let update = from_json(
                &serde_json::json!({"comment_list":[{"content_id":21,"content":"第 21 条",
                "reply_list":[{"id":id,"content":"追加回复"}]}]}),
            );
            store
                .save_article_interactions_and_comments(article, &Default::default(), &update)
                .unwrap();
        }
        let last = store
            .article_comments_page(article, first.next_cursor, 20)
            .unwrap();
        assert_eq!(last.items.len(), 5);
        assert_eq!(last.items[0].id, "21");
        assert_eq!(last.items[0].replies.len(), 2);
        assert_eq!(last.next_cursor, None);
        assert_eq!(last.cached_count, 25);
    }
}

const ARTICLE_HOST: &str = "mp.weixin.qq.com";
const PAGE_SIZE: usize = 20;

/// Fetch one public page at a time. The article supplies the comment identifier and,
/// when present, the short-lived article token. Neither is used to bypass a challenge.
pub async fn fetch_page(store: &crate::Store, article_id: i64) -> anyhow::Result<()> {
    use super::interactions::ArticleInteractions;
    use anyhow::{ensure, Context};
    use std::time::Duration;

    static FETCH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _active = FETCH
        .try_lock()
        .map_err(|_| anyhow::anyhow!("正在读取留言，请稍后再试"))?;
    let (article, _) = store.get_article_md(article_id)?.context("文章不存在")?;
    ensure!(article.is_deleted == 0, "文章已删除");
    let offset = store.article_comment_position(article_id)?;
    if offset < 0 {
        return Ok(());
    }

    let link = article.content_url.as_deref().context("文章缺少原文链接")?;
    let normalized =
        crate::proxy_addon::normalize_article_link(link).map_err(anyhow::Error::msg)?;
    let mut url = url::Url::parse(&normalized)?;
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none_or(|port| port == 443),
        "原文链接包含不支持的身份或端口"
    );
    url.set_scheme("https")
        .map_err(|_| anyhow::anyhow!("文章链接无效"))?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(18))
        .user_agent("Mozilla/5.0")
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url();
            if url.scheme() == "https"
                && url.host_str() == Some(ARTICLE_HOST)
                && url.username().is_empty()
                && url.password().is_none()
                && url.port().is_none_or(|port| port == 443)
                && (url.path() == "/s" || url.path().starts_with("/s/"))
                && attempt.previous().len() < 4
            {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()?;
    let html = public_get(store, &client, url.clone(), &article, 6 * 1024 * 1024).await?;
    let html = std::str::from_utf8(&html).context("微信文章页编码异常")?;
    ensure!(
        !html.contains(r#"class="weui-msg__title">环境异常"#)
            && !html.contains("wappoc_appmsgcaptcha"),
        "微信返回访问验证页，当前无法直接读取留言。可在微信里查看原文"
    );

    let ids = crate::proxy_addon::parse_article_meta_from_html(Some(html));
    let mid = article.mid.as_deref().context("文章缺少消息 ID")?;
    let idx = article.idx.context("文章缺少序号")?;
    ensure!(
        ids.get("biz").is_none_or(|value| value == &article.biz)
            && ids.get("mid").is_none_or(|value| value == mid)
            && ids
                .get("idx")
                .is_none_or(|value| value.parse::<i64>() == Ok(idx)),
        "微信返回的原文身份与文章库记录不一致"
    );

    let source_identified = ids.get("biz") == Some(&article.biz)
        && ids.get("mid").is_some_and(|value| value == mid)
        && ids.get("idx").and_then(|value| value.parse::<i64>().ok()) == Some(idx);
    let link_identified = crate::proxy_addon::parse_s_url(url.as_str())
        .is_some_and(|parsed| parsed.biz == article.biz && parsed.mid == mid && parsed.idx == idx);
    ensure!(
        source_identified || link_identified,
        "微信返回的页面没有可核对的文章身份"
    );

    let mut metrics = ArticleInteractions::parse_html(html).unwrap_or_default();
    let preloaded = from_html(html);
    if offset == 0 && (!preloaded.is_empty() || metrics.comment_count == Some(0)) {
        let loaded = preloaded.len() as i64;
        let next = match metrics.comment_count {
            Some(total) if loaded >= total => -1,
            _ => loaded,
        };
        return store.save_article_comment_batch(
            article_id,
            &metrics,
            &preloaded,
            Some((offset, next)),
        );
    }

    let comment_id = id_from_html(html).context("原文没有提供可读取的留言标识")?;
    let mut endpoint = url::Url::parse("https://mp.weixin.qq.com/mp/appmsg_comment")?;
    {
        let mut query = endpoint.query_pairs_mut();
        query
            .append_pair("action", "getcomment")
            .append_pair("__biz", &article.biz)
            .append_pair("appmsgid", mid)
            .append_pair("idx", &idx.to_string())
            .append_pair("comment_id", &comment_id)
            .append_pair("offset", &offset.to_string())
            .append_pair("limit", &PAGE_SIZE.to_string())
            .append_pair("f", "json");
        if let Some(token) = article_token(html) {
            query.append_pair("appmsg_token", &token);
        }
    }
    let bytes = public_get(store, &client, endpoint, &article, 1024 * 1024).await?;
    let payload: Value = serde_json::from_slice(&bytes)
        .context("微信未返回可读取的留言响应，可能需要有效的文章访问令牌")?;
    let (rows, next, reported) = decode_page(&payload, offset)?;
    metrics.comment_count = reported.or(metrics.comment_count);
    store.save_article_comment_batch(article_id, &metrics, &rows, Some((offset, next)))
}

fn article_token(html: &str) -> Option<String> {
    use std::sync::LazyLock;
    static TOKEN: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"\bappmsg_token\b["']?\s*[:=]\s*["']([A-Za-z0-9%+=_/-]{8,512})["']"#)
            .unwrap()
    });
    let mut values = TOKEN
        .captures_iter(html)
        .map(|entry| entry.get(1).unwrap().as_str());
    let value = values.next()?;
    values.all(|other| other == value).then(|| value.to_owned())
}

fn decode_page(
    payload: &Value,
    offset: i64,
) -> anyhow::Result<(Vec<ArticleComment>, i64, Option<i64>)> {
    use anyhow::{ensure, Context};

    let number = |value: &Value| value.as_i64().or_else(|| value.as_str()?.parse().ok());
    ensure!(
        payload.is_object()
            && [
                payload.get("ret"),
                payload.get("errcode"),
                payload.pointer("/base_resp/ret")
            ]
            .into_iter()
            .flatten()
            .all(|v| number(v) == Some(0)),
        "微信拒绝了这次留言请求，文章访问令牌可能不可用"
    );
    let fields = [
        Some(payload),
        payload.get("data"),
        payload.get("comment_data"),
    ];
    let get = |names: &[&str]| {
        fields
            .iter()
            .flatten()
            .find_map(|object| names.iter().find_map(|name| object.get(*name)))
    };
    let list = get(COMMENT_FIELDS).or_else(|| get(&["list", "items", "comments"]));
    let direct_rows = list.and_then(Value::as_array);
    let mut rows = from_json(payload);
    if rows.is_empty() {
        if let Some(seed) = direct_rows {
            append_rows(&mut rows, seed);
        }
    }
    ensure!(
        direct_rows.is_some() || !rows.is_empty(),
        "微信返回的留言列表格式无法识别"
    );
    let total = get(&[
        "elected_comment_total_cnt",
        "elected_comment_total_count",
        "total_count",
    ])
    .and_then(number)
    .filter(|value| *value >= 0);
    if let Some(direct_rows) = direct_rows {
        ensure!(
            direct_rows
                .iter()
                .all(|row| parse_comment(row, 0).is_some()),
            "微信返回的部分留言格式无法识别"
        );
    }
    let seen = direct_rows.map_or(rows.len(), Vec::len) as i64;
    let boundary = offset.checked_add(seen).context("留言分页超出范围")?;
    let continued = get(&["can_continue", "has_more", "is_continue"]).and_then(|v| {
        v.as_bool().or_else(|| match number(v) {
            Some(0) => Some(false),
            Some(1) => Some(true),
            _ => None,
        })
    });
    ensure!(
        seen != 0 || continued != Some(true),
        "微信返回空页且未结束留言列表"
    );
    let upstream_next = get(&["next_offset", "nextOffset"]).and_then(number);
    let next =
        if seen == 0 || continued == Some(false) || total.is_some_and(|total| boundary >= total) {
            -1
        } else if let Some(next) = upstream_next {
            ensure!(next > offset, "微信返回的留言游标未推进");
            next
        } else if continued == Some(true)
            || total.is_some_and(|total| boundary < total)
            || seen >= PAGE_SIZE as i64
        {
            boundary
        } else {
            -1
        };
    Ok((rows, next, total))
}

async fn public_get(
    store: &crate::Store,
    client: &reqwest::Client,
    url: url::Url,
    article: &crate::model::ArticleRow,
    limit: usize,
) -> anyhow::Result<Vec<u8>> {
    use crate::model::now;
    use anyhow::ensure;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(12);
    let request_id = loop {
        let permit = store.wb_request_permit(
            "webpage",
            "article_comments",
            article.id,
            &article.biz,
            now(),
        )?;
        if let Some(id) = permit.request_id {
            break id;
        }
        let delay = std::time::Duration::from_secs_f64((permit.next_at - now()).clamp(0.05, 12.0));
        ensure!(
            tokio::time::Instant::now() + delay <= deadline,
            "文章请求当前受限，请稍后再试"
        );
        tokio::time::sleep(delay).await;
    };
    let mut completion = super::request::Completion::new(store, &request_id);
    let mut status = None;
    let result = async {
        let mut response = client
            .get(url)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;
        status = Some(response.status().as_u16());
        if status == Some(429) {
            store.wb_cooldown(now() + store.wb_policy()?.cooldown_seconds as f64)?;
        }
        ensure!(
            response.status().is_success(),
            "微信返回 HTTP {}",
            response.status().as_u16()
        );
        ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= limit as u64),
            "微信响应超出长度上限"
        );
        let mut content = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(reqwest::Error::without_url)?
        {
            ensure!(content.len() + chunk.len() <= limit, "微信响应超出长度上限");
            content.extend_from_slice(&chunk);
        }
        Ok(content)
    }
    .await;
    let outcome = match (result.is_ok(), status) {
        (true, _) => "ok",
        (_, Some(429)) => "rate_limited",
        (_, Some(_)) => "http_error",
        _ => "network_error",
    };
    completion.finish(
        outcome,
        serde_json::json!({"http_status":status, "article_id":article.id}),
        now(),
    )?;
    result
}
