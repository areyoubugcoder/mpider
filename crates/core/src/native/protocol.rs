//! 原生桥载荷的纯解析与结果判定；不发网络请求，测试数据全部使用虚构标识。

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

use crate::model::ParsedArticle;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeRequest {
    pub request_id: String,
    pub ts: f64,
    pub cgi: i64,
    pub action: Option<i64>,
    pub offset: Option<String>,
    pub user_name: Option<String>,
    #[serde(default = "native_transport")]
    pub transport: String,
    #[serde(default)]
    pub capture_only: bool,
}

fn native_transport() -> String {
    "native".into()
}

#[derive(Clone, Debug)]
pub struct NativePage {
    pub biz: String,
    pub user_name: String,
    pub nickname: Option<String>,
    pub avatar: Option<String>,
    pub cursor: String,
    pub is_end: bool,
    pub articles: Vec<ParsedArticle>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WorkerEvent {
    Ready {
        dll_sha256: String,
        version: String,
    },
    Request {
        request: NativeRequest,
    },
    TargetDetected {
        request_id: String,
        payload: Value,
    },
    Response {
        request_id: String,
        ts: f64,
        payload: Value,
        #[serde(default)]
        session_path: Option<String>,
        #[serde(default)]
        evidence_path: Option<String>,
    },
    Fault {
        reason: String,
    },
    RequestFailed {
        request_id: String,
        ts: f64,
        reason: String,
    },
    Stopped,
}

/// 分层返回码，不把缺失码、HTTP 状态或旧接口错误码推断为原生请求成功。
#[derive(Clone, Debug, Default)]
pub struct ResponseCodes {
    pub outer: Option<i64>,
    pub jsapi: Option<i64>,
    pub inner: Option<i64>,
    pub rate_signal: bool,
    /// 服务端明确返回会话过期（如 H5 Session Timeout）：是业务结论，不是协议故障。
    pub session_timeout: bool,
}

fn number(v: Option<&Value>) -> Option<i64> {
    v.and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
}

pub fn response_body(payload: &Value) -> Result<Value> {
    if payload.get("transport").and_then(Value::as_str) == Some("mmtls") {
        ensure!(session_authenticated(payload), "会话响应未通过两层认证");
        let body = payload
            .get("body")
            .filter(|v| v.is_object())
            .context("会话响应缺少业务载荷")?;
        return Ok(body.clone());
    }
    let raw = payload
        .pointer("/jsapi_resp/resp_json")
        .context("响应缺少列表载荷")?;
    if let Some(s) = raw.as_str() {
        serde_json::from_str(s).context("列表载荷不是有效 JSON")
    } else {
        ensure!(raw.is_object(), "列表载荷不是对象");
        Ok(raw.clone())
    }
}

pub fn session_authenticated(payload: &Value) -> bool {
    payload.get("transport").and_then(Value::as_str) == Some("mmtls")
        && payload
            .get("authenticated_outer_records")
            .and_then(Value::as_u64)
            .is_some_and(|v| v >= 2)
        && payload.get("inner_authenticated").and_then(Value::as_bool) == Some(true)
}

pub fn response_codes(payload: &Value) -> ResponseCodes {
    let body = response_body(payload).ok();
    let message = payload
        .get("err_msg")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let error = payload
        .pointer("/jsapi_resp/error_msg")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let base_error = payload
        .pointer("/base_resp/error_msg")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let business_error = body
        .as_ref()
        .and_then(|v| v.pointer("/BaseResponse/ErrMsg"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let text = format!("{message} {error} {base_error} {business_error}").to_lowercase();
    ResponseCodes {
        outer: number(payload.pointer("/base_resp/ret")),
        jsapi: number(payload.pointer("/jsapi_resp/ret")),
        inner: body
            .as_ref()
            .and_then(|v| number(v.pointer("/BaseResponse/Ret"))),
        // 原生链路尚无经过实测的错误码词典，不能套用 getmsg 的 -3/-6 含义。
        rate_signal: [
            "频繁",
            "频率",
            "too frequent",
            "too many requests",
            "rate limit",
        ]
        .iter()
        .any(|word| text.contains(word)),
        session_timeout: text.contains("session timeout"),
    }
}

pub fn parse_page(
    payload: &Value,
    expected_user: &str,
    expected_biz: Option<&str>,
) -> Result<NativePage> {
    let codes = response_codes(payload);
    ensure!(
        (session_authenticated(payload) || (codes.outer == Some(0) && codes.jsapi == Some(0)))
            && codes.inner == Some(0),
        "接口返回异常或缺少返回码"
    );
    ensure!(
        session_authenticated(payload)
            || payload
                .get("err_msg")
                .and_then(Value::as_str)
                .is_some_and(|s| s.ends_with(":ok")),
        "桥调用未成功"
    );
    let body = response_body(payload)?;
    let user_name = body
        .pointer("/AccountInfo/UserName")
        .and_then(Value::as_str)
        .context("缺少公众号身份")?;
    ensure!(user_name == expected_user, "响应公众号与请求不一致");
    let list = body.get("MsgList").context("响应缺少列表容器")?;
    let messages = list
        .get("Msg")
        .and_then(Value::as_array)
        .context("响应缺少消息数组")?;
    let paging = list.get("PagingInfo").context("响应缺少翻页状态")?;
    let is_end = match number(paging.get("IsEnd")) {
        Some(0) => false,
        Some(1) => true,
        _ => bail!("未知结束标记"),
    };
    let cursor = paging
        .get("Offset")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    ensure!(is_end || !cursor.is_empty(), "未结束但缺少下一页游标");
    let mut biz = expected_biz.map(str::to_string);
    let mut articles = Vec::new();
    let mut seen = HashSet::new();
    for message in messages {
        let Some(app) = message.get("AppMsg") else {
            continue;
        };
        let published = number(app.pointer("/BaseInfo/CreateTime"))
            .filter(|t| *t > 0)
            .or_else(|| number(message.pointer("/BaseInfo/DateTime")).filter(|t| *t > 0));
        let Some(details) = app.get("DetailInfo").and_then(Value::as_array) else {
            continue;
        };
        for detail in details {
            let raw_url = detail
                .get("ContentUrl")
                .and_then(Value::as_str)
                .context("图文缺少链接")?;
            let content_url = raw_url.replace("&amp;", "&");
            let url = url::Url::parse(&content_url).context("文章链接无效")?;
            ensure!(
                matches!(url.scheme(), "http" | "https")
                    && url.host_str() == Some("mp.weixin.qq.com")
                    && url.path() == "/s",
                "文章链接不属于公众号文章"
            );
            let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
            let article_biz = params
                .get("__biz")
                .filter(|v| !v.is_empty())
                .context("文章链接缺少公众号标识")?;
            let mid = params
                .get("mid")
                .filter(|v| !v.is_empty())
                .context("文章链接缺少消息标识")?;
            let idx: i64 = params
                .get("idx")
                .context("文章链接缺少序号")?
                .parse()
                .context("文章序号无效")?;
            ensure!(idx > 0, "文章序号无效");
            if let Some(b) = &biz {
                ensure!(b == article_biz, "列表中混入其它公众号文章")
            } else {
                biz = Some(article_biz.clone())
            }
            if !seen.insert((mid.clone(), idx)) {
                continue;
            }
            let text = |key| {
                detail
                    .get(key)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            articles.push(ParsedArticle {
                mid: mid.clone(),
                idx,
                sn: params.get("sn").cloned(),
                title: text("Title"),
                digest: text("Digest"),
                content_url: Some(content_url),
                author: text("Author"),
                cover: text("CoverImgUrl"),
                published_at: published,
            });
        }
    }
    ensure!(!articles.is_empty() || is_end, "未结束但列表为空");
    let biz = biz.context("没有可核对的公众号标识")?;
    Ok(NativePage {
        biz,
        user_name: user_name.into(),
        nickname: body
            .pointer("/AccountInfo/NickName")
            .and_then(Value::as_str)
            .map(str::to_string),
        avatar: body
            .pointer("/AccountInfo/RoundHeadImgUrl")
            .and_then(Value::as_str)
            .map(str::to_string),
        cursor,
        is_end,
        articles,
    })
}

/// 校验请求与上一页的关系；初始页之后的其它首页请求不视为翻页。
pub fn validate_progress(
    pages: i64,
    previous_cursor: Option<&str>,
    request: &NativeRequest,
    page: &NativePage,
) -> Result<()> {
    if pages == 0 {
        ensure!(request.action == Some(0), "首个响应不是首页")
    } else {
        ensure!(request.action == Some(1), "采集中出现意外首页请求");
        ensure!(
            request.offset.as_deref() == previous_cursor,
            "请求未使用上页游标"
        );
    }
    ensure!(
        page.is_end || Some(page.cursor.as_str()) != request.offset.as_deref(),
        "游标未前进"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn payload() -> Value {
        json!({"base_resp":{"ret":0},"err_msg":"H5ExtTransfer:ok","jsapi_resp":{"ret":0,"resp_json":{
        "BaseResponse":{"Ret":0},"AccountInfo":{"UserName":"fake-account","NickName":"虚构公众号"},
        "MsgList":{"Msg":[{"BaseInfo":{"DateTime":100},"AppMsg":{"DetailInfo":[
            {"Title":"假文章一","ContentUrl":"https://mp.weixin.qq.com/s?__biz=FAKE&amp;mid=1&amp;idx=1&amp;sn=fake&amp;scene=126#rd"},
            {"Title":"假文章二","ContentUrl":"https://mp.weixin.qq.com/s?__biz=FAKE&mid=1&idx=2&sn=fake"}]}}],
            "PagingInfo":{"Offset":"fake-next","IsEnd":0}}}}})
    }
    #[test]
    fn parses_multiple_articles_and_preserves_link() {
        let p = parse_page(&payload(), "fake-account", Some("FAKE")).unwrap();
        assert_eq!(p.articles.len(), 2);
        assert!(p.articles[0]
            .content_url
            .as_ref()
            .unwrap()
            .ends_with("scene=126#rd"));
        assert_eq!(p.articles[0].published_at, Some(100));
    }
    #[test]
    fn rejects_wrong_account_and_missing_list() {
        assert!(parse_page(&payload(), "another", None).is_err());
        let mut p = payload();
        p["jsapi_resp"]["resp_json"]["MsgList"] = Value::Null;
        assert!(parse_page(&p, "fake-account", None).is_err());
    }
    #[test]
    fn session_requires_both_authentication_layers_without_inventing_bridge_codes() {
        let body = response_body(&payload()).unwrap();
        let mut session = json!({"transport":"mmtls","authenticated_outer_records":3,
            "inner_authenticated":true,"body":body});
        assert!(parse_page(&session, "fake-account", Some("FAKE")).is_ok());
        let codes = response_codes(&session);
        assert_eq!(codes.outer, None);
        assert_eq!(codes.jsapi, None);
        assert_eq!(codes.inner, Some(0));
        session["inner_authenticated"] = json!(false);
        assert!(parse_page(&session, "fake-account", Some("FAKE")).is_err());
        session["inner_authenticated"] = json!(true);
        session["authenticated_outer_records"] = json!(0);
        assert!(parse_page(&session, "fake-account", Some("FAKE")).is_err());
    }
    #[test]
    fn missing_ret_and_silent_empty_are_not_success() {
        let mut p = payload();
        p["base_resp"] = json!({});
        assert!(parse_page(&p, "fake-account", None).is_err());
        let mut p = payload();
        p["jsapi_resp"]["resp_json"]["MsgList"]["Msg"] = json!([]);
        assert!(parse_page(&p, "fake-account", Some("FAKE")).is_err());
    }
    #[test]
    fn checks_cursor_and_does_not_classify_legacy_code_as_rate_limit() {
        let page = parse_page(&payload(), "fake-account", None).unwrap();
        let req = NativeRequest {
            capture_only: false,
            transport: "native".into(),
            request_id: "fake".into(),
            ts: 0.,
            cgi: 5814,
            action: Some(1),
            offset: Some("before".into()),
            user_name: Some("fake-account".into()),
        };
        assert!(validate_progress(1, Some("before"), &req, &page).is_ok());
        assert!(validate_progress(1, Some("other"), &req, &page).is_err());
        assert!(!response_codes(&json!({"base_resp":{"ret":-6}})).rate_signal);
        assert!(response_codes(&json!({"err_msg":"H5ExtTransfer:fail 操作过于频繁"})).rate_signal);
    }
    #[test]
    fn session_timeout_error_is_its_own_classification() {
        // 服务端会话过期：认证已过、无列表载荷，只回错误结构。
        let payload = json!({"transport":"mmtls","authenticated_outer_records":3,"inner_authenticated":true,
            "body":{"BaseResponse":{"Ret":-262,"ErrMsg":"H5 Session Timeout"}}});
        let codes = response_codes(&payload);
        assert!(codes.session_timeout);
        assert_eq!(codes.inner, Some(-262));
        let reply = crate::workbench::engine::Reply {
            request_id: Some("fake".into()),
            sent_at: 0.,
            outcome: "response".into(),
            payload: payload.clone(),
            evidence: None,
            next_at: 0.,
        };
        assert_eq!(reply.classification(), "session_timeout");
        // 普通业务错误不因文本缺失被误判为会话过期。
        let other = json!({"transport":"mmtls","authenticated_outer_records":3,"inner_authenticated":true,
            "body":{"BaseResponse":{"Ret":-1,"ErrMsg":"其他错误"}}});
        assert!(!response_codes(&other).session_timeout);
        let other_reply = crate::workbench::engine::Reply {
            request_id: None,
            sent_at: 0.,
            outcome: "response".into(),
            payload: other,
            evidence: None,
            next_at: 0.,
        };
        assert_eq!(other_reply.classification(), "business_error");
    }
    #[test]
    fn repeated_cursor_wrong_biz_and_empty_end() {
        let mut raw = payload();
        let p = parse_page(&raw, "fake-account", None).unwrap();
        let req = NativeRequest {
            capture_only: false,
            transport: "native".into(),
            request_id: "fake".into(),
            ts: 0.,
            cgi: 5814,
            action: Some(1),
            offset: Some(p.cursor.clone()),
            user_name: Some("fake-account".into()),
        };
        assert!(validate_progress(1, Some(&p.cursor), &req, &p).is_err());
        assert!(parse_page(&raw, "fake-account", Some("OTHER")).is_err());
        raw["jsapi_resp"]["resp_json"]["MsgList"]["Msg"] = json!([]);
        raw["jsapi_resp"]["resp_json"]["MsgList"]["PagingInfo"]["IsEnd"] = json!(1);
        assert!(
            parse_page(&raw, "fake-account", Some("FAKE"))
                .unwrap()
                .is_end
        );
        raw["jsapi_resp"]["resp_json"]["BaseResponse"] =
            json!({"Ret":7,"ErrMsg":"too many requests"});
        assert!(response_codes(&raw).rate_signal);
    }
}
