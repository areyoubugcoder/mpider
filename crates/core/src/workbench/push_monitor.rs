//! 本地推送监测：只读发现文章链接，持久化候选后按统一闸门解析网页并入库。
//! 不加载第三方程序，不获取会话密钥，不触发微信窗口操作。
use crate::{model::now, Store};
use anyhow::{ensure, Context, Result};
use regex::bytes::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, LazyLock,
    },
    time::Duration,
};

#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::WindowsScanner;

pub const PURPOSE: &str = "push_metadata";
pub const MAX_CANDIDATES: usize = 10_000;
const MAX_HTML: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub scan_seconds: u64,
    pub recent_hours: u64,
    pub hourly_budget: i64,
    /// 空集合表示全部公众号；使用稳定 biz，不按显示名称匹配。
    pub bizzes: Vec<String>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            scan_seconds: 10,
            recent_hours: 24,
            hourly_budget: 60,
            bizzes: vec![],
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (5..=300).contains(&self.scan_seconds),
            "扫描间隔须为 5 至 300 秒"
        );
        ensure!(
            (1..=168).contains(&self.recent_hours),
            "文章时间窗须为 1 至 168 小时"
        );
        ensure!(
            (1..=120).contains(&self.hourly_budget),
            "每小时网页请求上限须为 1 至 120"
        );
        ensure!(
            self.bizzes.len() <= 1000 && self.bizzes.iter().all(|b| valid_biz(b)),
            "公众号范围无效"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Run {
    pub id: i64,
    pub active: bool,
    pub phase: String,
    pub config: Config,
    pub started_at: f64,
    pub last_scan_at: f64,
    pub next_scan_at: f64,
    pub next_fetch_at: f64,
    pub processes: usize,
    pub read_bytes: u64,
    pub rounds: u64,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct MonitorEvent {
    pub id: i64,
    pub first_seen_at: f64,
    pub finished_at: Option<f64>,
    pub outcome: String,
    pub nickname: String,
    pub title: String,
    pub biz: String,
    pub article_id: Option<i64>,
    pub published_at: Option<i64>,
    pub reason: Option<String>,
}
#[derive(Serialize)]
pub struct Status {
    pub supported: bool,
    pub run: Run,
    pub pending: i64,
    pub imported: i64,
    pub requests_hour: i64,
    pub events: Vec<MonitorEvent>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub key: String,
    pub url: String,
    pub biz: Option<String>,
}
#[derive(Debug)]
pub struct Candidate {
    pub id: i64,
    pub url: String,
    pub attempts: u32,
    pub first_seen_at: f64,
}
#[derive(Default)]
pub struct Scan {
    pub links: Vec<Link>,
    pub processes: usize,
    pub read_bytes: u64,
    pub inaccessible: usize,
}
pub trait Scanner: Send + 'static {
    fn scan(&mut self, cancel: &AtomicBool) -> Result<Scan>;
}

fn valid_biz(biz: &str) -> bool {
    !biz.is_empty()
        && biz.len() <= 256
        && biz
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"+/=_-".contains(&c))
}
fn digits(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_digit())
}

