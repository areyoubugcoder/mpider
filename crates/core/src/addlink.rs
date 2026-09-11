//! 「批量添加公众号」：只收文章**短链**，匿名直连文章页（不开微信、不走代理、不带凭证），从 HTML
//! 解析出公众号 biz / 名称 / 头像，建号并把这条链接记成该号的**种子链接**（库里还没有文章时，
//! 巡检 / 历史抓取 / 重新巡检用它接力换凭证，见 [`Store::latest_article_urls`]）。
//!
//! 只接受**图文文章**（页面 `var item_show_type = "0"`）：视频分享 / 图片轮播 / 文字消息页
//! 没有稳定的正文版式，直接拒绝并说明类型。长链没有 `chksm` 参数时匿名访问必回验证页、拿不到
//! 公众号名称，所以链接一律要求短链（文章右上角「复制链接」得到的形式）。
//!
//! 解析（[`resolve_html`] / [`item_show_type`]）是纯函数，可脱网单测；网络与入库在 [`add_links`]。

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::applog::{self, Stage};
use crate::proxy_addon::{
    parse_account_avatar_from_html, parse_account_name_from_html, parse_article_meta_from_html,
    require_short_article_link,
};
use crate::store::Store;

/// 两条链接之间的间隔（毫秒）：匿名访问公开页不占 `getmsg` 配额，但也不连发。
pub const LINK_GAP_MS: u64 = 400;

/// 请求超时（秒）。
const TIMEOUT_SECS: u64 = 15;

/// 桌面 Chrome UA：与正文补采一致（换微信 UA 会触发风控验证页）。
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36";

/// 一行链接的处理结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AddLinkStatus {
    /// 新建了公众号。
    Added,
    /// 公众号已存在：更新了名称 / 头像 / 种子链接。
    Updated,
    /// 被拒绝（原因见 `reason`）。
    Rejected,
    /// 没有处理（前面的链接命中验证页，本批中止）。
    Skipped,
}

/// 一行链接的处理结果（GUI 逐行展示）。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AddLinkResult {
    /// 用户输入的原始行（去首尾空白）。
    pub line: String,
    pub status: AddLinkStatus,
    pub biz: Option<String>,
    pub nickname: Option<String>,
    /// 拒绝 / 跳过的原因。
    pub reason: Option<String>,
}

/// 一批链接的汇总。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AddLinksSummary {
    pub added: usize,
    pub updated: usize,
    pub rejected: usize,
    pub skipped: usize,
    pub items: Vec<AddLinkResult>,
}

/// 从文章页解析出的公众号信息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAccount {
    pub biz: String,
    pub nickname: String,
    pub avatar: Option<String>,
    /// 文章标题（日志用）。
    pub title: Option<String>,
}

/// 解析失败原因。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveError {
    /// 微信返回了人机验证页：本机可能被限流，本批应中止。
    VerifyPage,
    /// 其它原因（文章不可用 / 不是图文 / 页面缺公众号信息），只影响这一条。
    Other(String),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::VerifyPage => write!(f, "微信返回验证页：本机可能被限流，稍后再试"),
            ResolveError::Other(s) => write!(f, "{s}"),
        }
    }
}

/// 页面里的文章类型：`var item_show_type = "N"` → `var real_item_show_type = "N"` → 页内数据对象里的
/// `item_show_type: 'N' * 1`（带引号的才是数据；页面脚本模板里还有不带引号的 `item_show_type: 5`，
/// 那是代码常量，不能认）。
/// 0 = 图文文章（含标准富文本与转载式）、5 = 视频分享、8 = 图片轮播、10 = 文字消息。
pub fn item_show_type(html: &str) -> Option<i64> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"var\s+item_show_type\s*=\s*["'](\d+)["']"#).unwrap());
    static RE_REAL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"var\s+real_item_show_type\s*=\s*["'](\d+)["']"#).unwrap());
    static RE_OBJ: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"\b(?:real_)?item_show_type\s*:\s*["'](\d+)["']"#).unwrap());
    RE.captures(html)
        .or_else(|| RE_REAL.captures(html))
        .or_else(|| RE_OBJ.captures(html))
        .and_then(|c| c[1].parse().ok())
}

