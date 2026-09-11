//! 微信公众号响应解析（纯函数，脱网单测）—— 对齐 Python 原型的响应解析。
//!
//! 网络层（fetch_*）属后续阶段；此处只移植两个解析器：
//! - [`parse_msg_list`]：展开 `general_msg_list` 多图文，去重，字段对齐。
//! - [`parse_article_html`]：用 `scraper` 取 `#activity-name` / og:title / `#js_content`。

use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;
use scraper::{Html, Selector};
use serde_json::Value;

use crate::model::{Credential, ParsedArticle};

/// 回放请求默认 UA：必须与微信内置浏览器一致，否则接口会拒绝（对齐 Python `config.DEFAULT_UA`）。
pub const DEFAULT_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36 MicroMessenger/7.0.0 MacWechat";

const BASE: &str = "https://mp.weixin.qq.com";

/// 拉取一页历史消息列表（`mp/profile_ext?action=getmsg`）。翻页靠 `offset`，
/// `can_msg_continue=1` 表示还有下一页。对齐 Python `wechat_api.fetch_msg_list`。
///
/// - `wrap`：可选 URL 重写钩子（单测把请求指到本地 mock 服务；真机传 `None`）。
/// - 直连 + `no_proxy`（等价 Python `trust_env=False`）：抓凭证代理开着时不劫持本请求。
pub async fn fetch_msg_list(
    cred: &Credential,
    offset: i64,
    count: i64,
    wrap: Option<&(dyn Fn(&str) -> String + Send + Sync)>,
) -> Result<Value> {
    let params: [(&str, String); 11] = [
        ("action", "getmsg".to_string()),
        ("__biz", cred.biz.clone()),
        ("f", "json".to_string()),
        ("offset", offset.to_string()),
        ("count", count.to_string()),
        ("is_ok", "1".to_string()),
        ("scene", "124".to_string()),
        ("uin", cred.uin.clone().unwrap_or_default()),
        ("key", cred.key.clone().unwrap_or_default()),
        ("pass_ticket", cred.pass_ticket.clone().unwrap_or_default()),
        ("wxtoken", cred.wxtoken.clone().unwrap_or_default()),
    ];
    let mut full = reqwest::Url::parse_with_params(&format!("{BASE}/mp/profile_ext"), &params)?;
    // x5 单独加（可能是 "0"）。
    full.query_pairs_mut()
        .append_pair("x5", &cred.x5.clone().unwrap_or_else(|| "0".to_string()));
    let target = match wrap {
        Some(w) => w(full.as_str()),
        None => full.to_string(),
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(DEFAULT_UA)
        .no_proxy()
        .build()?;

    let mut req = client.get(target);
    if let Some(cookie) = &cred.cookie {
        req = req.header("Cookie", cookie);
    }
    // 错误里不带 URL（reqwest 默认把完整 URL 拼进错误文案，其中含 key/pass_ticket）。
    let resp = req
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("请求文章列表接口失败：{}", describe_reqwest_err(e)))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| anyhow::anyhow!("读取文章列表响应失败：{}", describe_reqwest_err(e)))?;
    classify_list_response(status.as_u16(), &text)
}

/// reqwest 错误连同**原因链**（超时 / 连接被拒 / TLS …），不带 URL（URL 里有 key/pass_ticket）。
/// reqwest 的 Display 只有一句「error sending request」，真正的原因在 `source()` 链里。
fn describe_reqwest_err(e: reqwest::Error) -> String {
    let e = e.without_url();
    let mut s = e.to_string();
    if e.is_timeout() {
        s.push_str("（超时）");
    } else if e.is_connect() {
        s.push_str("（连接失败）");
    }
    let mut src = std::error::Error::source(&e);
    while let Some(c) = src {
        s.push_str(&format!("：{c}"));
        src = c.source();
    }
    s
}

