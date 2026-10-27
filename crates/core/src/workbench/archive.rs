//! 正文与图片下载：独立队列、统一 HTTP 闸门、图片补齐及本地阅读缓存。
use super::*;
use crate::{model::now, Store};
use anyhow::{bail, ensure, Context, Result};
use base64::Engine;
use scraper::{Html, Selector};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

const MAX_HTML: usize = 16 * 1024 * 1024;
const MAX_IMAGE: usize = 25 * 1024 * 1024;
static IMAGE_ACTIVE: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
struct ImageGuard;
impl Drop for ImageGuard {
    fn drop(&mut self) {
        IMAGE_ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let mut file = options.open(&tmp)?;
    file.write_all(data)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(tmp, path)?;
    Ok(())
}
fn escaped(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn active(store: &Store, id: i64) -> bool {
    store
        .wb_task(id)
        .is_ok_and(|t| ["queued", "running"].contains(&t.state.as_str()))
}

/// 所有正文、图片、重试与验证页对照均先入账；worker 数量不会放大同类发送频率。
struct Resource {
    bytes: Vec<u8>,
    request_id: String,
}
async fn http_get(
    store: &Store,
    client: &reqwest::Client,
    url: &str,
    category: &str,
    owner: i64,
    biz: &str,
    article_id: i64,
) -> Result<Resource> {
    let control = category == "body_control";
    let category = if control { "body" } else { category };
    let parsed = url::Url::parse(url)?;
    ensure!(
        ["http", "https"].contains(&parsed.scheme()),
        "资源链接协议无效"
    );
    let retries = store.wb_policy()?.retries;
    for attempt in 0..=retries {
        let request_id = loop {
            ensure!(active(store, owner), "任务已暂停或取消");
            store.wb_with_live_article(article_id, || Ok(()))?;
            ensure!(
                store.wb_policy()?.cooldown_until <= now(),
                "全局冷却中，任务已暂停"
            );
            let permit = store.wb_request_permit(
                category,
                if attempt == 0 {
                    if control {
                        "archive_control"
                    } else {
                        "archive"
                    }
                } else {
                    "archive_retry"
                },
                owner,
                biz,
                now(),
            )?;
            if let Some(id) = permit.request_id {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        store.system_log("archive","info","正文或图片请求已关联文章",json!({"request_id":request_id,"task_id":owner,"article_id":article_id,"biz":biz,"category":category}))?;
        let mut http_status = None;
        let outcome = async {
            let mut response = client.get(url).send().await?;
            let status = response.status();
            http_status = Some(status.as_u16());
            if status.as_u16() == 429 {
                store.wb_cooldown(now() + store.wb_policy()?.cooldown_seconds as f64)?;
                bail!("HTTP 429，全局冷却");
            }
            ensure!(status.is_success(), "HTTP {}", status.as_u16());
            let limit = if category == "image" {
                MAX_IMAGE
            } else {
                MAX_HTML
            };
            ensure!(
                response.content_length().is_none_or(|v| v <= limit as u64),
                "响应超过大小上限"
            );
            let mut data = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                ensure!(data.len() + chunk.len() <= limit, "响应超过大小上限");
                data.extend_from_slice(&chunk);
            }
            Ok::<_, anyhow::Error>(data)
        }
        .await;
        store.wb_request_finish(
            &request_id,
            if outcome.is_ok() {
                "ok"
            } else if http_status == Some(429) {
                "rate_limited"
            } else if http_status.is_some() {
                "http_error"
            } else {
                "network_error"
            },
            json!({"attempt":attempt,"http_status":http_status,"control":control}),
            now(),
        )?;
        match outcome {
            Ok(bytes) => return Ok(Resource { bytes, request_id }),
            Err(error) => {
                if attempt == retries || store.wb_policy()?.cooldown_until > now() {
                    return Err(error);
                }
            }
        }
    }
    unreachable!()
}

fn image_extension(data: &[u8]) -> Result<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok("png")
    } else if data.starts_with(&[255, 216, 255]) {
        Ok("jpg")
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Ok("gif")
    } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        Ok("webp")
    } else if data.get(4..8) == Some(b"ftyp")
        && [b"avif".as_slice(), b"avis".as_slice()].contains(&data.get(8..12).unwrap_or_default())
    {
        Ok("avif")
    } else {
        bail!("图片响应格式不受支持")
    }
}
fn image_key(url: &str) -> String {
    url::Url::parse(url)
        .map(|u| format!("{}{}", u.host_str().unwrap_or_default(), u.path()))
        .unwrap_or_else(|_| url.into())
}

