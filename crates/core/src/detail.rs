//! 文章正文补采（detail）：对 `detail_done=0` 的文章并发抓公开 /s 页并解析为 Markdown。
//!
//! 对齐 Python `collector.collect_detail` 的行为：
//! - 网络抓取 + 解析（article-md 库）在 worker 任务里并发；**入库写串行**在聚合循环里做
//!   （SQLite 单写者，避免写冲突）；
//! - 节流是**每 worker** 的：每个 worker 请求后各自睡 `throttle_ms`，整体速率
//!   ≈ workers/throttle_ms；worker 启动时错峰（与 Python 基准的差异，Python 是
//!   ThreadPoolExecutor 齐发），避免「同一瞬间 workers 个请求」的突发指纹。
//!   要真正的「每 throttle_ms 一篇」串行，把 workers 设为 1；
//! - 某篇命中人机验证页时**先做对照确认**（再抓一条已知有效 URL）：对照也验证页
//!   才置停止位（真限流，其余任务跳过）；对照正常则按该篇失败计数——实测存在
//!   无效 sn 的短链稳定返回验证页，没有对照会把整批误判成被限流（与 Python 的差异）；
//! - 解析主实现是 [`article_md`]（六种版式 → Markdown），失败回退
//!   [`crate::wechat::parse_article_html`] 抽纯文本。
//!
//! 公开 /s 页无需凭证，因此本模块可独立于抓凭证整链单独运行（全局/单号/单篇入口共用）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;
use tokio::sync::mpsc;

use crate::applog::{self, Stage};
use crate::model::{ArticleFields, ArticleRow};
use crate::store::Store;

/// 每篇完成后回调一次的结构化进度（GUI 进度条 + 逐行日志共用）。
#[derive(Clone, Debug, Serialize)]
pub struct DetailProgress {
    pub completed: usize,
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    pub line: String,
}

/// 一批补采的汇总结果（对齐 Python 返回 dict 的键）。
#[derive(Clone, Debug, Default, Serialize)]
pub struct DetailSummary {
    pub done: usize,
    pub candidates: usize,
    pub failed: usize,
    pub skipped: usize,
    /// 被标记为永久不可用的篇数（微信提示页：已删除/违规/暂不可看）。
    pub unavailable: usize,
    pub rate_limited: bool,
    pub feedback: String,
}

/// 补采配置。
#[derive(Clone, Debug)]
pub struct DetailConfig {
    /// 指定公众号；None = 全部。
    pub biz: Option<String>,
    /// 指定单篇文章 id（「抓取详情」按钮）；None = 按 biz/全部取候选。
    pub article_id: Option<i64>,
    /// 本批最多处理多少篇；`None` = 不设上限，把当前全部待补一次处理完（GUI「抓取全部详情」走这里）。
    pub limit: Option<i64>,
    /// 并发 worker 数（1..=12）。
    pub workers: usize,
    /// 每次请求后的节流延时（毫秒）。
    pub throttle_ms: u64,
}

impl Default for DetailConfig {
    fn default() -> Self {
        Self {
            biz: None,
            article_id: None,
            limit: None,
            workers: 4,
            throttle_ms: 1000,
        }
    }
}

const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36";

/// 单篇抓取结果分类（worker → 聚合循环）。
enum FetchOutcome {
    Ok(Box<article_md::ArticleResult>),
    /// article-md 没解出正文，但旧解析抽到了纯文本。
    TextOnly {
        title: Option<String>,
        author: Option<String>,
        text: String,
    },
    /// 微信提示页（已删除/违规/暂不可看）：永久不可用，标记后不再重试。
    Unavailable(String),
    Rate,
    Skip,
    Fail(String),
}