/// 把列表接口的原始响应分成三类：限流信号（[`RateLimited`]）/ 其它 HTTP 错误 / 正常 JSON。
///
/// 限流信号 = HTTP 429、5xx，或 2xx 但返回的是网页而非 JSON（微信把带凭证的接口请求
/// 重定向到人机验证页时就是这样）。纯函数，便于单测。
pub fn classify_list_response(status: u16, text: &str) -> Result<Value> {
    if status == 429 || (500..600).contains(&status) {
        return Err(RateLimited(format!("文章列表接口返回 HTTP {status}")).into());
    }
    if !(200..300).contains(&status) {
        anyhow::bail!("文章列表接口返回 HTTP {status}");
    }
    let trimmed = text.trim_start();
    if trimmed.starts_with('<') {
        return Err(
            RateLimited("文章列表接口返回了网页而非 JSON（疑似人机验证页）".to_string()).into(),
        );
    }
    Ok(serde_json::from_str::<Value>(text).unwrap_or(Value::Null))
}

/// 微信侧限流信号（HTTP 429 / 5xx / 接口被重定向到验证页）。上层据此**整机退避**，而不是
/// 当成该号的普通采集异常。用 `anyhow::Error::downcast_ref::<RateLimited>()` 判别。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimited(pub String);

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for RateLimited {}

/// 判别一个错误是否为限流信号。
pub fn is_rate_limited_err(e: &anyhow::Error) -> bool {
    e.downcast_ref::<RateLimited>().is_some()
}

/// 从列表接口错误里抠出 HTTP 状态码（留档 `list_call_log.http_status` 用）：
/// 文案里有「HTTP 429」这类字样就取其数字；[`RateLimited`] 的「接口回网页」是 2xx 拿到 HTML，记 200；
/// 传输层错误（连不上 / 超时）返回 `None`。纯函数，便于单测。
pub fn error_http_status(e: &anyhow::Error) -> Option<i64> {
    let text = e.to_string();
    if let Some(pos) = text.find("HTTP ") {
        let digits: String = text[pos + 5..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(n) = digits.parse::<i64>() {
            return Some(n);
        }
    }
    if is_rate_limited_err(e) && text.contains("网页") {
        return Some(200);
    }
    None
}

/// **账号级封禁**信号：`getmsg` 回 HTTP 200 但 `ret=-6`（`unknown error`）或 `-12`，
/// `home_page_list=[]`。微信已把**这个微信号**识别为异常请求方——与 biz / key 是否新鲜无关，
/// 换 key 会继续 -6；公开资料与实测：限制约 1 天，换微信号即恢复。
/// 上层据此**长时退避**（[`crate::runstate::block_trigger`]），不重试、不换 key。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountBlocked(pub String);

impl std::fmt::Display for AccountBlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for AccountBlocked {}

/// 判别一个错误是否为账号级封禁信号。
pub fn is_account_blocked_err(e: &anyhow::Error) -> bool {
    e.downcast_ref::<AccountBlocked>().is_some()
}

/// `ret` 码是否属于「账号被识别 / 需验证」（`-6` unknown error、`-12`；对齐
/// 实测记录：-6/-12 需验证）。`errmsg` 含 `freq`（freq control）也算。
pub fn is_blocked_ret(ret: i64, errmsg: &str) -> bool {
    matches!(ret, -6 | -12) || errmsg.to_ascii_lowercase().contains("freq")
}

/// 微信 `ret` 码（顶层或 `base_resp.ret`）；缺失视为 `None`。
pub fn response_ret(obj: &Value) -> Option<i64> {
    let map = obj.as_object()?;
    if let Some(r) = map.get("ret").and_then(Value::as_i64) {
        return Some(r);
    }
    map.get("base_resp")
        .and_then(Value::as_object)
        .and_then(|b| b.get("ret"))
        .and_then(Value::as_i64)
}

/// 文章正文解析结果（对齐 Python parse_article_html 返回）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArticleContent {
    pub title: Option<String>,
    pub author: Option<String>,
    pub content_html: Option<String>,
    pub content_text: Option<String>,
}