async fn download_image(
    store: &Store,
    client: &reqwest::Client,
    url: &str,
    dir: &Path,
    task: i64,
    biz: &str,
    article_id: i64,
) -> Result<String> {
    let _guard = loop {
        ensure!(active(store, task), "任务已暂停或取消");
        store.wb_with_live_article(article_id, || Ok(()))?;
        let max = store.wb_policy()?.image_workers;
        let current = IMAGE_ACTIVE.load(Ordering::SeqCst);
        if current < max
            && IMAGE_ACTIVE
                .compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            break ImageGuard;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let hash = format!("{:x}", Sha256::digest(url.as_bytes()));
    for ext in ["png", "jpg", "gif", "webp", "avif"] {
        let relative = format!("images/{hash}.{ext}");
        let p = dir.join(&relative);
        if p.is_file() && std::fs::read(&p).is_ok_and(|data| image_extension(&data).is_ok()) {
            return Ok(relative);
        }
    }
    let response = http_get(store, client, url, "image", task, biz, article_id).await?;
    let ext = match image_extension(&response.bytes) {
        Ok(ext) => ext,
        Err(error) => {
            store.wb_request_content(&response.request_id, "invalid_image")?;
            return Err(error);
        }
    };
    let relative = format!("images/{hash}.{ext}");
    store.wb_with_live_article(article_id, || {
        atomic_write(&dir.join(&relative), &response.bytes)
    })?;
    Ok(relative)
}

/// 从正文节点提取图片，将每个 img 改写成可信本地资源或缺图提示，再做 HTML 清洗。
fn offline_body(html: &str, images: &HashMap<String, String>) -> String {
    let doc = Html::parse_document(html);
    let selector = Selector::parse("#js_content").unwrap();
    let fragment = doc
        .select(&selector)
        .next()
        .map(|v| v.inner_html())
        .unwrap_or_default();
    let body = Html::parse_fragment(&fragment);
    let mut output = fragment.clone();
    for img in body.select(&Selector::parse("img").unwrap()) {
        let raw = img
            .value()
            .attr("data-src")
            .or_else(|| img.value().attr("src"))
            .unwrap_or_default();
        let replacement = match images.get(&image_key(raw)) {
            Some(local) => format!(
                "<img src=\"{}\" alt=\"{}\" loading=\"lazy\">",
                escaped(local),
                escaped(img.value().attr("alt").unwrap_or_default())
            ),
            None => "<p>［图片待补齐］</p>".into(),
        };
        output = output.replace(&img.html(), &replacement);
    }
    ammonia::Builder::default()
        .url_relative(ammonia::UrlRelative::PassThrough)
        .clean(&output)
        .to_string()
}
fn shell(title: &str, body: &str) -> String {
    format!("<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\"><title>{}</title><style>body{{max-width:800px;margin:32px auto;padding:0 20px;font:16px/1.85 system-ui}}img{{max-width:100%;height:auto}}pre{{white-space:pre-wrap}}a{{color:#245a9a}}</style></head><body><h1>{}</h1>{}</body></html>",escaped(title),escaped(title),body)
}

async fn collect_article(
    store: Arc<Store>,
    root: PathBuf,
    task: i64,
    id: i64,
    client: reqwest::Client,
) -> Result<ArchiveRecord> {
    let (row, _) = store.get_article_md(id)?.context("文章不存在")?;
    ensure!(row.is_deleted == 0, "文章已删除");
    let url = row.content_url.as_deref().context("文章没有链接")?;
    let dir = root.join("archive/articles").join(id.to_string());
    store.wb_with_live_article(id, || {
        std::fs::create_dir_all(&dir)?;
        Ok(())
    })?;
    let cached = store.wb_cached_html(id)?;
    let mut request_id = None;
    let html = if let Some(html) = cached.filter(|s| !s.is_empty()) {
        html
    } else {
        let response = http_get(&store, &client, url, "body", task, &row.biz, id).await?;
        request_id = Some(response.request_id.clone());
        match String::from_utf8(response.bytes) {
            Ok(html) => html,
            Err(_) => {
                store.wb_request_content(&response.request_id, "invalid_encoding")?;
                bail!("正文编码无效");
            }
        }
    };
    store.wb_with_live_article(id, || {
        atomic_write(&dir.join("original.html"), html.as_bytes())
    })?;
    let parsed = match article_md::parse_html(url, &html) {
        Ok(value) => value,
        Err(article_md::ParseError::VerifyPage) => {
            if let Some(id) = &request_id {
                store.wb_request_content(id, "verification_required")?;
            }
            if let Some(control) = store.wb_control_url(id)? {
                let response = http_get(
                    &store,
                    &client,
                    &control,
                    "body_control",
                    task,
                    &row.biz,
                    id,
                )
                .await?;
                match article_md::parse_html(&control, &String::from_utf8_lossy(&response.bytes)) {
                    Err(article_md::ParseError::VerifyPage) => {
                        store.wb_request_content(&response.request_id, "verification_required")?;
                        store.wb_cooldown(now() + store.wb_policy()?.cooldown_seconds as f64)?;
                        bail!("对照文章同样要求验证，全局冷却");
                    }
                    Ok(parsed) if parsed.markdown.as_ref().is_some_and(|v| !v.is_empty()) => {}
                    _ => {
                        store.wb_request_content(&response.request_id, "empty_body")?;
                        store.wb_task_state(
                            task,
                            "paused",
                            Some("验证页对照无有效正文，证据不足；请检查后继续"),
                        )?;
                    }
                }
            } else {
                store.wb_task_state(
                    task,
                    "paused",
                    Some("正文要求验证，缺少已知有效对照；未判定限流，请检查后继续"),
                )?;
            }
            bail!("正文要求验证，结果未判定为限流，请人工检查链接")
        }
        Err(article_md::ParseError::Unavailable(reason)) => {
            if let Some(id) = &request_id {
                store.wb_request_content(id, "unavailable")?;
            }
            store.mark_article_unavailable(id, &reason)?;
            bail!("正文不可用：{reason}")
        }
    };
    let markdown = parsed.markdown.clone().filter(|v| !v.trim().is_empty());
    let markdown = match markdown {
        Some(value) => value,
        None => {
            if let Some(id) = &request_id {
                store.wb_request_content(id, "empty_body")?;
            }
            bail!("未解析到正文");
        }
    };
    store.wb_save_article_body(id, &html, &markdown, parsed.title.as_deref())?;
    // DOM 借用在 await 前释放，后台 worker 保持 Send。
    let mut urls: Vec<String> = {
        let doc = Html::parse_document(&html);
        let selector = Selector::parse("#js_content img").unwrap();
        doc.select(&selector)
            .filter_map(|img| {
                img.value()
                    .attr("data-src")
                    .or_else(|| img.value().attr("src"))
            })
            .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
            .map(str::to_string)
            .collect()
    };
    if let Some(cover) = parsed.cover.clone().or(store.wb_article_cover(id)?) {
        urls.push(cover)
    }
    let mut seen = HashSet::new();
    urls.retain(|u| seen.insert(image_key(u)));
    let mut image_tasks = tokio::task::JoinSet::new();
    let mut images = HashMap::new();
    let mut missing = Vec::new();
    for u in urls {
        let s = store.clone();
        let c = client.clone();
        let d = dir.clone();
        let biz = row.biz.clone();
        image_tasks.spawn(async move {
            let r = download_image(&s, &c, &u, &d, task, &biz, id).await;
            (u, r)
        });
    }
    while let Some(result) = image_tasks.join_next().await {
        let (u, result) = result?;
        match result {
            Ok(path) => {
                images.insert(image_key(&u), path);
            }
            Err(_) => missing.push(u),
        }
    }
    let title = parsed
        .title
        .as_deref()
        .or(row.title.as_deref())
        .unwrap_or("无标题");
    let mut local_md = markdown;
    let image_re = regex::Regex::new(r"!\[([^\]]*)\]\(([^)]+)\)").unwrap();
    local_md = image_re
        .replace_all(&local_md, |cap: &regex::Captures<'_>| {
            match images.get(&image_key(&cap[2])) {
                Some(path) => format!("![{}]({})", &cap[1], path),
                None => "［图片待补齐］".into(),
            }
        })
        .into_owned();
    let body = offline_body(&html, &images);
    // 特殊文章没有标准正文节点时仍提供可离线阅读的纯文本 Markdown。
    let body = if body.trim().is_empty() {
        format!("<pre>{}</pre>", escaped(&local_md))
    } else {
        body
    };
    store.wb_with_live_article(id, || {
        atomic_write(&dir.join("index.html"), shell(title, &body).as_bytes())
    })?;
    store.wb_with_live_article(id, || {
        atomic_write(&dir.join("article.md"), local_md.as_bytes())
    })?;
    let state = if missing.is_empty() {
        "complete"
    } else {
        "partial"
    };
    let record = ArchiveRecord {
        article_id: id,
        state: state.into(),
        directory: dir.display().to_string(),
        missing_images: missing.clone(),
        updated_at: now(),
        error: None,
    };
    store.wb_with_live_article(id, || {
        atomic_write(
            &dir.join("metadata.json"),
            &serde_json::to_vec_pretty(
                &json!({"article":row,"archive":record,"images":images,"missing_images":missing}),
            )?,
        )
    })?;
    store.wb_with_live_article(id, || write_manifest(&dir))?;
    store.wb_save_archive(&record)?;
    Ok(record)
}