/// 文章类型码 → 中文名。
pub fn show_type_name(t: i64) -> String {
    match t {
        0 => "图文文章".to_string(),
        5 => "视频分享".to_string(),
        8 => "图片轮播".to_string(),
        10 => "文字消息".to_string(),
        n => format!("类型码 {n}"),
    }
}

/// 从文章页 HTML 解析公众号信息（纯函数）。
///
/// 顺序：验证页 → 文章类型（非 0 直接拒绝）→ `article-md` 解析（识别删除 / 违规等不可用页，
/// 并给出 biz / 名称）→ 兜底用 `proxy_addon` 的正则再找一遍 biz / 名称 / 头像。
pub fn resolve_html(url: &str, html: &str) -> Result<ResolvedAccount, ResolveError> {
    if article_md::is_verify_page(html) {
        return Err(ResolveError::VerifyPage);
    }
    let show_type = item_show_type(html);
    if let Some(t) = show_type.filter(|t| *t != 0) {
        return Err(ResolveError::Other(format!(
            "不是图文文章（{}），请换一篇图文文章",
            show_type_name(t)
        )));
    }
    let parsed = match article_md::parse_html(url, html) {
        Ok(r) => r,
        Err(article_md::ParseError::VerifyPage) => return Err(ResolveError::VerifyPage),
        Err(article_md::ParseError::Unavailable(reason)) => {
            return Err(ResolveError::Other(format!("文章不可用：{reason}")))
        }
    };
    if show_type.is_none() {
        return Err(ResolveError::Other(
            "无法识别文章类型（页面里没有 item_show_type），请换一篇图文文章".to_string(),
        ));
    }
    let biz = parsed
        .biz
        .filter(|b| !b.is_empty())
        .or_else(|| parse_article_meta_from_html(Some(html)).remove("biz"))
        .filter(|b| !b.is_empty());
    let Some(biz) = biz else {
        return Err(ResolveError::Other(
            "页面里没有公众号 biz，无法建号".to_string(),
        ));
    };
    let nickname = parsed
        .mp_name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .or_else(|| parse_account_name_from_html(Some(html)));
    let Some(nickname) = nickname else {
        return Err(ResolveError::Other(
            "页面里没有公众号名称，无法建号".to_string(),
        ));
    };
    Ok(ResolvedAccount {
        biz,
        nickname,
        avatar: parse_account_avatar_from_html(Some(html)),
        title: parsed.title,
    })
}

/// 直连 client：桌面 UA、不带 Cookie、不走系统代理（抓凭证的 MITM 开着时也不能被劫持进自家代理）。
fn client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(UA)
        .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
        .no_proxy()
        .build()?)
}

