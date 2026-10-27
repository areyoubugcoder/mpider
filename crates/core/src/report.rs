//! 采集结果上报（通用 HTTP JSON）—— 用户在系统设置里填一个自己的 HTTP 服务地址（可选 Bearer token），
//! **每个公众号列表采完就当场** POST 一条本模块定义的载荷（手动任务与巡检批次都报）。
//!
//! - 载荷形状（[`ReportPayload`]）：
//!   ```json
//!   {"source":"mpider","job_id":123,"job_kind":"sweep","account":{"biz":"…","nickname":"…"},
//!    "collected_at":1789000000,
//!    "articles":[{"url":"…","title":"…","published_at":1788990000,"is_new":true}]}
//!   ```
//!   `articles` 只含发布时间 ≥ 该号「最后发布时间」的文章（按页顺序、不去重）；`is_new` = 本次采集**新入库**。
//!   按时间比对后没有新文章也报 `articles: []`；列表没正常获取到的号不报。
//! - 2xx 即成功，不重试（服务端自行幂等）；非 2xx / 网络错误记进任务的 `report_error`、任务分类为「服务端错误」。
//! - 未启用（`enabled=false` 或地址为空）时不发、返回 [`ReportAck::skipped`]，任务分类不算失败。
//! - 请求走直连（`no_proxy`）：抓凭证的 MITM 代理开着时也不劫持本请求。日志只记地址，不记 token。

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::model::{Epoch, ReportAck};

/// 上报配置（来自运行配置 `report_*`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReportConfig {
    /// 总开关（默认关）。
    pub enabled: bool,
    /// 接收上报的 HTTP(S) 地址（POST JSON）。
    pub url: String,
    /// 可选 token：非空则加 `Authorization: Bearer <token>`。**不得写日志。**
    pub token: String,
    /// 请求超时（秒，默认 15）。
    pub timeout_secs: u64,
}

impl Default for ReportConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            token: String::new(),
            timeout_secs: 15,
        }
    }
}

impl ReportConfig {
    /// 是否会真的出网：开关开且地址非空。
    pub fn active(&self) -> bool {
        self.enabled && !self.url.trim().is_empty()
    }
}

/// 载荷里的一篇文章。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReportArticle {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 发布时间（epoch 秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<i64>,
    /// 本次采集是否新入本地库。
    #[serde(default)]
    pub is_new: bool,
}

/// 载荷里的公众号。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReportAccount {
    pub biz: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
}

/// 一次上报的载荷：**一个公众号一条**。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReportPayload {
    /// 固定 `"mpider"`，接收方据此区分来源。
    pub source: String,
    /// 本地 jobs 表 id。
    pub job_id: i64,
    /// `sweep`（定时巡检批次 / 单号重新巡检）/ `history`（历史抓取任务结束或暂停时报一次）。
    pub job_kind: String,
    pub account: ReportAccount,
    /// 该号列表采完的时刻（epoch 秒）。
    pub collected_at: Epoch,
    pub articles: Vec<ReportArticle>,
}

impl ReportPayload {
    pub const SOURCE: &'static str = "mpider";

    /// 组一条载荷（`collected_at` 取当前时刻）。
    pub fn new(
        job_id: i64,
        job_kind: &str,
        biz: &str,
        nickname: Option<String>,
        articles: Vec<ReportArticle>,
    ) -> Self {
        Self {
            source: Self::SOURCE.to_string(),
            job_id,
            job_kind: job_kind.to_string(),
            account: ReportAccount {
                biz: biz.to_string(),
                nickname,
            },
            collected_at: crate::model::now(),
            articles,
        }
    }

    /// 文章 URL 列表（本地结果 `accounts[].urls` 用）。
    pub fn urls(&self) -> Vec<String> {
        self.articles.iter().map(|a| a.url.clone()).collect()
    }
}