/// **静默空页**判定：`getmsg` 回 HTTP 200、`ret=0`、`errmsg="ok"`，但**没有列表容器**且 `msg_count=0`
/// —— 微信既不报错也不给数据。
///
/// **2026-09-10 起这是接口的常态**（见 `Faq.tsx` 第一条「当前现状」）：当天中午同一微信号还能正常采到
/// 149 篇（响应含 22533 字节的 `general_msg_list`），当天下午起同一请求就只剩这种空响应，`general_msg_list`
/// / `next_offset` / `real_type` 字段一并消失。随后用三个微信号（含一个**从未触发过限制、本次是其首次列表
/// 请求**的全新号）、Windows 与 mac 两台机器、7 个公众号交叉验证，响应完全一致 —— 全新号首次请求即空，
/// 排除了账号级限制，指向**服务端对该接口的调整**。列表采集功能因此不可用，本判定的作用是让软件如实
/// 报「取不到数据」而不是误报「没有更多历史」。
///
/// 调用方只在**首采首页**（offset=0 的第 1 页）用它：一个有文章的公众号首页不可能为空，而真正
/// 翻到底至少发生在第 2 页或 offset>0 的续采上，不会命中。纯函数，便于单测。
pub fn is_silent_empty_list(obj: &Value) -> bool {
    let Some(map) = obj.as_object() else {
        return false;
    };
    // 微信明确报错（ret 非 0）的响应由调用方按接口错误处理，不算静默空页。
    if response_ret(obj).unwrap_or(0) != 0 {
        return false;
    }
    // 只要微信给了**列表容器**（顶层 `list` 数组，或 `general_msg_list` 里能解出 `list`），
    // 哪怕它是空的，也说明接口在正常回列表 —— 那是「没有更多」，不是静默空页。
    // 两种字段都要认：[`parse_msg_list`] 两种都解析（真机回 `general_msg_list`，mock 回 `list`）。
    if map.get("list").is_some_and(Value::is_array) {
        return false;
    }
    if let Some(gml) = map.get("general_msg_list") {
        let inner = match gml {
            Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
            other => other.clone(),
        };
        if inner
            .as_object()
            .is_some_and(|o| o.get("list").is_some_and(Value::is_array))
        {
            return false;
        }
    }
    let msg_count = map
        .get("msg_count")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0);
    msg_count == 0
}

/// 微信 `ret=-3`（no session）视为凭证过期的判定（依据实测记录）。
///
/// 纯解析函数不抛异常；上层网络层可用它把凭证过期与空数据区分开。
pub fn is_credential_expired(obj: &Value) -> bool {
    let Some(map) = obj.as_object() else {
        return false;
    };
    if map.get("ret").and_then(Value::as_i64) == Some(-3) {
        return true;
    }
    if let Some(base) = map.get("base_resp").and_then(Value::as_object) {
        if base.get("ret").and_then(Value::as_i64) == Some(-3) {
            return true;
        }
    }
    false
}