/// 批量添加：逐行校验 → 保序去重 → 逐条匿名直连解析 → 建号 + 记种子链接。
///
/// `progress(done, total, line)` 在每条开始请求前回调（GUI 显示「正在解析 3/20」）。命中验证页时
/// 该条记拒绝、其余未处理的行记跳过（本机可能被限流，继续打只会加重）。每条之间隔 [`LINK_GAP_MS`]。
pub async fn add_links(
    store: &Store,
    lines: &[String],
    progress: &(dyn Fn(usize, usize, &str) + Send + Sync),
) -> AddLinksSummary {
    let mut summary = AddLinksSummary::default();
    let mut accepted: Vec<(String, String)> = Vec::new();
    for raw in lines {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        match require_short_article_link(line) {
            Ok(u) => {
                if accepted.iter().any(|(_, a)| a == &u) {
                    summary.push(AddLinkResult::rejected(line, "重复"));
                } else {
                    accepted.push((line.to_string(), u));
                }
            }
            Err(reason) => summary.push(AddLinkResult::rejected(line, reason)),
        }
    }
    let total = accepted.len();
    let client = match client() {
        Ok(c) => c,
        Err(e) => {
            for (line, _) in &accepted {
                summary.push(AddLinkResult::rejected(
                    line,
                    &format!("无法创建请求客户端：{e}"),
                ));
            }
            return summary;
        }
    };
    let mut aborted = false;
    for (i, (line, url)) in accepted.iter().enumerate() {
        if aborted {
            summary.push(AddLinkResult {
                line: line.clone(),
                status: AddLinkStatus::Skipped,
                biz: None,
                nickname: None,
                reason: Some("前一条命中验证页，本批中止；稍后再添加".to_string()),
            });
            continue;
        }
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(LINK_GAP_MS)).await;
        }
        progress(i, total, line);
        let html = match fetch(&client, url).await {
            Ok(h) => h,
            Err(e) => {
                summary.push(AddLinkResult::rejected(line, &format!("请求失败：{e}")));
                continue;
            }
        };
        match resolve_html(url, &html) {
            Ok(r) => {
                let existed = store.get_account(&r.biz).ok().flatten().is_some();
                let saved = store
                    .upsert_account(&r.biz, Some(&r.nickname), r.avatar.as_deref())
                    .and_then(|_| store.set_seed_url(&r.biz, url));
                match saved {
                    Ok(()) => {
                        let status = if existed {
                            AddLinkStatus::Updated
                        } else {
                            AddLinkStatus::Added
                        };
                        applog::info(
                            Stage::Task,
                            format!(
                                "📥 {}公众号「{}」（文章：{}）",
                                if existed { "更新" } else { "已添加" },
                                r.nickname,
                                r.title.as_deref().unwrap_or("无标题")
                            ),
                        );
                        summary.push(AddLinkResult {
                            line: line.clone(),
                            status,
                            biz: Some(r.biz),
                            nickname: Some(r.nickname),
                            reason: None,
                        });
                    }
                    Err(e) => {
                        summary.push(AddLinkResult::rejected(line, &format!("入库失败：{e}")));
                    }
                }
            }
            Err(ResolveError::VerifyPage) => {
                applog::warn(
                    Stage::Task,
                    "批量添加：微信返回验证页，本机可能被限流，本批剩余链接不再处理".to_string(),
                );
                summary.push(AddLinkResult::rejected(
                    line,
                    &ResolveError::VerifyPage.to_string(),
                ));
                aborted = true;
            }
            Err(ResolveError::Other(reason)) => {
                summary.push(AddLinkResult::rejected(line, &reason));
            }
        }
    }
    progress(total, total, "");
    applog::info(
        Stage::Task,
        format!(
            "批量添加完成：新增 {} 个号，更新 {} 个，拒绝 {} 条，跳过 {} 条",
            summary.added, summary.updated, summary.rejected, summary.skipped
        ),
    );
    summary
}

/// 抓一页（跟随 302，返回 HTML 文本）。
async fn fetch(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let resp = client.get(url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {}", status.as_u16());
    }
    Ok(resp.text().await?)
}

impl AddLinkResult {
    fn rejected(line: &str, reason: &str) -> Self {
        Self {
            line: line.to_string(),
            status: AddLinkStatus::Rejected,
            biz: None,
            nickname: None,
            reason: Some(reason.to_string()),
        }
    }
}

