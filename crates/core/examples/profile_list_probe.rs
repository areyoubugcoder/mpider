//! 以本机 profile 文件进行一次首页验证，复用 collect_list 闸门及留档。
//! 凭证只写入调用方指定的本机数据库，输出仅包含结果摘要。

use anyhow::{bail, Context, Result};
use mpider_core::{collector, model::CredentialFields, proxy_addon, store::Store};
use serde_json::{json, Value};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        bail!("用法：profile_list_probe <profile.json> <本机验证数据库>");
    }
    let profile: Value = serde_json::from_slice(&std::fs::read(&args[0])?)?;
    let captured = chrono::DateTime::parse_from_rfc3339(
        profile["CaptureTime"].as_str().context("缺少捕获时间")?,
    )?;
    let age = chrono::Utc::now().timestamp() - captured.timestamp();
    if !(0..1800).contains(&age) {
        bail!("profile 已超过本次验证的 30 分钟窗口，拒绝将旧凭证当作新凭证");
    }
    if profile["Host"] != "mp.weixin.qq.com" {
        bail!("profile 主机不匹配");
    }
    let cookie = profile["Cookie"].as_str().unwrap_or("");
    let request_url = profile["RequestUrl"].as_str().context("缺少请求路径")?;
    let creds = proxy_addon::extract_credentials(request_url, None, Some(cookie));
    let biz = creds.biz.context("缺少公众号标识")?;
    if creds.key.is_none() || creds.uin.is_none() {
        bail!("profile 缺少 key 或 uin");
    }
    let store = Store::open(&args[1])?;
    store.upsert_account(&biz, Some("验证公众号"), None)?;
    store.upsert_credential(
        &biz,
        &CredentialFields {
            uin: creds.uin,
            key: creds.key,
            pass_ticket: creds.pass_ticket,
            wxtoken: creds.wxtoken,
            x5: creds.x5,
            appmsg_token: creds.appmsg_token,
            cookie: Some(cookie.to_string()),
            extra: Some(json!({"user_agent":profile["UserAgent"]})),
        },
    )?;
    let plan = collector::ListPlan {
        max_pages: 1,
        source: collector::ListSource::Retry,
        ..collector::ListPlan::first_page()
    };
    let outcome = collector::collect_list(&store, &biz, &plan, 1800 - age, None).await;
    let calls: Vec<Value> = store
        .list_calls_since(0.0, 10)?
        .iter()
        .map(|call| {
            json!({"http_status":call.factors.http_status,"ret":call.factors.ret,
                          "outcome":call.outcome,"item_count":call.articles})
        })
        .collect();
    let result = match outcome {
        Ok(out) => json!({"request_completed":true,"list_available":out.total>0,
                         "profile_age_seconds":age,"total":out.total,
                         "pages":out.pages,"silent_empty":out.suspect_blocked,
                         "credential_expired":out.cred_expired,"calls":calls}),
        Err(err) => json!({"request_completed":false,"list_available":false,
                          "profile_age_seconds":age,
                          "credential_expired":collector::is_credential_expired_err(&err),
                          "rate_limited":mpider_core::wechat::is_rate_limited_err(&err),
                          "account_blocked":mpider_core::wechat::is_account_blocked_err(&err),
                          "calls":calls}),
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