/// 解析 `general_msg_list`，展开多图文，返回文章列表。
///
/// 入参可以是：微信完整响应 dict、`general_msg_list` 的 JSON 字符串、
/// 或已解析的 `{"list": [...]}` dict（对齐 Python 的多形状容错）。
pub fn parse_msg_list(json_obj: Value) -> Vec<ParsedArticle> {
    // 1) 统一成 Value：字符串先解析。
    let json_obj = match json_obj {
        Value::String(s) => match serde_json::from_str::<Value>(&s) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        },
        other => other,
    };

    // 2) 定位到含 "list" 的容器。
    let container: Value = if let Some(map) = json_obj.as_object() {
        if let Some(gml) = map.get("general_msg_list") {
            match gml {
                Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
                other => other.clone(),
            }
        } else {
            json_obj.clone()
        }
    } else {
        return Vec::new();
    };

    let Some(container) = container.as_object() else {
        return Vec::new();
    };
    let raw_list = match container.get("list") {
        Some(Value::Array(a)) => a.clone(),
        _ => return Vec::new(),
    };

    let mut articles: Vec<ParsedArticle> = Vec::new();
    let mut seen: std::collections::HashSet<(String, i64)> = std::collections::HashSet::new();

    for msg in &raw_list {
        let Some(msg) = msg.as_object() else {
            continue;
        };
        let comm = msg.get("comm_msg_info").and_then(Value::as_object);
        let published_at = comm.and_then(|c| c.get("datetime")).and_then(Value::as_i64);
        let mid_fallback = comm.and_then(|c| c.get("id")).and_then(number_to_string);

        // 非图文消息（无 app_msg_ext_info）跳过。
        let Some(app) = msg.get("app_msg_ext_info").and_then(Value::as_object) else {
            continue;
        };

        // candidates = [app] + multi_app_msg_item_list
        let mut candidates: Vec<&serde_json::Map<String, Value>> = vec![app];
        if let Some(Value::Array(subs)) = app.get("multi_app_msg_item_list") {
            for s in subs {
                if let Some(m) = s.as_object() {
                    candidates.push(m);
                }
            }
        }

        for (pos, item) in candidates.iter().enumerate() {
            let default_idx = (pos + 1) as i64;
            let Some(art) = build_article(item, mid_fallback.as_deref(), published_at, default_idx)
            else {
                continue;
            };
            let key = (art.mid.clone(), art.idx);
            if seen.contains(&key) {
                continue; // 多图文首篇可能与 multi 列表首项重复，去重
            }
            seen.insert(key);
            articles.push(art);
        }
    }
    articles
}

/// 把 general_msg_list 里的单个图文条目转成 [`ParsedArticle`]（对齐 Python `_build_article`）。
fn build_article(
    item: &serde_json::Map<String, Value>,
    mid_fallback: Option<&str>,
    published_at: Option<i64>,
    default_idx: i64,
) -> Option<ParsedArticle> {
    // content_url 常带 &amp; 实体，需先反转义。
    let content_url = html_unescape(
        item.get("content_url")
            .and_then(Value::as_str)
            .unwrap_or(""),
    );
    let (qmid, qidx, qsn) = extract_url_ids(&content_url);

    let mid = qmid.or_else(|| mid_fallback.map(|s| s.to_string()))?;
    let idx = qidx.unwrap_or(default_idx);

    Some(ParsedArticle {
        mid,
        idx,
        sn: qsn,
        title: item
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_string),
        digest: item
            .get("digest")
            .and_then(Value::as_str)
            .map(str::to_string),
        content_url: if content_url.is_empty() {
            None
        } else {
            Some(content_url)
        },
        author: item
            .get("author")
            .and_then(Value::as_str)
            .map(str::to_string),
        cover: item
            .get("cover")
            .and_then(Value::as_str)
            .map(str::to_string),
        published_at,
    })
}

/// 从 `/s?__biz=&mid=&idx=&sn=` 的 URL query 里补出 (mid, idx, sn)。
fn extract_url_ids(content_url: &str) -> (Option<String>, Option<i64>, Option<String>) {
    if content_url.is_empty() {
        return (None, None, None);
    }
    let query = match content_url.split_once('?') {
        Some((_, q)) => q.split('#').next().unwrap_or(""),
        None => return (None, None, None),
    };
    // parse_qs 语义：每个键取第一次出现的值。
    let mut mid = None;
    let mut idx_raw = None;
    let mut sn = None;
    for (k, v) in form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "mid" if mid.is_none() => mid = Some(v.into_owned()),
            "idx" if idx_raw.is_none() => idx_raw = Some(v.into_owned()),
            "sn" if sn.is_none() => sn = Some(v.into_owned()),
            _ => {}
        }
    }
    let idx = idx_raw.and_then(|s| s.parse::<i64>().ok());
    (mid, idx, sn)
}