impl AddLinksSummary {
    fn push(&mut self, item: AddLinkResult) {
        match item.status {
            AddLinkStatus::Added => self.added += 1,
            AddLinkStatus::Updated => self.updated += 1,
            AddLinkStatus::Rejected => self.rejected += 1,
            AddLinkStatus::Skipped => self.skipped += 1,
        }
        self.items.push(item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 最小图文页：article-md 认 `rich_media_content` 版式；biz / nickname 在 script 里。
    fn rich(show_type: &str) -> String {
        format!(
            r#"<html><head><meta property="og:title" content="标题" /></head><body>
<script type="text/javascript">var item_show_type = "{show_type}";</script>
<script type="text/javascript">
  window.__allowLoadResFromMp = true;
  var biz = "" || "MzA5MTIxNzU=";
  var mid = "100" || "";
  var idx = "1" || "";
  var sn = "abc" || "";
  var hd_head_img = "https://mmbiz.qpic.cn/head/0";
  var round_head_img = "https:\/\/mmbiz.qpic.cn\/head\/round";
  var nickname = htmlDecode("测试公众号");
</script>
<div class="rich_media_content" id="js_content"><p>正文</p></div>
</body></html>"#
        )
    }

    #[test]
    fn parses_show_type() {
        assert_eq!(item_show_type(r#"var item_show_type = "0";"#), Some(0));
        assert_eq!(item_show_type(r#"var real_item_show_type = "8";"#), Some(8));
        assert_eq!(item_show_type("item_show_type: '10' * 1,"), Some(10));
        assert_eq!(
            item_show_type("item_show_type: 5,"),
            None,
            "不带引号的是模板代码"
        );
        assert_eq!(show_type_name(5), "视频分享");
        assert_eq!(show_type_name(7), "类型码 7");
    }

    #[test]
    fn resolves_rich_text_page() {
        let r = resolve_html("https://mp.weixin.qq.com/s/abc", &rich("0")).unwrap();
        assert_eq!(r.biz, "MzA5MTIxNzU=");
        assert_eq!(r.nickname, "测试公众号");
        assert_eq!(
            r.avatar.as_deref(),
            Some("https://mmbiz.qpic.cn/head/round")
        );
        assert_eq!(r.title.as_deref(), Some("标题"));
    }

    #[test]
    fn rejects_non_rich_types() {
        let e = resolve_html("u", &rich("5")).unwrap_err();
        assert!(matches!(e, ResolveError::Other(ref s) if s.contains("视频分享")));
        let e = resolve_html("u", &rich("10")).unwrap_err();
        assert!(matches!(e, ResolveError::Other(ref s) if s.contains("文字消息")));
    }

    #[test]
    fn rejects_verify_and_unavailable_pages() {
        let verify = r#"<html><script src="/mp/secitptpage/template/verify.js"></script></html>"#;
        assert_eq!(resolve_html("u", verify), Err(ResolveError::VerifyPage));
        let gone =
            r#"<html><body><div class="weui-msg"><p>该内容已被发布者删除</p></div></body></html>"#;
        let e = resolve_html("u", gone).unwrap_err();
        assert!(matches!(e, ResolveError::Other(ref s) if s.contains("文章不可用")));
    }

    #[test]
    fn rejects_page_without_show_type_or_name() {
        let html = rich("0").replace(r#"var item_show_type = "0";"#, "");
        let e = resolve_html("u", &html).unwrap_err();
        assert!(matches!(e, ResolveError::Other(ref s) if s.contains("无法识别文章类型")));
        let html = rich("0").replace(r#"var nickname = htmlDecode("测试公众号");"#, "");
        let e = resolve_html("u", &html).unwrap_err();
        assert!(matches!(e, ResolveError::Other(ref s) if s.contains("没有公众号名称")));
    }

    /// 出网（默认忽略，`cargo test -p mpider-core add_links_live -- --ignored` 手动跑）：两条公开图文短链
    /// 应能匿名解析出名称建号，并记下种子链接。
    #[tokio::test]
    #[ignore]
    async fn add_links_live_short_links() {
        let store = Store::open_in_memory().unwrap();
        let lines = vec![
            "https://mp.weixin.qq.com/s/kBnH6S9e0lkVKEO6inOcWg".to_string(),
            "https://mp.weixin.qq.com/s/1vIZpOQkLZUQxUHTIlNEpA".to_string(),
        ];
        let s = add_links(&store, &lines, &|d, t, l| eprintln!("{d}/{t} {l}")).await;
        eprintln!("{s:#?}");
        assert_eq!(s.added, 2, "{s:?}");
        let accounts = store.list_accounts().unwrap();
        assert_eq!(accounts.len(), 2);
        assert!(accounts
            .iter()
            .all(|a| a.nickname.is_some() && a.seed_url.is_some()));
        assert_eq!(
            store
                .latest_article_urls(&accounts[0].biz, 5)
                .unwrap()
                .len(),
            1
        );
    }

    /// 不出网：只校验行（长链 / 重复 / 非文章链接都被拒），有效短链为空时不发请求。
    #[tokio::test]
    async fn add_links_validates_lines_offline() {
        let store = Store::open_in_memory().unwrap();
        let lines = vec![
            "".to_string(),
            "https://mp.weixin.qq.com/s?__biz=A==&mid=1&idx=1&sn=x".to_string(),
            "https://example.com/s/abc".to_string(),
        ];
        let s = add_links(&store, &lines, &|_, _, _| {}).await;
        assert_eq!(s.rejected, 2);
        assert_eq!(s.items.len(), 2);
        assert!(s.items[0].reason.as_deref().unwrap().contains("只接受短链"));
        assert!(store.list_accounts().unwrap().is_empty());
    }
}