fn files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let meta = std::fs::symlink_metadata(&path)?;
        ensure!(
            !meta.file_type().is_symlink(),
            "归档目录包含链接，已停止导出"
        );
        if meta.is_dir() {
            result.extend(files(&path)?)
        } else if meta.is_file() {
            result.push(path)
        }
    }
    result.sort();
    Ok(result)
}
fn write_manifest(dir: &Path) -> Result<()> {
    let mut rows = Vec::new();
    for path in files(dir)? {
        if path.file_name().is_some_and(|s| s == "manifest.json") {
            continue;
        }
        let data = std::fs::read(&path)?;
        rows.push(json!({"path":path.strip_prefix(dir)?.to_string_lossy().replace('\\',"/"),"bytes":data.len(),"sha256":format!("{:x}",Sha256::digest(&data))}));
    }
    atomic_write(
        &dir.join("manifest.json"),
        &serde_json::to_vec_pretty(&rows)?,
    )
}

/// 阅读接口仅接受文章 ID；内联已归档图片，不授予 WebView 任意磁盘访问权限。
pub fn reader_html(store: &Store, root: &Path, id: i64) -> Result<String> {
    let record = store.wb_archive(id)?.context("请先下载正文")?;
    let dir = root
        .join("archive/articles")
        .join(id.to_string())
        .canonicalize()?;
    ensure!(
        Path::new(&record.directory).canonicalize()? == dir,
        "正文缓存路径不一致"
    );
    let mut html = std::fs::read_to_string(dir.join("index.html"))?;
    let re = regex::Regex::new(r#"src="(images/[a-f0-9]+\.(?:png|jpg|gif|webp|avif))""#)?;
    let mut replacements = Vec::new();
    for cap in re.captures_iter(&html) {
        let path = dir.join(&cap[1]).canonicalize()?;
        ensure!(path.starts_with(&dir), "图片路径超出归档目录");
        let data = std::fs::read(path)?;
        let ext = image_extension(&data)?;
        let mime = if ext == "jpg" { "jpeg" } else { ext };
        replacements.push((
            cap[0].to_string(),
            format!(
                "src=\"data:image/{};base64,{}\"",
                mime,
                base64::engine::general_purpose::STANDARD.encode(data)
            ),
        ));
    }
    for (from, to) in replacements {
        html = html.replace(&from, &to)
    }
    Ok(html)
}

/// 排队期间可能由其他入口完成正文；跳过发生在申请任何 HTTP 许可之前。
async fn download_article(
    store: Arc<Store>,
    root: PathBuf,
    task: i64,
    id: i64,
    client: reqwest::Client,
) -> Result<Option<ArchiveRecord>> {
    if store.wb_body_available(id)? {
        return Ok(None);
    }
    collect_article(store, root, task, id, client)
        .await
        .map(Some)
}

pub async fn run(store: Arc<Store>, root: PathBuf) {
    let client=match reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(30)).user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/130.0.0.0 Safari/537.36").build(){Ok(c)=>c,Err(_)=>return};
    loop {
        // 删除缓存可能暂被阅读器占用，保留在队列中跨重启重试。
        let _ = store.wb_cleanup_deleted_archives(&root);
        let candidate = store
            .wb_runnable_tasks(true, now())
            .unwrap_or_default()
            .into_iter()
            .filter(|t| t.kind == "archive" && ["queued", "running"].contains(&t.state.as_str()))
            .min_by_key(|t| t.id);
        if let Some(mut task) = candidate {
            if store.wb_policy().is_ok_and(|p| p.cooldown_until > now()) {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            let result = async {
                if task.state == "queued" {
                    store.wb_task_state(task.id, "running", None)?;
                }
                let ids: Vec<i64> = serde_json::from_value(task.target["article_ids"].clone())?;
                let mut queue: std::collections::VecDeque<i64> = ids
                    .into_iter()
                    .filter(|id| !task.progress.finished_ids.contains(id))
                    .collect();
                let mut workers = tokio::task::JoinSet::new();
                loop {
                    let max = store.wb_policy()?.body_workers;
                    while workers.len() < max && active(&store, task.id) {
                        let Some(id) = queue.pop_front() else { break };
                        let s = store.clone();
                        let r = root.clone();
                        let c = client.clone();
                        let task_id = task.id;
                        workers.spawn(async move {
                            let result = download_article(s, r, task_id, id, c).await;
                            (id, result)
                        });
                    }
                    let Some(result) = workers.join_next().await else {
                        break;
                    };
                    let (id, result) = result?;
                    task = store.wb_task(task.id)?;
                    if !task.target["article_ids"]
                        .as_array()
                        .is_some_and(|ids| ids.iter().any(|value| value.as_i64() == Some(id)))
                    {
                        continue;
                    }
                    if !["queued", "running"].contains(&task.state.as_str()) {
                        continue;
                    }
                    if store.wb_policy()?.cooldown_until > now() {
                        store.wb_task_state(
                            task.id,
                            "paused",
                            Some("全局冷却，正文与图片进度已保存"),
                        )?;
                        continue;
                    }
                    let outcome = match &result {
                        Ok(None) => "skipped",
                        Ok(Some(record)) => record.state.as_str(),
                        Err(_) => "failed",
                    };
                    store.system_log(
                        "archive",
                        if outcome == "failed" { "error" } else { "info" },
                        "单篇文章下载处理结束",
                        json!({"task_id":task.id,"article_id":id,"outcome":outcome}),
                    )?;
                    task.progress.finished_ids.push(id);
                    task.progress.completed += 1;
                    match result {
                        Ok(None) => {
                            task.progress.skipped += 1;
                        }
                        Ok(Some(a)) => {
                            if a.state == "partial" {
                                task.progress.partial += 1
                            }
                        }
                        Err(error) => {
                            task.progress.failed += 1;
                            if store.wb_archive(id)?.is_none_or(|a| a.state == "failed") {
                                store.wb_save_archive(&ArchiveRecord {
                                    article_id: id,
                                    state: "failed".into(),
                                    directory: root
                                        .join("archive/articles")
                                        .join(id.to_string())
                                        .display()
                                        .to_string(),
                                    missing_images: Vec::new(),
                                    updated_at: now(),
                                    error: Some(crate::applog::redact(&error.to_string())),
                                })?;
                            }
                        }
                    }
                    task.updated_at = now();
                    store.wb_save_task(&task)?;
                }
                task = store.wb_task(task.id)?;
                if ["queued", "running"].contains(&task.state.as_str()) {
                    let unfinished = task.target["article_ids"].as_array().is_some_and(|ids| {
                        ids.iter()
                            .filter_map(|v| v.as_i64())
                            .any(|id| !task.progress.finished_ids.contains(&id))
                    });
                    if unfinished {
                        task.state = "queued".into();
                        task.reason = Some("下载目标已更新，继续处理剩余文章".into());
                        store.wb_save_task(&task)?;
                        return Ok::<_, anyhow::Error>(());
                    }
                    task.state = "done".into();
                    task.updated_at = now();
                    task.reason = Some(format!(
                        "已处理 {} 篇，跳过 {} 篇，缺图 {} 篇，失败 {} 篇",
                        task.progress.completed,
                        task.progress.skipped,
                        task.progress.partial,
                        task.progress.failed
                    ));
                    store.wb_save_task(&task)?;
                    super::engine::record_task_finish(&store, &task);
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            if let Err(error) = result {
                let _ = store.wb_task_state(
                    task.id,
                    "failed",
                    Some(&format!(
                        "归档执行或导出失败：{}",
                        crate::applog::redact(&error.to_string())
                    )),
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