/// 丢弃跟踪及会话参数；只接受微信文章域名、标准端口与有效的文章身份。
pub fn normalize(raw: &str) -> Option<Link> {
    let cleaned = raw
        .replace("\\/", "/")
        .replace("&amp;", "&")
        .replace("&#38;", "&");
    let parsed = url::Url::parse(&cleaned).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str()? != "mp.weixin.qq.com"
        || parsed.port().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }
    let mut url = url::Url::parse("https://mp.weixin.qq.com").ok()?;
    if let Some(short) = parsed.path().strip_prefix("/s/") {
        if !(20..=24).contains(&short.len())
            || !short
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
        {
            return None;
        }
        url.set_path(parsed.path());
        return Some(Link {
            key: format!("short:{short}"),
            url: url.into(),
            biz: None,
        });
    }
    if parsed.path() != "/s" {
        return None;
    }
    let mut values = BTreeMap::new();
    for (k, v) in parsed.query_pairs() {
        if ["__biz", "mid", "idx", "sn", "chksm"].contains(&k.as_ref()) {
            let value = percent_encoding::percent_decode_str(&v)
                .decode_utf8()
                .ok()?
                .into_owned();
            if values.insert(k.into_owned(), value).is_some() {
                return None;
            }
        }
    }
    let biz = values.get("__biz")?;
    let mid = values.get("mid")?;
    let idx: i64 = values
        .get("idx")
        .map(String::as_str)
        .unwrap_or("1")
        .parse()
        .ok()?;
    let sn = values.get("sn")?;
    if !valid_biz(biz)
        || !digits(mid)
        || !(1..=100).contains(&idx)
        || sn.len() != 32
        || !sn.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return None;
    }
    url.set_path("/s");
    url.query_pairs_mut()
        .append_pair("__biz", biz)
        .append_pair("mid", mid)
        .append_pair("idx", &idx.to_string())
        .append_pair("sn", &sn.to_ascii_lowercase());
    if let Some(check) = values
        .get("chksm")
        .filter(|s| (16..=64).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_hexdigit()))
    {
        url.query_pairs_mut().append_pair("chksm", check);
    }
    Some(Link {
        key: format!("article:{biz}:{mid}:{idx}"),
        url: url.into(),
        biz: Some(biz.clone()),
    })
}

/// 每块同时识别 ASCII 与 UTF-16LE；只匹配带域名的链接，拒绝任意 /s/ 噪声。
pub fn discover(data: &[u8]) -> Vec<Link> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)(?:https?:(?:/|\\/){2})?mp\.weixin\.qq\.com(?:/|\\/)s(?:\?[^\x00-\x20"'<>\\]{1,1500}|(?:/|\\/)[A-Za-z0-9_-]{20,24})"#).unwrap()
    });
    let mut found = BTreeMap::<String, Link>::new();
    let mut scan = |bytes: &[u8]| {
        for m in RE.find_iter(bytes).take(MAX_CANDIDATES) {
            let Ok(raw) = std::str::from_utf8(m.as_bytes()) else {
                continue;
            };
            let raw = if raw.starts_with("http") {
                raw.to_owned()
            } else {
                format!("https://{raw}")
            };
            if let Some(link) = normalize(&raw) {
                let replace = found
                    .get(&link.key)
                    .is_none_or(|old| !old.url.contains("chksm=") && link.url.contains("chksm="));
                if replace && found.len() < MAX_CANDIDATES {
                    found.insert(link.key.clone(), link);
                }
            }
        }
    };
    scan(data);
    // 两种对齐均检查；非 ASCII 单元插入分隔符，禁止跨字符串拼接。
    for offset in [0, 1] {
        let ascii: Vec<u8> = data[offset.min(data.len())..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| if p[1] == 0 && p[0] < 128 { p[0] } else { 0 })
            .collect();
        scan(&ascii);
    }
    found.into_values().collect()
}

#[derive(Clone, Debug)]
pub struct Metadata {
    pub biz: String,
    pub mid: String,
    pub idx: i64,
    pub sn: String,
    pub url: String,
    pub nickname: String,
    pub title: String,
    pub published_at: i64,
    pub digest: Option<String>,
    pub cover: Option<String>,
}