/// 用 `scraper` 解析正文页，返回 title/author/content_html/content_text。
///
/// - 标题：`#activity-name` → og:title → `<title>`
/// - 作者：`#js_author_name` → meta[name=author] → `#js_name`
/// - 正文：`#js_content` 的 HTML 与纯文本（剔除 script/style）
pub fn parse_article_html(html_text: &str) -> ArticleContent {
    let doc = Html::parse_document(html_text);

    let text_of = |selector: &str| -> Option<String> {
        let sel = Selector::parse(selector).ok()?;
        let node = doc.select(&sel).next()?;
        let t = node.text().collect::<String>();
        let t = t.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    };
    let meta_of = |selector: &str| -> Option<String> {
        let sel = Selector::parse(selector).ok()?;
        let node = doc.select(&sel).next()?;
        let v = node.value().attr("content")?.trim();
        if v.is_empty() {
            None
        } else {
            Some(v.to_string())
        }
    };

    let title = text_of("#activity-name")
        .or_else(|| meta_of(r#"meta[property="og:title"]"#))
        .or_else(|| text_of("title"));
    let author = text_of("#js_author_name")
        .or_else(|| meta_of(r#"meta[name="author"]"#))
        .or_else(|| text_of("#js_name"));

    let mut content_html = None;
    let mut content_text = None;
    if let Ok(sel) = Selector::parse("#js_content") {
        if let Some(node) = doc.select(&sel).next() {
            // 去掉 script/style，避免污染正文（scraper DOM 不可变，改用字符串剥离）。
            let outer = node.html();
            let stripped = strip_script_style(&outer);
            content_html = Some(stripped.clone());
            // 从剥离后的片段收集文本，按行折叠空白。
            let frag = Html::parse_fragment(&stripped);
            let mut lines: Vec<String> = Vec::new();
            for chunk in frag.root_element().text() {
                for line in chunk.split('\n') {
                    let t = line.trim();
                    if !t.is_empty() {
                        lines.push(t.to_string());
                    }
                }
            }
            content_text = if lines.is_empty() {
                None
            } else {
                Some(lines.join("\n"))
            };
        }
    }

    ArticleContent {
        title,
        author,
        content_html,
        content_text,
    }
}

/// 剥离 HTML 里的 `<script>...</script>` 与 `<style>...</style>`（大小写不敏感、跨行）。
fn strip_script_style(html: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?is)<script\b[^>]*>.*?</script\s*>|<style\b[^>]*>.*?</style\s*>").unwrap()
    });
    RE.replace_all(html, "").into_owned()
}

/// 把 JSON 标量数字/字符串转成字符串（用于 comm_msg_info.id 兜底 mid）。
fn number_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// 最小 HTML 实体反转义（对齐 Python `html.unescape` 常见分支：命名 + 数字）。
fn html_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    static ENT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z][a-zA-Z0-9]*);").unwrap()
    });
    ENT_RE
        .replace_all(s, |caps: &regex::Captures| {
            let body = &caps[1];
            if let Some(hex) = body.strip_prefix("#x").or_else(|| body.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| caps[0].to_string())
            } else if let Some(dec) = body.strip_prefix('#') {
                dec.parse::<u32>()
                    .ok()
                    .and_then(char::from_u32)
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| caps[0].to_string())
            } else {
                match body {
                    "amp" => "&".to_string(),
                    "lt" => "<".to_string(),
                    "gt" => ">".to_string(),
                    "quot" => "\"".to_string(),
                    "apos" => "'".to_string(),
                    "nbsp" => "\u{a0}".to_string(),
                    _ => caps[0].to_string(), // 未知实体原样保留
                }
            }
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 留档用的 HTTP 状态抠取：429 / 5xx 从文案取数字；验证页记 200；传输层错误无状态。
    /// 静默空页（账号软限制）与正常响应的区分：只有「ret=0 + 无 general_msg_list + msg_count=0」才算。
    #[test]
    fn test_is_silent_empty_list() {
        use serde_json::json;
        // 真机实测形态（2026-09-11，Windows 与 mac 上一致）
        assert!(is_silent_empty_list(&json!({
            "ret": 0, "errmsg": "ok", "msg_count": 0,
            "can_msg_continue": 0, "home_page_list": []
        })));
        // 有列表容器 = 微信在正常回列表，属「没有更多」，不是静默空页（两种字段都认）
        assert!(!is_silent_empty_list(&json!({
            "ret": 0, "errmsg": "ok", "msg_count": 0,
            "general_msg_list": "{\"list\":[]}"
        })));
        assert!(!is_silent_empty_list(&json!({
            "ret": 0, "msg_count": 0, "list": []
        })));
        assert!(!is_silent_empty_list(&json!({
            "ret": 0, "msg_count": 10, "general_msg_list": "{\"list\":[{}]}"
        })));
        // msg_count 非 0 也不算（即便没 general_msg_list）
        assert!(!is_silent_empty_list(&json!({"ret": 0, "msg_count": 3})));
        // 微信明确报错的响应交给接口错误分支，不算静默空页
        assert!(!is_silent_empty_list(
            &json!({"ret": -6, "errmsg": "unknown error"})
        ));
        assert!(!is_silent_empty_list(&json!({
            "base_resp": {"ret": -3}, "msg_count": 0
        })));
        // 非对象
        assert!(!is_silent_empty_list(&json!("x")));
    }

    #[test]
    fn test_error_http_status() {
        let e429 = classify_list_response(429, "").unwrap_err();
        assert_eq!(error_http_status(&e429), Some(429));
        let e503 = classify_list_response(503, "").unwrap_err();
        assert_eq!(error_http_status(&e503), Some(503));
        let e403 = classify_list_response(403, "").unwrap_err();
        assert_eq!(error_http_status(&e403), Some(403));
        let page = classify_list_response(200, "<html>verify</html>").unwrap_err();
        assert_eq!(error_http_status(&page), Some(200));
        let net = anyhow::anyhow!("请求文章列表接口失败：error sending request（超时）");
        assert_eq!(error_http_status(&net), None);
    }

    fn sample_general_msg_list() -> Value {
        let inner = json!({
            "list": [
                {
                    "comm_msg_info": {"id": 2650399036i64, "type": 49, "datetime": 1787542200i64},
                    "app_msg_ext_info": {
                        "title": "主文章标题",
                        "digest": "主摘要",
                        "author": "作者A",
                        "cover": "https://mmbiz.qpic.cn/cover1.jpg",
                        "content_url": "http://mp.weixin.qq.com/s?__biz=VEVTVEJJWjAwMQ==&amp;mid=2650399036&amp;idx=1&amp;sn=abc123&amp;chksm=xxx",
                        "is_multi": 1,
                        "multi_app_msg_item_list": [
                            {
                                "title": "次条标题",
                                "digest": "次摘要",
                                "author": "作者B",
                                "cover": "https://mmbiz.qpic.cn/cover2.jpg",
                                "content_url": "http://mp.weixin.qq.com/s?__biz=VEVTVEJJWjAwMQ==&amp;mid=2650399036&amp;idx=2&amp;sn=def456&amp;chksm=yyy"
                            }
                        ]
                    }
                },
                {
                    "comm_msg_info": {"id": 2650399000i64, "type": 49, "datetime": 1787500000i64},
                    "app_msg_ext_info": {
                        "title": "单篇文章",
                        "digest": "单摘要",
                        "author": "作者C",
                        "cover": "https://mmbiz.qpic.cn/cover3.jpg",
                        "content_url": "http://mp.weixin.qq.com/s?__biz=VEVTVEJJWjAwMQ==&amp;mid=2650399000&amp;idx=1&amp;sn=ghi789&amp;chksm=zzz",
                        "is_multi": 0,
                        "multi_app_msg_item_list": []
                    }
                },
                {
                    "comm_msg_info": {"id": 111, "type": 1, "datetime": 1787400000i64}
                }
            ]
        });
        json!({
            "ret": 0,
            "errmsg": "ok",
            "msg_count": 2,
            "can_msg_continue": 1,
            "next_offset": 12,
            "general_msg_list": inner.to_string()
        })
    }

    #[test]
    fn test_parse_msg_list_full_response() {
        let arts = parse_msg_list(sample_general_msg_list());
        assert_eq!(arts.len(), 3); // 首篇 + 次条 + 单篇；非图文被跳过

        let a0 = &arts[0];
        assert_eq!(a0.mid, "2650399036");
        assert_eq!(a0.idx, 1);
        assert_eq!(a0.sn.as_deref(), Some("abc123"));
        assert_eq!(a0.title.as_deref(), Some("主文章标题"));
        assert_eq!(a0.author.as_deref(), Some("作者A"));
        assert_eq!(a0.digest.as_deref(), Some("主摘要"));
        assert!(a0.cover.as_deref().unwrap().ends_with("cover1.jpg"));
        assert_eq!(a0.published_at, Some(1787542200));
        // content_url 的 &amp; 被反转义成 &
        let cu = a0.content_url.as_deref().unwrap();
        assert!(!cu.contains("&amp;"));
        assert!(cu.contains("mid=2650399036"));

        let a1 = &arts[1];
        assert_eq!(a1.mid, "2650399036");
        assert_eq!(a1.idx, 2);
        assert_eq!(a1.sn.as_deref(), Some("def456"));
        assert_eq!(a1.title.as_deref(), Some("次条标题"));

        let a2 = &arts[2];
        assert_eq!(a2.mid, "2650399000");
        assert_eq!(a2.idx, 1);
        assert_eq!(a2.sn.as_deref(), Some("ghi789"));
    }

    #[test]
    fn test_parse_msg_list_accepts_json_string() {
        let inner_str = match sample_general_msg_list() {
            Value::Object(m) => m.get("general_msg_list").unwrap().clone(),
            _ => unreachable!(),
        };
        let arts = parse_msg_list(inner_str);
        assert_eq!(arts.len(), 3);
        assert_eq!(arts[0].sn.as_deref(), Some("abc123"));
    }

    #[test]
    fn test_parse_msg_list_empty() {
        assert_eq!(
            parse_msg_list(json!({"general_msg_list": "{\"list\": []}"})).len(),
            0
        );
        assert_eq!(parse_msg_list(json!({})).len(), 0);
    }

    const ARTICLE_HTML: &str = r#"