/// 并发补正文主入口。`progress` 每完成一篇回调一次（含最终汇总行）。
pub async fn collect_detail(
    store: Arc<Store>,
    cfg: DetailConfig,
    progress: impl Fn(DetailProgress) + Send + Sync + 'static,
) -> Result<DetailSummary> {
    let workers = cfg.workers.clamp(1, 12);
    let pending = store.articles_pending_detail(cfg.biz.as_deref(), cfg.article_id, cfg.limit)?;
    let total = pending.len();

    // 每行同时写环节日志（入库 + GUI 日志页）与结构化进度回调（进度条）。
    let emit = |p: DetailProgress| {
        let line = p.line.clone();
        if line.contains('⛔') || line.contains('✗') {
            applog::warn(Stage::Detail, line);
        } else {
            applog::info(Stage::Detail, line);
        }
        progress(p)
    };
    emit(DetailProgress {
        completed: 0,
        total,
        done: 0,
        failed: 0,
        line: format!(
            "▶ 开始补正文：共 {total} 篇（库=article-md，并发={workers}，节流={}ms/请求）",
            cfg.throttle_ms
        ),
    });

    let mut summary = DetailSummary {
        candidates: total,
        ..Default::default()
    };
    if total == 0 {
        summary.feedback = "没有待补详情的文章".to_string();
        emit(DetailProgress {
            completed: 0,
            total,
            done: 0,
            failed: 0,
            line: format!("✅ {}", summary.feedback),
        });
        return Ok(summary);
    }

    // 直连 + no_proxy——与 wechat.rs 同理：抓凭证的 MITM 代理开着时（系统代理），公开 /s 页请求
    // 不能被劫持进自家代理。
    let client = reqwest::Client::builder()
        .user_agent(UA)
        .timeout(std::time::Duration::from_secs(15))
        .no_proxy()
        .build()?;
    let stop = Arc::new(AtomicBool::new(false));
    let queue = Arc::new(std::sync::Mutex::new(
        pending
            .into_iter()
            .collect::<std::collections::VecDeque<ArticleRow>>(),
    ));
    let (tx, mut rx) = mpsc::channel::<(ArticleRow, FetchOutcome)>(workers * 2);

    // 限流对照 URL：已知有效的一条。命中验证页时先抓它确认——实测存在无效短链
    // （sn 坏）稳定返回验证页，没有对照会把整批误判成被限流。
    let control_url: Option<String> = store.any_detail_done_url().unwrap_or(None);

    for i in 0..workers {
        let (client, stop, queue, tx) = (client.clone(), stop.clone(), queue.clone(), tx.clone());
        let control = control_url.clone();
        let throttle = cfg.throttle_ms;
        // 错峰启动（与 Python 基准的差异）：第 i 个 worker 先睡 i*throttle/workers 再开工，
        // 把「启动瞬间 workers 个请求同时发出」摊成均匀节奏——请求间距 ≈ throttle/workers。
        let stagger = throttle * (i as u64) / (workers as u64);
        tokio::spawn(async move {
            if stagger > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(stagger)).await;
            }
            loop {
                let Some(row) = queue.lock().unwrap().pop_front() else {
                    break;
                };
                let outcome = if stop.load(Ordering::Relaxed) {
                    FetchOutcome::Skip
                } else {
                    let mut o = fetch_one(&client, &row).await;
                    if matches!(o, FetchOutcome::Rate) {
                        o = confirm_rate(&client, control.as_deref()).await;
                    }
                    if matches!(o, FetchOutcome::Rate) {
                        stop.store(true, Ordering::Relaxed);
                    }
                    o
                };
                // 先执行后等待：结果立刻上报（进度实时可见），节流放在两次请求**之间**——
                // 跳过的（未发请求）不节流，队列已空也不再空等最后一次。
                let did_fetch = !matches!(outcome, FetchOutcome::Skip);
                if tx.send((row, outcome)).await.is_err() {
                    break;
                }
                if did_fetch && !queue.lock().unwrap().is_empty() {
                    tokio::time::sleep(std::time::Duration::from_millis(throttle)).await;
                }
            }
        });
    }
    drop(tx);

    // 聚合循环：串行入库 + 进度回调。
    let mut completed = 0usize;
    while let Some((row, outcome)) = rx.recv().await {
        completed += 1;
        let label = row.title.clone().unwrap_or_else(|| {
            format!(
                "{}_{}",
                row.mid.as_deref().unwrap_or("?"),
                row.idx.unwrap_or(0)
            )
        });
        let line = match outcome {
            FetchOutcome::Ok(parsed) => {
                if let Some(name) = parsed.mp_name.as_deref() {
                    let _ = store.upsert_account(&row.biz, Some(name), None);
                }
                let md_len = parsed
                    .markdown
                    .as_deref()
                    .map(str::chars)
                    .map(Iterator::count)
                    .unwrap_or(0);
                let fields = ArticleFields {
                    title: parsed.title.clone(),
                    author: parsed.mp_name.clone(),
                    digest: parsed.digest.clone(),
                    cover: parsed.cover.clone(),
                    published_at: parsed.publish_time.map(|t| t as f64),
                    content_text: parsed.markdown.clone(),
                    content_md: parsed.markdown.clone(),
                    detail_done: Some(1),
                    ..Default::default()
                };
                let mid = row.mid.clone().unwrap_or_default();
                let _ = store.upsert_article(&row.biz, &mid, row.idx.unwrap_or(0), &fields);
                let _ = store.clear_detail_failure(row.id);
                summary.done += 1;
                format!(
                    "[{completed}/{total}] ✓ {}（{md_len} 字）",
                    truncate_chars(parsed.title.as_deref().unwrap_or(&label), 36)
                )
            }
            FetchOutcome::TextOnly {
                title,
                author,
                text,
            } => {
                let fields = ArticleFields {
                    title: title.clone(),
                    author,
                    content_text: Some(text.clone()),
                    content_md: Some(text),
                    detail_done: Some(1),
                    ..Default::default()
                };
                let mid = row.mid.clone().unwrap_or_default();
                let _ = store.upsert_article(&row.biz, &mid, row.idx.unwrap_or(0), &fields);
                let _ = store.clear_detail_failure(row.id);
                summary.done += 1;
                format!(
                    "[{completed}/{total}] ✓ {}（回退纯文本）",
                    truncate_chars(title.as_deref().unwrap_or(&label), 36)
                )
            }
            FetchOutcome::Unavailable(reason) => {
                // 永久不可用：detail_done=-1 出候选池，之后手动/批量都不再抓它。
                let _ = store.mark_article_unavailable(row.id, &reason);
                summary.unavailable += 1;
                format!(
                    "[{completed}/{total}] 🚫 不可用（{reason}）：{}",
                    truncate_chars(&label, 28)
                )
            }
            FetchOutcome::Rate => {
                let first = !summary.rate_limited;
                summary.rate_limited = true;
                if first {
                    format!("[{completed}/{total}] ⛔ 被微信限流（验证页），正在停止其余任务…")
                } else {
                    format!("[{completed}/{total}] ⛔ 限流跳过")
                }
            }
            FetchOutcome::Skip => {
                summary.skipped += 1;
                format!(
                    "[{completed}/{total}] ↷ 跳过：{}",
                    truncate_chars(&label, 28)
                )
            }
            FetchOutcome::Fail(why) => {
                summary.failed += 1;
                let attempts = store.bump_detail_attempt(row.id, &why).unwrap_or(0);
                let tail = if attempts >= 3 {
                    "，已达上限，批量补采将跳过"
                } else {
                    ""
                };
                format!(
                    "[{completed}/{total}] ✗ {why}（第 {attempts} 次{tail}）：{}",
                    truncate_chars(&label, 28)
                )
            }
        };
        emit(DetailProgress {
            completed,
            total,
            done: summary.done,
            failed: summary.failed,
            line,
        });
    }

    summary.feedback = format!(
        "详情采集{}篇（候选{}篇，失败{}篇{}{}）{}",
        summary.done,
        summary.candidates,
        summary.failed,
        if summary.unavailable > 0 {
            format!("，不可用{}篇已标记", summary.unavailable)
        } else {
            String::new()
        },
        if summary.skipped > 0 {
            format!("，跳过{}篇", summary.skipped)
        } else {
            String::new()
        },
        if summary.rate_limited {
            "；已被微信限流，已停止本批，请稍后/换网络再试"
        } else {
            ""
        }
    );
    emit(DetailProgress {
        completed,
        total,
        done: summary.done,
        failed: summary.failed,
        line: format!("✅ 补正文完成：{}", summary.feedback),
    });
    Ok(summary)
}