pub fn metadata(link: &Link, html: &str) -> Result<Metadata> {
    let real_body = ["js_content", "js_text_desc", "picture_page_info_list"]
        .iter()
        .any(|s| html.contains(s));
    ensure!(
        real_body
            || ![
                "环境异常",
                "安全验证",
                "完成验证后",
                "访问过于频繁",
                "请输入验证码"
            ]
            .iter()
            .any(|s| html.contains(s)),
        "verification_required"
    );
    let parsed = article_md::parse_html(&link.url, html).map_err(|e| match e {
        article_md::ParseError::VerifyPage => anyhow::anyhow!("verification_required"),
        article_md::ParseError::Unavailable(_) => anyhow::anyhow!("unavailable"),
    })?;
    let fallback = crate::proxy_addon::parse_article_meta_from_html(Some(html));
    let biz = parsed
        .biz
        .or_else(|| fallback.get("biz").cloned())
        .context("缺少公众号身份")?;
    let mid = parsed
        .mid
        .or_else(|| fallback.get("mid").cloned())
        .context("缺少文章标识")?;
    let idx = parsed
        .idx
        .or_else(|| fallback.get("idx").and_then(|s| s.parse().ok()))
        .context("缺少图文序号")?;
    let sn = parsed
        .sn
        .or_else(|| fallback.get("sn").cloned())
        .context("缺少文章签名")?;
    let mut url = url::Url::parse("https://mp.weixin.qq.com/s")?;
    url.query_pairs_mut()
        .append_pair("__biz", &biz)
        .append_pair("mid", &mid)
        .append_pair("idx", &idx.to_string())
        .append_pair("sn", &sn);
    // 原长链的 chksm 可保留，但以网页自述身份与已验证输入交叉核对。
    if let Some(check) = url::Url::parse(&link.url)?
        .query_pairs()
        .find(|(k, _)| k == "chksm")
        .map(|(_, v)| v.into_owned())
    {
        url.query_pairs_mut().append_pair("chksm", &check);
    }
    let canonical = normalize(url.as_str()).context("网页文章身份无效")?;
    if link.biz.is_some() {
        ensure!(canonical.key == link.key, "网页与发现链接的文章身份不一致");
    }
    let nickname = parsed
        .mp_name
        .or_else(|| crate::proxy_addon::parse_account_name_from_html(Some(html)))
        .filter(|v| !v.trim().is_empty())
        .context("缺少公众号名称")?;
    let doc = scraper::Html::parse_document(html);
    let title = parsed
        .title
        .or_else(|| {
            doc.select(&scraper::Selector::parse("#activity-name").unwrap())
                .next()
                .map(|n| n.text().collect::<String>().trim().to_owned())
        })
        .filter(|s| !s.is_empty())
        .context("缺少文章标题")?;
    static CT: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"(?:var\s+(?:ct|oriCreateTime)\s*=\s*["']?)(\d{9,12})"#).unwrap()
    });
    let published_at = parsed
        .publish_time
        .or_else(|| CT.captures(html).and_then(|c| c[1].parse().ok()))
        .context("缺少发布时间")?;
    ensure!(
        published_at > 0 && nickname.len() <= 1024 && title.len() <= 4096,
        "网页元信息无效"
    );
    Ok(Metadata {
        biz,
        mid,
        idx,
        sn,
        url: canonical.url,
        nickname,
        title,
        published_at,
        digest: parsed.digest,
        cover: parsed.cover,
    })
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(20))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36")
        // 每次 HTTP 发送都须记账；拒绝自动跟随重定向，避免隐式请求与跨域访问。
        .redirect(reqwest::redirect::Policy::none()).build()?)
}

async fn fetch(client: &reqwest::Client, url: &str) -> Result<(u16, String)> {
    let mut response = client.get(url).send().await?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Ok((status, String::new()));
    }
    ensure!(
        response
            .content_length()
            .is_none_or(|v| v <= MAX_HTML as u64),
        "网页超过大小上限"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(bytes.len() + chunk.len() <= MAX_HTML, "网页超过大小上限");
        bytes.extend_from_slice(&chunk);
    }
    Ok((status, String::from_utf8(bytes).context("网页不是 UTF-8")?))
}

/// 显式开启后才运行；桌面启动只将旧运行置为停止，保留队列和预算。
pub async fn run(store: Arc<Store>, id: i64) {
    #[cfg(windows)]
    let result = run_with_scanner(store.clone(), id, windows::WindowsScanner::default()).await;
    #[cfg(not(windows))]
    let result: Result<()> = Err(anyhow::anyhow!("推送监测仅支持 Windows"));
    if let Err(error) = result {
        let _ = store.push_end(
            id,
            "failed",
            Some(&crate::store::system_logs::safe_text(&error.to_string())),
        );
    }
}