<html>
<head>
  <meta property="og:title" content="OG标题">
  <meta name="author" content="Meta作者">
  <title>网页标题</title>
</head>
<body>
  <h1 id="activity-name">  测试文章标题  </h1>
  <span id="js_author_name">署名作者</span>
  <div id="js_name">公众号名</div>
  <div id="js_content">
    <p>第一段正文。</p>
    <p>第二段正文。</p>
    <script>var x = 1;</script>
  </div>
</body>
</html>
"#;

    #[test]
    fn test_parse_article_html_full() {
        let r = parse_article_html(ARTICLE_HTML);
        assert_eq!(r.title.as_deref(), Some("测试文章标题")); // #activity-name 优先且去空白
        assert_eq!(r.author.as_deref(), Some("署名作者")); // #js_author_name 优先
        let text = r.content_text.as_deref().unwrap();
        assert!(text.contains("第一段正文。"));
        assert!(text.contains("第二段正文。"));
        assert!(!text.contains("var x")); // script 已剔除
        assert!(r.content_html.as_deref().unwrap().contains("js_content"));
    }

    #[test]
    fn test_parse_article_html_fallback() {
        let html = r#"
        <html><head>
          <meta property="og:title" content="备用标题">
          <meta name="author" content="备用作者">
        </head><body>
          <div id="js_content"><p>正文A</p></div>
        </body></html>
        "#;
        let r = parse_article_html(html);
        assert_eq!(r.title.as_deref(), Some("备用标题"));
        assert_eq!(r.author.as_deref(), Some("备用作者"));
        assert!(r.content_text.as_deref().unwrap().contains("正文A"));
    }
}