/// 命中验证页后的二次判定：再抓一次对照 URL（已知有效的文章）。
/// 对照也验证页 → 真被限流（Rate）；对照正常 → 是该篇链接自身的问题
/// （无效 sn 的短链会**稳定**返回验证页），按普通失败计数（满 3 次自动出池）。
/// 库里还没有任何已完成详情（无对照）时保守地按限流处理。
async fn confirm_rate(client: &reqwest::Client, control: Option<&str>) -> FetchOutcome {
    let Some(url) = control else {
        return FetchOutcome::Rate;
    };
    let html = match client.get(url).send().await {
        Ok(resp) => match resp.text().await {
            Ok(t) => t,
            Err(_) => return FetchOutcome::Rate,
        },
        Err(_) => return FetchOutcome::Rate,
    };
    match article_md::parse_html(url, &html) {
        Err(article_md::ParseError::VerifyPage) => FetchOutcome::Rate,
        _ => FetchOutcome::Fail("命中验证页（对照正常，链接疑似无效）".to_string()),
    }
}

/// 抓一篇并解析。限流 → Rate；article-md 无正文 → 回退旧解析抽纯文本。
async fn fetch_one(client: &reqwest::Client, row: &ArticleRow) -> FetchOutcome {
    let Some(url) = row.content_url.as_deref().filter(|u| !u.is_empty()) else {
        return FetchOutcome::Fail("无链接".to_string());
    };
    let html = match client.get(url).send().await {
        Ok(resp) => match resp.error_for_status() {
            Ok(resp) => match resp.text().await {
                Ok(t) => t,
                Err(_) => return FetchOutcome::Fail("读响应失败".to_string()),
            },
            Err(_) => return FetchOutcome::Fail("HTTP 错误".to_string()),
        },
        Err(_) => return FetchOutcome::Fail("请求失败".to_string()),
    };
    match article_md::parse_html(url, &html) {
        Err(article_md::ParseError::VerifyPage) => FetchOutcome::Rate,
        Err(article_md::ParseError::Unavailable(reason)) => FetchOutcome::Unavailable(reason),
        Ok(parsed) if parsed.markdown.is_some() => FetchOutcome::Ok(Box::new(parsed)),
        Ok(_) => {
            // 回退：旧解析（selectolax 等价物）抽纯文本。
            let c = crate::wechat::parse_article_html(&html);
            match c.content_text {
                Some(text) if !text.is_empty() => FetchOutcome::TextOnly {
                    title: c.title,
                    author: c.author,
                    text,
                },
                _ => FetchOutcome::Fail("无正文".to_string()),
            }
        }
    }
}

/// 按字符数截断（标题里有中文，不能按字节切）。
fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_pending_returns_immediately() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let s = collect_detail(store, DetailConfig::default(), |_| {})
            .await
            .unwrap();
        assert_eq!(s.candidates, 0);
        assert!(!s.rate_limited);
    }

    #[test]
    fn truncate_multibyte_safe() {
        assert_eq!(truncate_chars("中文标题很长", 3), "中文标");
    }
}