/// 把逐号上报结果合成一个任务级 [`ReportAck`]：跳过的（未启用）不计；其余全部 2xx 才算成功；
/// 否则列出失败的号（`label` 是号的显示名）。一个都没真正发出返回 `None`。纯函数，便于单测。
pub fn merge_acks(acks: &[(String, ReportAck)]) -> Option<ReportAck> {
    let sent: Vec<&(String, ReportAck)> = acks.iter().filter(|(_, a)| !a.skipped).collect();
    if sent.is_empty() {
        return None;
    }
    let failed: Vec<&&(String, ReportAck)> = sent.iter().filter(|(_, a)| !a.ok).collect();
    if failed.is_empty() {
        return Some(ReportAck {
            ok: true,
            http_status: sent.last().and_then(|(_, a)| a.http_status),
            error: None,
            skipped: false,
        });
    }
    let detail = failed
        .iter()
        .take(5)
        .map(|(label, a)| {
            format!(
                "{label}：{}",
                a.error.as_deref().unwrap_or("上报服务未返回成功状态")
            )
        })
        .collect::<Vec<_>>()
        .join("；");
    Some(ReportAck {
        ok: false,
        http_status: failed[0].1.http_status,
        error: Some(format!(
            "{}/{} 个号上报失败（{detail}{}）",
            failed.len(),
            sent.len(),
            if failed.len() > 5 { "…" } else { "" }
        )),
        skipped: false,
    })
}

/// HTTP 上报客户端（可 clone 共享）。
#[derive(Clone)]
pub struct HttpReporter {
    cfg: ReportConfig,
    client: reqwest::Client,
}

impl HttpReporter {
    pub fn new(cfg: ReportConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(
                cfg.timeout_secs.clamp(1, 300),
            ))
            .build()?;
        Ok(Self { cfg, client })
    }

    /// 配置（不含 token 原文的用途：日志里只用 `url`）。
    pub fn config(&self) -> &ReportConfig {
        &self.cfg
    }

    /// POST 一条载荷。未启用 → `skipped`；2xx → ok；非 2xx → `HTTP <status>`；网络错误 → 错误文本。
    pub async fn report(&self, payload: &ReportPayload) -> ReportAck {
        if !self.cfg.active() {
            return ReportAck::skipped();
        }
        let body = match serde_json::to_vec(payload) {
            Ok(b) => b,
            Err(e) => return ReportAck::failed(format!("序列化上报载荷失败：{e}")),
        };
        let mut req = self
            .client
            .post(self.cfg.url.trim())
            .header("Content-Type", "application/json")
            .body(body);
        let token = self.cfg.token.trim();
        if !token.is_empty() {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        match req.send().await {
            Ok(resp) => ReportAck::from_status(resp.status().as_u16()),
            Err(e) => ReportAck::failed(describe_err(&e)),
        }
    }
}

