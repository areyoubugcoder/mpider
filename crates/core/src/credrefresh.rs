//! 凭证续期（**独立封装**，标准输入输出）—— 把「一批公众号的凭证过期了，用接力重新拿 key」
//! 抽成可复用的一步：输入 [`RefreshTarget`]（号 + 该号一条已抓到的文章链接），输出
//! [`RefreshReport`]（哪些号拿到了新 key、哪些失败、用了几次尝试）。
//!
//! 原理：抓凭证代理仍在跑（编排器在收尾前不停代理），把各目标号的**取样链接**写进接力队列，
//! RPA 点一次固定种子入口 → 入口页被注入跳到第一条取样链接 → 文章页 JS 发 `getappmsgext`
//! （带 key）被 MITM 抓到 → 接力再跳下一条……**一次开浏览器把整批过期号一起续掉**（而不是
//! 过期一个开一次），也正是接力天然适合批量的地方。
//!
//! 重试：微信内置浏览器常见"打开后页面未加载/未绘制"，所以每轮尝试**独立计时**、点空/停滞
//! 就关窗重点，最多 `max_attempts`（默认 3）次；每轮只重做**还没拿到新 key** 的号。
//! 命中人机验证页整批中止（与 orchestrator 看门狗一致）。
//!
//! 与 orchestrator 一样，依赖全部可注入（store / relay / rpa / sleep），脱网可测；步骤日志走
//! [`crate::applog`]（环节 = 凭证续期）。

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::applog::{self, Stage};
use crate::model::now;
use crate::orchestrator::SleepFn;
use crate::relay::RelayQueue;
use crate::rpa::{self, WeChatController};
use crate::store::Store;

/// 续期目标：某个号 + 该号一条**已抓到的**文章长链（打开它就会触发带 key 的凭证请求）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefreshTarget {
    pub biz: String,
    pub sample_url: String,
}

/// 续期配置。
#[derive(Clone, Debug)]
pub struct RefreshConfig {
    /// 最多尝试几轮（每轮 = 重置队列 + 点种子 + 等待）。产品要求 ≥ 3。
    pub max_attempts: u32,
    /// 每轮最长等待（秒）：等各号新 key 到位；超时即进入下一轮。
    pub wait_secs: u64,
    /// 点种子后多少秒没有任何 `/s` 请求判"点空"，提前结束本轮（不用等满 `wait_secs`）。
    pub launch_timeout_secs: u64,
    /// 判活 TTL（秒）。
    pub cred_ttl_secs: i64,
    /// 固定种子入口链接（RPA 点它；`None` 时点第一条取样链接——人工/NoOp 模式）。
    pub seed_url: Option<String>,
}

impl Default for RefreshConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            wait_secs: 60,
            launch_timeout_secs: 10,
            cred_ttl_secs: 30 * 60,
            seed_url: None,
        }
    }
}

/// 续期结果（标准输出）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefreshReport {
    /// 拿到新 key 的号。
    pub refreshed: Vec<String>,
    /// 用尽尝试仍没拿到的号。
    pub failed: Vec<String>,
    /// 实际用了几轮。
    pub attempts: u32,
    /// 整批中止原因（命中验证页）。
    pub aborted: Option<String>,
}

impl RefreshReport {
    pub fn all_ok(&self) -> bool {
        self.failed.is_empty() && self.aborted.is_none()
    }
}

/// 凭证续期器。
pub struct CredentialRefresher {
    pub store: Arc<Store>,
    pub relay: RelayQueue,
    pub rpa: Arc<dyn WeChatController>,
    pub sleep: SleepFn,
    pub cfg: RefreshConfig,
}

impl CredentialRefresher {
    /// 便捷构造。
    pub fn new(
        store: Arc<Store>,
        relay: RelayQueue,
        rpa: Arc<dyn WeChatController>,
        sleep: SleepFn,
        cfg: RefreshConfig,
    ) -> Self {
        Self {
            store,
            relay,
            rpa,
            sleep,
            cfg,
        }
    }

    fn emit(&self, msg: String) {
        applog::info(Stage::Refresh, msg);
    }