pub async fn run_with_scanner<S: Scanner>(
    store: Arc<Store>,
    id: i64,
    mut scanner: S,
) -> Result<()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = LOCK.lock().await;
    let client = client()?;
    while store.push_is_active(id)? {
        let run = store.push_run()?;
        if now() >= run.next_scan_at {
            let cancel = Arc::new(AtomicBool::new(false));
            let signal = cancel.clone();
            let mut task = tokio::task::spawn_blocking(move || {
                let result = scanner.scan(&signal);
                (scanner, result)
            });
            let (returned, result) = loop {
                tokio::select! {
                    result = &mut task => { break result?; }
                    _ = tokio::time::sleep(Duration::from_millis(200)) => {
                        if !store.push_is_active(id)? { cancel.store(true, Ordering::Relaxed); }
                    }
                }
            };
            scanner = returned;
            if !store.push_is_active(id)? {
                break;
            }
            let scan = result?;
            store.push_scanned(id, scan, now())?;
        }
        if let Some(candidate) = store.push_next(id, now())? {
            process(&store, &client, id, candidate).await?;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Ok(())
}

async fn process(
    store: &Store,
    client: &reqwest::Client,
    id: i64,
    candidate: Candidate,
) -> Result<()> {
    let endpoint = candidate.url.clone();
    process_at(store, client, id, candidate, &endpoint).await
}

// endpoint 仅由生产调用方传入已验证候选；测试可注入本机 HTTP fixture，不访问微信。
async fn process_at(
    store: &Store,
    client: &reqwest::Client,
    id: i64,
    candidate: Candidate,
    endpoint: &str,
) -> Result<()> {
    let link = normalize(&candidate.url).context("候选链接已损坏")?;
    let permit = store.wb_request_permit(
        "body",
        PURPOSE,
        id,
        link.biz.as_deref().unwrap_or(""),
        now(),
    )?;
    let Some(request_id) = permit.request_id else {
        store.push_deferred(id, permit.next_at, permit.reason.as_deref())?;
        return Ok(());
    };
    if !store.push_claim(id, candidate.id)? {
        store.wb_request_finish(&request_id, "cancelled", serde_json::json!({}), now())?;
        return Ok(());
    }
    let response = fetch(client, endpoint);
    tokio::pin!(response);
    let result = loop {
        tokio::select! {
            result = &mut response => break result,
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                if !store.push_is_active(id)? {
                    store.wb_request_finish(&request_id, "cancelled", serde_json::json!({}), now())?;
                    return Ok(());
                }
            }
        }
    };
    let (outcome, meta, retry, cooldown, status) = match result {
        Ok((429, _)) => ("rate_limited", None, true, true, Some(429)),
        Ok((code, _)) if !(200..300).contains(&code) => {
            ("http_error", None, code >= 500, false, Some(code))
        }
        Ok((code, html)) => match metadata(&link, &html) {
            Ok(meta) => ("ok", Some(meta), false, false, Some(code)),
            Err(e) if e.to_string() == "verification_required" => {
                ("verification_required", None, true, true, Some(code))
            }
            Err(e) if e.to_string() == "unavailable" => {
                ("unavailable", None, false, false, Some(code))
            }
            Err(_) => ("invalid_metadata", None, true, false, Some(code)),
        },
        Err(_) => ("network_error", None, true, false, None),
    };
    store.wb_request_finish(
        &request_id,
        outcome,
        serde_json::json!({"http_status":status}),
        now(),
    )?;
    if cooldown {
        store.wb_cooldown(now() + 7200.)?;
    }
    if let Some(meta) = meta {
        store.push_import(id, candidate.id, &meta, now())?;
    } else {
        store.push_result(
            id,
            candidate.id,
            outcome,
            retry,
            if cooldown { now() + 7200. } else { now() + 60. },
        )?;
    }
    Ok(())
}