/// reqwest 错误文案：去掉 URL（地址本身不敏感，但保持与其它模块一致、短一点）。
fn describe_err(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "请求超时".to_string()
    } else if e.is_connect() {
        "连接失败".to_string()
    } else {
        let mut s = e.to_string();
        if let Some(pos) = s.find(" for url (") {
            s.truncate(pos);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn payload() -> ReportPayload {
        ReportPayload::new(
            7,
            "sweep",
            "AAA==",
            Some("测试号".into()),
            vec![ReportArticle {
                url: "https://mp.weixin.qq.com/s?__biz=AAA==&mid=1&idx=1&sn=x".into(),
                title: Some("标题".into()),
                published_at: Some(1_700_000_000),
                is_new: true,
            }],
        )
    }

    #[test]
    fn payload_shape() {
        let v = serde_json::to_value(payload()).unwrap();
        assert_eq!(v["source"], "mpider");
        assert_eq!(v["job_id"], 7);
        assert_eq!(v["job_kind"], "sweep");
        assert_eq!(v["account"]["biz"], "AAA==");
        assert_eq!(v["account"]["nickname"], "测试号");
        assert!(v["collected_at"].is_number());
        assert_eq!(v["articles"][0]["is_new"], true);
        assert_eq!(v["articles"][0]["published_at"], 1_700_000_000);
        // 没有 task_id / mp_id 这类字段
        assert!(v.get("task_id").is_none() && v.get("mp_id").is_none());
    }

    #[test]
    fn merge_acks_rules() {
        assert!(merge_acks(&[]).is_none());
        assert!(merge_acks(&[("a".into(), ReportAck::skipped())]).is_none());
        let ok = merge_acks(&[
            ("a".into(), ReportAck::from_status(200)),
            ("b".into(), ReportAck::skipped()),
            ("c".into(), ReportAck::from_status(204)),
        ])
        .unwrap();
        assert!(ok.ok && ok.http_status == Some(204));
        let bad = merge_acks(&[
            ("a".into(), ReportAck::from_status(200)),
            ("b".into(), ReportAck::from_status(502)),
            ("c".into(), ReportAck::failed("连接失败")),
        ])
        .unwrap();
        assert!(!bad.ok);
        assert_eq!(bad.http_status, Some(502));
        let err = bad.error.unwrap();
        assert!(err.contains("2/3") && err.contains("b：HTTP 502") && err.contains("c：连接失败"));
    }

    #[tokio::test]
    async fn disabled_is_skipped_without_network() {
        let r = HttpReporter::new(ReportConfig {
            enabled: false,
            url: "http://127.0.0.1:9/x".into(),
            ..Default::default()
        })
        .unwrap();
        let ack = r.report(&payload()).await;
        assert!(ack.skipped && !ack.ok && ack.error.is_none());
        let r = HttpReporter::new(ReportConfig {
            enabled: true,
            url: "  ".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(r.report(&payload()).await.skipped);
    }

    async fn spawn_receiver(
        status: &'static str,
    ) -> (std::net::SocketAddr, tokio::sync::oneshot::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 2048];
            loop {
                let n = s.read(&mut tmp).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                    let cl = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if buf.len() >= pos + 4 + cl {
                        break;
                    }
                }
            }
            let _ = s
                .write_all(
                    format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .await;
            let _ = tx.send(String::from_utf8_lossy(&buf).to_string());
        });
        (addr, rx)
    }

    #[tokio::test]
    async fn posts_json_with_bearer_and_maps_status() {
        let (addr, rx) = spawn_receiver("204 No Content").await;
        let r = HttpReporter::new(ReportConfig {
            enabled: true,
            url: format!("http://{addr}/hook"),
            token: "tok".into(),
            timeout_secs: 5,
        })
        .unwrap();
        let ack = r.report(&payload()).await;
        assert!(ack.ok && ack.http_status == Some(204) && !ack.skipped);
        let raw = rx.await.unwrap();
        assert!(raw.starts_with("POST /hook HTTP/1.1"));
        assert!(
            raw.contains("authorization: Bearer tok") || raw.contains("Authorization: Bearer tok")
        );
        let body = raw.split("\r\n\r\n").nth(1).unwrap();
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["source"], "mpider");
        assert_eq!(v["articles"].as_array().unwrap().len(), 1);

        let (addr, _rx) = spawn_receiver("502 Bad Gateway").await;
        let r = HttpReporter::new(ReportConfig {
            enabled: true,
            url: format!("http://{addr}/hook"),
            ..Default::default()
        })
        .unwrap();
        let ack = r.report(&payload()).await;
        assert!(!ack.ok && ack.http_status == Some(502));
        assert_eq!(ack.error.as_deref(), Some("HTTP 502"));
    }

    #[tokio::test]
    async fn network_error_is_failed() {
        let r = HttpReporter::new(ReportConfig {
            enabled: true,
            url: "http://127.0.0.1:9/nope".into(),
            timeout_secs: 2,
            ..Default::default()
        })
        .unwrap();
        let ack = r.report(&payload()).await;
        assert!(!ack.ok && !ack.skipped && ack.http_status.is_none());
        assert!(ack.error.is_some());
    }
}