    /// 该号是否在 `since` 之后拿到了新的、仍新鲜的 key。
    fn refreshed_since(&self, biz: &str, since: f64) -> bool {
        match self.store.get_credential(biz) {
            Ok(Some(c)) => {
                c.captured_at >= since
                    && self
                        .store
                        .credential_is_fresh(biz, self.cfg.cred_ttl_secs)
                        .unwrap_or(false)
            }
            _ => false,
        }
    }

    /// 批量续期：一次接力把 `targets` 里的号一起续掉；失败的号在下一轮重做，最多
    /// `max_attempts` 轮。空目标直接返回。**不**负责起/停代理与清队列之外的收尾
    /// （结束时会 `relay.clear()` 并关一次浏览器，便于调用方继续采集）。
    pub async fn refresh(&self, targets: &[RefreshTarget]) -> RefreshReport {
        let mut report = RefreshReport::default();
        // 按 biz 去重，保序。
        let mut pending: Vec<RefreshTarget> = Vec::new();
        for t in targets {
            if !t.biz.is_empty()
                && !t.sample_url.is_empty()
                && !pending.iter().any(|p| p.biz == t.biz)
            {
                pending.push(t.clone());
            }
        }
        if pending.is_empty() {
            return report;
        }

        for attempt in 1..=self.cfg.max_attempts.max(1) {
            report.attempts = attempt;
            let started = now();
            let urls: Vec<String> = pending.iter().map(|t| t.sample_url.clone()).collect();
            self.relay.set(&urls);
            self.emit(format!(
                "🔑 凭证续期 第 {attempt}/{} 轮：{} 个号待续（{}）",
                self.cfg.max_attempts,
                pending.len(),
                pending
                    .iter()
                    .map(|t| self.store.account_label(&t.biz))
                    .collect::<Vec<_>>()
                    .join("、")
            ));
            let seed = self.cfg.seed_url.clone().unwrap_or_else(|| urls[0].clone());
            let launched_at = Instant::now();
            // 人工模式且待命页在线：队列已接上，待命页长轮询会自己取首条跳走，不点、不打扰人。
            let resident = self.rpa.manual() && crate::seedserver::resident_alive();
            let rpa_ok = if resident {
                applog::info(
                    Stage::Rpa,
                    "续期：微信内置浏览器待命页在线，交给它自动接力".to_string(),
                );
                false
            } else {
                applog::info(
                    Stage::Rpa,
                    "续期：点击种子链接，打开微信内置浏览器…".to_string(),
                );
                let (ok, msg) = self.rpa.open_seed(&seed);
                if ok {
                    applog::info(Stage::Rpa, format!("微信内置浏览器已打开：{msg}"));
                } else {
                    applog::warn(Stage::Rpa, format!("打开微信内置浏览器失败：{msg}"));
                }
                ok
            };
            // 人工模式（mac / 关闭 RPA）且待命页不在线：推全局提醒让人去微信点开一篇文章（等待上限已由编排层抬到人工下限）。
            let manual_alert = if !rpa_ok && self.rpa.manual() && !resident {
                let labels: Vec<String> = pending
                    .iter()
                    .map(|t| self.store.account_label(&t.biz))
                    .collect();
                rpa::manual_open_alert(&labels, self.cfg.wait_secs)
            } else {
                None
            };

            let deadline = Instant::now() + Duration::from_secs(self.cfg.wait_secs);
            let mut aborted = false;
            let mut last_tick = u64::MAX;
            loop {
                // 逐号看是否拿到新 key。
                pending.retain(|t| {
                    if self.refreshed_since(&t.biz, started) {
                        report.refreshed.push(t.biz.clone());
                        false
                    } else {
                        true
                    }
                });
                if pending.is_empty() {
                    break;
                }
                if let Some(url) = self.relay.verify_hit() {
                    report.aborted = Some(format!("命中人机验证页（{url}）"));
                    aborted = true;
                    break;
                }
                if let Some(reason) = self.relay.abort_reason() {
                    report.aborted = Some(reason);
                    aborted = true;
                    break;
                }
                // 瞬态倒计时（只推 GUI，不入库）。
                let remain = deadline.saturating_duration_since(Instant::now()).as_secs();
                if remain.is_multiple_of(5) && remain != last_tick {
                    applog::progress(
                        Stage::Refresh,
                        format!("等待新凭证… 剩余 {remain}s（还差 {} 个号）", pending.len()),
                    );
                    last_tick = remain;
                }
                let now_i = Instant::now();
                if now_i >= deadline {
                    break;
                }
                // 点空：点种子后一直没有 /s 请求 → 本轮提前结束，下一轮重点。
                if rpa_ok
                    && self.cfg.launch_timeout_secs > 0
                    && self
                        .relay
                        .last_request_at()
                        .map(|t| t < launched_at)
                        .unwrap_or(true)
                    && now_i - launched_at >= Duration::from_secs(self.cfg.launch_timeout_secs)
                {
                    applog::warn(
                        Stage::Refresh,
                        "点种子后无任何文章请求（疑似点空/未绘制），本轮放弃".to_string(),
                    );
                    break;
                }
                (self.sleep)(Duration::from_millis(200)).await;
            }
            if let Some(id) = manual_alert {
                crate::runstate::alert_ack(id);
            }

            if aborted || pending.is_empty() {
                break;
            }
            applog::warn(
                Stage::Refresh,
                format!("本轮仍有 {} 个号未拿到新凭证，关窗重来", pending.len()),
            );
            let _ = self.rpa.close_browser();
        }

        report.failed = pending.into_iter().map(|t| t.biz).collect();
        // 收尾：清队列 + 关窗，把浏览器留在干净状态给后续采集。
        self.relay.clear();
        let _ = self.rpa.close_browser();
        let summary = format!(
            "🔑 续期结束：成功 {} 个，失败 {} 个，用 {} 轮{}",
            report.refreshed.len(),
            report.failed.len(),
            report.attempts,
            report
                .aborted
                .as_ref()
                .map(|r| format!("；中止：{r}"))
                .unwrap_or_default()
        );
        if report.all_ok() {
            self.emit(summary);
        } else {
            applog::warn(Stage::Refresh, summary);
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture;
    use crate::orchestrator::default_sleep;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 可编排的模拟 RPA：第 n 次 open_seed 执行第 n 个闭包。
    struct ScriptedRpa {
        calls: Arc<AtomicUsize>,
        steps: Vec<Box<dyn Fn() + Send + Sync>>,
    }
    impl WeChatController for ScriptedRpa {
        fn open_seed(&self, _url: &str) -> (bool, String) {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(s) = self.steps.get(n) {
                s();
            }
            (true, format!("模拟点击 #{}", n + 1))
        }
        fn close_browser(&self) -> (bool, String) {
            (true, "模拟关闭".into())
        }
    }

    fn cred_url(biz: &str, key: &str) -> String {
        format!(
            "https://mp.weixin.qq.com/mp/getappmsgext?__biz={biz}&uin=U&key={key}&pass_ticket=P"
        )
    }

    fn targets() -> Vec<RefreshTarget> {
        vec![
            RefreshTarget {
                biz: "AAA==".into(),
                sample_url: "https://mp.weixin.qq.com/s?__biz=AAA==&mid=1&idx=1&sn=a".into(),
            },
            RefreshTarget {
                biz: "BBB==".into(),
                sample_url: "https://mp.weixin.qq.com/s?__biz=BBB==&mid=2&idx=1&sn=b".into(),
            },
            // 重复的号只算一次
            RefreshTarget {
                biz: "AAA==".into(),
                sample_url: "https://mp.weixin.qq.com/s?__biz=AAA==&mid=9&idx=1&sn=z".into(),
            },
        ]
    }

    #[tokio::test]
    async fn test_refresh_batch_first_attempt_ok() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let relay = RelayQueue::new();
        // 预置两份「过期」凭证（实测失效）。
        for b in ["AAA==", "BBB=="] {
            capture::capture_request(&store, &cred_url(b, "OLD"), None, None);
            store.mark_credential_invalid(b).unwrap();
            assert!(!store.credential_is_fresh(b, 1800).unwrap());
        }
        let (s1, r1) = (store.clone(), relay.clone());
        let rpa = ScriptedRpa {
            calls: Arc::new(AtomicUsize::new(0)),
            steps: vec![Box::new(move || {
                // 一次接力把两条取样链接都打开、各自抓到新 key。
                r1.touch();
                for row in r1.rows() {
                    let biz = crate::proxy_addon::parse_s_url(&row.url).unwrap().biz;
                    capture::capture_request(&s1, &cred_url(&biz, "NEW"), None, None);
                    r1.advance(Some(&row.url), None);
                }
            })],
        };
        let calls = rpa.calls.clone();
        let rf = CredentialRefresher::new(
            store.clone(),
            relay.clone(),
            Arc::new(rpa),
            Arc::new(|_d| Box::pin(async {})),
            RefreshConfig {
                wait_secs: 5,
                ..Default::default()
            },
        );
        let rep = rf.refresh(&targets()).await;
        assert!(rep.all_ok(), "{rep:?}");
        assert_eq!(rep.attempts, 1);
        assert_eq!(rep.refreshed.len(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "整批一次开浏览器");
        assert!(store.credential_is_fresh("AAA==", 1800).unwrap());
        assert!(store.credential_is_fresh("BBB==", 1800).unwrap());
        assert_eq!(relay.pending_count(), 0, "收尾清队列");
    }

    #[tokio::test]
    async fn test_refresh_retries_only_missing_and_gives_up_after_max() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let relay = RelayQueue::new();
        let (s1, r1) = (store.clone(), relay.clone());
        let r2 = relay.clone();
        let rpa = ScriptedRpa {
            calls: Arc::new(AtomicUsize::new(0)),
            steps: vec![
                // 第 1 轮：点空（无任何请求）→ launch_timeout=0 立刻放弃本轮
                Box::new(|| {}),
                // 第 2 轮：只有 AAA 拿到 key
                Box::new(move || {
                    r1.touch();
                    capture::capture_request(&s1, &cred_url("AAA==", "NEW"), None, None);
                    // 第 2 轮队列里应只剩两条（AAA/BBB），且 BBB 没拿到
                    assert_eq!(r1.rows().len(), 2);
                }),
                // 第 3 轮：只剩 BBB，仍没拿到 → 用尽
                Box::new(move || {
                    r2.touch();
                    assert_eq!(r2.rows().len(), 1, "只重做还没拿到 key 的号");
                    assert!(r2.rows()[0].url.contains("BBB=="));
                }),
            ],
        };
        let calls = rpa.calls.clone();
        let rf = CredentialRefresher::new(
            store.clone(),
            relay.clone(),
            Arc::new(rpa),
            Arc::new(|_d| Box::pin(async {})),
            RefreshConfig {
                max_attempts: 3,
                wait_secs: 0,
                launch_timeout_secs: 0,
                ..Default::default()
            },
        );
        let rep = rf.refresh(&targets()).await;
        assert_eq!(rep.attempts, 3);
        assert_eq!(calls.load(Ordering::SeqCst), 3, "至少 3 次重试机制");
        assert_eq!(rep.refreshed, vec!["AAA==".to_string()]);
        assert_eq!(rep.failed, vec!["BBB==".to_string()]);
        assert!(rep.aborted.is_none());
    }

    #[tokio::test]
    async fn test_refresh_aborts_on_verify_page() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let relay = RelayQueue::new();
        let r1 = relay.clone();
        let rpa = ScriptedRpa {
            calls: Arc::new(AtomicUsize::new(0)),
            steps: vec![Box::new(move || {
                r1.touch();
                r1.mark_verify("https://mp.weixin.qq.com/s?__biz=AAA==&mid=1&idx=1&sn=a");
            })],
        };
        let calls = rpa.calls.clone();
        let rf = CredentialRefresher::new(
            store,
            relay,
            Arc::new(rpa),
            default_sleep(),
            RefreshConfig {
                wait_secs: 5,
                ..Default::default()
            },
        );
        let rep = rf.refresh(&targets()).await;
        assert!(rep.aborted.is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "验证页不重试");
        assert_eq!(rep.failed.len(), 2);
    }

    #[tokio::test]
    async fn test_refresh_empty_targets_noop() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let rpa = ScriptedRpa {
            calls: Arc::new(AtomicUsize::new(0)),
            steps: vec![],
        };
        let calls = rpa.calls.clone();
        let rf = CredentialRefresher::new(
            store,
            RelayQueue::new(),
            Arc::new(rpa),
            Arc::new(|_d| Box::pin(async {})),
            RefreshConfig::default(),
        );
        let rep = rf.refresh(&[]).await;
        assert_eq!(rep.attempts, 0);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(rep.all_ok());
    }
}
