//! 脚本注入接力（服务层）—— 纯函数，无副作用（对齐 Python 原型的同名模块）。
//!
//! MITM 在每个 `/s` 文章响应里注入一小段 JS：停留 `dwell_ms` 毫秒后把
//! `window.location` 指向队列里的下一条链接，从而在微信内置浏览器里“接力”逐条
//! 打开本批链接。本模块只负责“生成脚本 / 塞进 HTML / 生成独立接力页”，不碰队列状态。

use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use regex::Regex;

use crate::model::RelayRow;

/// 注入脚本的标记，便于识别/去重（同一页只注入一次）。与 Python `relay.MARKER` 一致。
pub const MARKER: &str = "__mpider_relay__";

/// 生成注入到 /s 文章页的接力脚本（固定停留 `dwell_ms`；等价 `build_relay_script_range(.., d, d)`）。
pub fn build_relay_script(next_url: Option<&str>, dwell_ms: i64) -> String {
    build_relay_script_range(next_url, dwell_ms, dwell_ms)
}

/// 生成接力脚本，停留时长在 `[dwell_min_ms, dwell_max_ms]` 内**随机**（页面里用 `Math.random`
/// 取，每跳不同）。固定节拍是可疑特征，参考项目 wx-shortlink-worker 也是随机区间。
///
/// `next_url` 为 `None`/空表示本批已到末条，注入一个“收尾”脚本（不再跳转）。
/// 下限 300ms（对齐 Python）；`dwell_max_ms < dwell_min_ms` 时按固定 `dwell_min_ms`。
pub fn build_relay_script_range(
    next_url: Option<&str>,
    dwell_min_ms: i64,
    dwell_max_ms: i64,
) -> String {
    build_relay_script_home(next_url, None, dwell_min_ms, dwell_max_ms)
}

/// 同 [`build_relay_script_range`]，但本批到末条时若给了 `home_url`（人工模式的待命页，见
/// `seedserver` 模块文档），停留后用 `location.replace` 把浏览器送回待命页（replace 不留历史，
/// 待命页再跳下一批时不会被「后退」干扰）；没给则只打收尾标记。
pub fn build_relay_script_home(
    next_url: Option<&str>,
    home_url: Option<&str>,
    dwell_min_ms: i64,
    dwell_max_ms: i64,
) -> String {
    let lo = dwell_min_ms.max(300);
    let hi = dwell_max_ms.max(lo);
    let span = hi - lo;
    let has_next = next_url.map(|u| !u.is_empty()).unwrap_or(false);
    let home = home_url.filter(|u| !u.is_empty());
    let body = if has_next {
        // serde_json 把 URL 安全转义成 JS 字符串字面量（等价 Python json.dumps）。
        let nxt = serde_json::to_string(next_url.unwrap()).unwrap_or_else(|_| "\"\"".to_string());
        format!(
            "var u={nxt};var d={lo}+Math.floor(Math.random()*{span1});setTimeout(function(){{try{{window.location.href=u;}}catch(e){{location.assign(u);}}}},d);",
            span1 = span + 1
        )
    } else if let Some(home) = home {
        let h = serde_json::to_string(home).unwrap_or_else(|_| "\"\"".to_string());
        format!(
            "window.__mpider_relay_done=true;var h={h};var d={lo}+Math.floor(Math.random()*{span1});setTimeout(function(){{try{{location.replace(h);}}catch(e){{location.href=h;}}}},d);",
            span1 = span + 1
        )
    } else {
        "window.__mpider_relay_done=true;".to_string()
    };
    format!(r#"<script id="{MARKER}">/*{MARKER}*/{body}</script>"#)
}

/// 生成**种子页 → 首条**这一跳的接力脚本：固定停留 `dwell_ms`（不随机、**下限 0**）后跳到 `next_url`。
///
/// 与 [`build_relay_script`] 的区别只在下限：文章页停留对齐 Python 有 300ms 下限（像人看一眼），
/// 种子页是本机服务直出的入口页、没内容要看，`seed_dwell_ms` 默认 100ms（2026-09-08 起），允许配 0 立即跳。
/// `next_url` 为空同 [`build_relay_script`]：注入收尾脚本。
pub fn build_seed_script(next_url: Option<&str>, dwell_ms: i64) -> String {
    let d = dwell_ms.max(0);
    let has_next = next_url.map(|u| !u.is_empty()).unwrap_or(false);
    let body = if has_next {
        let nxt = serde_json::to_string(next_url.unwrap()).unwrap_or_else(|_| "\"\"".to_string());
        format!(
            "var u={nxt};var d={d};setTimeout(function(){{try{{window.location.href=u;}}catch(e){{location.assign(u);}}}},d);"
        )
    } else {
        "window.__mpider_relay_done=true;".to_string()
    };
    format!(r#"<script id="{MARKER}">/*{MARKER}*/{body}</script>"#)
}

/// 判断该 HTML 是否已被注入过接力脚本（避免重复注入）。
pub fn already_injected(html_text: Option<&str>) -> bool {
    html_text.map(|h| h.contains(MARKER)).unwrap_or(false)
}

/// 把脚本注入 HTML：优先塞在 `</body>` 之前，没有 body 则追加到末尾。
///
/// 幂等：若已注入过（含 MARKER）则原样返回。
pub fn inject_into_html(html_text: &str, script: &str) -> String {
    if html_text.is_empty() {
        return script.to_string();
    }
    if already_injected(Some(html_text)) {
        return html_text.to_string();
    }
    static BODY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)</body\s*>").unwrap());
    if let Some(m) = BODY_RE.find(html_text) {
        let mut out = String::with_capacity(html_text.len() + script.len());
        out.push_str(&html_text[..m.start()]);
        out.push_str(script);
        out.push_str(&html_text[m.start()..]);
        out
    } else {
        format!("{html_text}{script}")
    }
}

/// 返回需要从响应头里删掉的 CSP 相关键名（大小写不敏感匹配）。
///
/// 纯函数：只判断该删哪些键，实际删除由调用方执行（对齐 Python `relax_csp_headers`）。
pub fn relax_csp_headers<'a, I>(header_keys: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let targets = [
        "content-security-policy",
        "content-security-policy-report-only",
    ];
    header_keys
        .into_iter()
        .filter(|k| targets.contains(&k.to_ascii_lowercase().as_str()))
        .map(|k| k.to_string())
        .collect()
}

/// 生成一个**独立**的接力页 HTML（备用/手工场景，对齐 Python `build_relay_page`）。
pub fn build_relay_page(links: &[String], dwell_ms: i64, title: &str) -> String {
    let safe_links: Vec<&String> = links.iter().filter(|u| !u.is_empty()).collect();
    let data = serde_json::to_string(&safe_links).unwrap_or_else(|_| "[]".to_string());
    let dwell = dwell_ms.max(300);
    let title_esc = html_escape(title);
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>{title_esc}</title></head><body>\
<h3>{title_esc}</h3>\
<p>共 {n} 条，将每 {dwell}ms 接力打开一条。</p>\
<pre id=\"log\"></pre><script>\
var L={data},i=0,D={dwell};\
var log=document.getElementById('log');\
function step(){{if(i>=L.length){{log.textContent+='\\n完成';return;}}\
var u=L[i++];log.textContent+='\\n打开 '+i+'/'+L.length+' '+u;\
window.location.href=u;}}\
setTimeout(step,300);</script></body></html>",
        n = safe_links.len(),
    )
}

// ---------------------------------------------------------------------------
// 进程内接力队列（P4）—— orchestrator 与 capture handler 共享同一实例
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Item {
    url: String,
    opened: bool,
    /// 停滞后被放回队尾重试过一次（再停滞就隔离，不再重试）。
    retried: bool,
    /// 这条任务链接**实际解析到的公众号 biz**（打开时从文章 HTML 抓到；短链 302 落地长链后
    /// 也在此登记）。回报只认队列各项的 resolved_biz——种子/散号不在队列里，天然被排除。
    resolved_biz: Option<String>,
}

/// 停滞处理结果（[`RelayQueue::requeue_inflight`]）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StallAction {
    /// 当前没有在途链接（浏览器被关 / 入口页还没打开），只能整体重点种子。
    NoInflight,
    /// 在途链接第一次卡住：放回队尾，稍后再试一次。
    Requeued(String),
    /// 在途链接再次卡住：标记 timeout 隔离出接力，用剩余队列续跑。
    Isolated(String),
}

#[derive(Default)]
struct State {
    items: Vec<Item>,
    /// 最近一次注入脚本让浏览器去跳的链接（参考项目的 inflight）：它的响应回来前都算在途。
    inflight: Option<String>,
    /// 最近一次微信 WebView 发出的 `/s` 文章请求时刻——接力唯一的心跳源。
    last_request_at: Option<Instant>,
    /// 命中人机验证页的链接（有值即整批中止）。
    verify_hit: Option<String>,
    /// 代理层请求整批中止的原因（如微信号不一致）；随 `set` / `clear` 一起清。
    abort: Option<String>,
    /// 两次停滞后被隔离出接力的链接。
    timeouts: Vec<String>,
}

/// 进程内共享的接力队列（`Arc<Mutex<..>>`，`Clone` 即共享同一份状态）。
///
/// Rust 版全在一个进程内，接力队列直接用内存共享，去掉了 Python 版跨进程用 SQLite
/// 的 `relay` 表这层 hack。除队列本身外还持有接力运行态（对齐参考项目 wx-shortlink-worker
/// 的状态机）：**在途链接** inflight、**心跳**（最后一次 `/s` 请求时间）、验证页命中、
/// 隔离名单——MITM 钩子写，orchestrator 的看门狗读。
///
/// `advance` 只认**精确 URL / mid 相同 / 短链 path 相同**的"当前这条"；匹配不上（用户误开
/// 的别的文章、预取、入口页 302 落地页）**不标记任何一条**，只返回下一条待跳——不再像 Python
/// `store.relay_advance` 那样兜底标"第一条未打开"，否则会把没访问过的链接当成已打开而漏抓。
///
/// 用法：orchestrator 起代理前 `set(本批链接)`，把**同一个** `RelayQueue` 传给
/// `capture::start`（`CaptureConfig::relay`）；MITM 响应钩子在每个 `/s` 上 `advance` 取下一条。
#[derive(Clone, Default)]
pub struct RelayQueue {
    inner: Arc<Mutex<State>>,
}

impl RelayQueue {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.lock().expect("relay queue mutex")
    }

    /// 重置队列为 links（清空旧队列与全部运行态，跳过空串）。返回入队条数。
    pub fn set(&self, links: &[String]) -> usize {
        let mut st = self.lock();
        *st = State::default();
        for url in links {
            if !url.is_empty() {
                st.items.push(Item {
                    url: url.clone(),
                    opened: false,
                    retried: false,
                    resolved_biz: None,
                });
            }
        }
        st.items.len()
    }

    /// 追加一条待打开链接（去重：已在队列里则不重复加）。返回是否真的入队。
    ///
    /// 用于短链解析：从文章 HTML 拼出的规范长链动态入队，接力据此继续打开长链抓凭证。
    pub fn push(&self, url: &str) -> bool {
        if url.is_empty() {
            return false;
        }
        let mut st = self.lock();
        if st.items.iter().any(|i| i.url == url) {
            return false;
        }
        st.items.push(Item {
            url: url.to_string(),
            opened: false,
            retried: false,
            resolved_biz: None,
        });
        true
    }

    /// 清空队列与全部运行态。
    pub fn clear(&self) {
        *self.lock() = State::default();
    }

    /// 未打开的条数。
    pub fn pending_count(&self) -> usize {
        self.lock().items.iter().filter(|i| !i.opened).count()
    }

    /// 全部未打开的链接（含被停滞放回队尾的），供中止时把剩余记为未完成。
    pub fn pending_links(&self) -> Vec<String> {
        self.lock()
            .items
            .iter()
            .filter(|i| !i.opened)
            .map(|i| i.url.clone())
            .collect()
    }

    /// 第一条未打开链接（**只看不改状态**）。
    pub fn first_pending(&self) -> Option<String> {
        self.lock()
            .items
            .iter()
            .find(|i| !i.opened)
            .map(|i| i.url.clone())
    }

    /// 供"固定入口页"注入接力：取本批第一条未打开链接并记为**在途**（不标记已打开——
    /// 入口页不是队列成员，不能用 `advance`）。
    pub fn bootstrap_next(&self) -> Option<String> {
        let mut st = self.lock();
        let next = st.items.iter().find(|i| !i.opened).map(|i| i.url.clone());
        st.inflight = next.clone();
        next
    }

    /// 当前队列全部行（seq/url/opened），供调试查看。
    pub fn rows(&self) -> Vec<RelayRow> {
        self.lock()
            .items
            .iter()
            .enumerate()
            .map(|(seq, i)| RelayRow {
                seq: seq as i64,
                url: i.url.clone(),
                opened: i.opened as i64,
            })
            .collect()
    }

    /// 接力推进：把"当前这条"标记已打开，返回下一条未打开链接的 URL（同时记为在途）；无则 `None`。
    ///
    /// 匹配"当前这条"：精确 URL > `mid` 查询参数相同 > 短链 `/s/<hash>` path 相同。三者都匹配
    /// 不上时——若有**在途**链接（infligh，最近注入让浏览器去跳的那条），认它已解析到本页
    /// （短链 302 跳长链就是这种：落地 `/s?__biz=..` 对不上原 `/s/<hash>`），标记在途已打开，
    /// 打破"反复跳回自身"的死循环；无在途才真视为队列外散页、**不标记**（见类型文档）。
    /// 幂等：当前这条已打开则不重复推进。
    pub fn advance(&self, current_url: Option<&str>, current_mid: Option<&str>) -> Option<String> {
        self.advance_with_biz(current_url, current_mid, None)
    }

    /// 同 [`Self::advance`]，但把本页解析到的公众号 `resolved_biz` 登记到被标记的那条队列项上
    /// （回报据此只报下发任务链接解析到的号）。短链 302 落地长链时，登记的是落地页的 biz。
    pub fn advance_with_biz(
        &self,
        current_url: Option<&str>,
        current_mid: Option<&str>,
        resolved_biz: Option<&str>,
    ) -> Option<String> {
        let mut st = self.lock();
        if st.items.is_empty() {
            st.inflight = None;
            return None;
        }
        let target = st
            .items
            .iter()
            .position(|i| item_matches(&i.url, current_url, current_mid));
        // 记下"这次标记了哪一条"，好把 resolved_biz 登记到它上面。
        let marked = match target {
            Some(idx) => {
                st.items[idx].opened = true;
                Some(idx)
            }
            None => {
                // 精确匹配不上：微信短链 `/s/<hash>` 常 302 跳规范长链 `/s?__biz=..&mid=..`，
                // 落地页 path 是 `/s`、也没有原短链的 mid，三种匹配全落空。此时若有**在途**链接
                // （最近一次注入让浏览器去跳的那条），就认它已解析到本页，把它标记已打开——否则
                // 它永远 pending，注入脚本每次都把 next 算成它、反复跳回自身造成死循环。
                // 仅在确有在途时兜底；无在途（散页/用户误开）保持不标记（见类型文档）。
                match st.inflight.clone() {
                    Some(infl) => match st.items.iter().position(|i| i.url == infl && !i.opened) {
                        Some(idx) => {
                            st.items[idx].opened = true;
                            Some(idx)
                        }
                        None => None,
                    },
                    None => None,
                }
            }
        };
        if let (Some(idx), Some(biz)) = (marked, resolved_biz.filter(|b| !b.is_empty())) {
            st.items[idx].resolved_biz = Some(biz.to_string());
        }
        let next = st.items.iter().find(|i| !i.opened).map(|i| i.url.clone());
        st.inflight = next.clone();
        next
    }

    /// 本批各任务链接**实际解析到的公众号 biz**（保序去重）。回报只认这些号——种子入口页
    /// 不在队列里，散页/误开也不会被 [`Self::advance_with_biz`] 登记，故都天然排除。
    pub fn resolved_bizs(&self) -> Vec<String> {
        let st = self.lock();
        let mut out: Vec<String> = Vec::new();
        for i in &st.items {
            if let Some(b) = i.resolved_biz.as_ref().filter(|b| !b.is_empty()) {
                if !out.contains(b) {
                    out.push(b.clone());
                }
            }
        }
        out
    }

    /// 各队列项 `(url, 解析到的 biz)`（只含已登记 biz 的项，保序）。编排器据此把任务链接的
    /// 「最后更新时间」映射到号上（短链 URL 本身无 `__biz`，只能靠接力时解析登记）。
    pub fn resolved_pairs(&self) -> Vec<(String, String)> {
        self.lock()
            .items
            .iter()
            .filter_map(|i| {
                i.resolved_biz
                    .as_ref()
                    .filter(|b| !b.is_empty())
                    .map(|b| (i.url.clone(), b.clone()))
            })
            .collect()
    }

    // ---- 运行态：心跳 / 在途 / 停滞 / 验证页 ----

    /// 记一次心跳：微信 WebView 刚发出了一条 `/s` 文章请求。
    pub fn touch(&self) {
        self.lock().last_request_at = Some(Instant::now());
    }

    /// 最近一次心跳时刻（自 `set`/`clear` 以来没有请求则 `None`）。
    pub fn last_request_at(&self) -> Option<Instant> {
        self.lock().last_request_at
    }

    /// 当前在途链接（最近一次注入让浏览器去跳的那条；它的响应回来后被 `advance` 换成下一条）。
    pub fn inflight(&self) -> Option<String> {
        self.lock().inflight.clone()
    }

    /// 接力停滞时处理在途链接（对齐参考项目）：第一次卡住放回队尾重试一次；再卡住标 timeout
    /// 隔离（标为已打开，不再跳它）。两种情况都清空在途，随后由调用方重点种子续跑剩余队列。
    pub fn requeue_inflight(&self) -> StallAction {
        let mut st = self.lock();
        let Some(url) = st.inflight.take() else {
            return StallAction::NoInflight;
        };
        let Some(idx) = st.items.iter().position(|i| i.url == url) else {
            return StallAction::NoInflight;
        };
        if st.items[idx].opened {
            // 已被 advance 标过（响应其实到了），不算卡住。
            return StallAction::NoInflight;
        }
        if st.items[idx].retried {
            st.items[idx].opened = true;
            st.timeouts.push(url.clone());
            StallAction::Isolated(url)
        } else {
            let mut item = st.items.remove(idx);
            item.retried = true;
            st.items.push(item);
            StallAction::Requeued(url)
        }
    }

    /// 两次停滞后被隔离出接力的链接（收尾时随剩余链接一起记为未完成）。
    pub fn timeouts(&self) -> Vec<String> {
        self.lock().timeouts.clone()
    }

    /// 记录命中人机验证页的链接（MITM 响应钩子调用；看门狗据此整批中止）。
    pub fn mark_verify(&self, url: &str) {
        let mut st = self.lock();
        if st.verify_hit.is_none() {
            st.verify_hit = Some(url.to_string());
        }
    }

    /// 本批是否命中了验证页（命中的链接）。
    pub fn verify_hit(&self) -> Option<String> {
        self.lock().verify_hit.clone()
    }

    /// 请求整批中止（代理层发现「登录的微信号与激活的不一致」等必须停下的情况）；只记第一条原因。
    pub fn mark_abort(&self, reason: &str) {
        let mut st = self.lock();
        if st.abort.is_none() {
            st.abort = Some(reason.to_string());
        }
    }

    /// 本批是否被要求中止（原因）。
    pub fn abort_reason(&self) -> Option<String> {
        self.lock().abort.clone()
    }
}

/// 取 URL 查询串里某个参数的值（不解码；够用于 mid 这类纯数字比对）。
fn url_query_param<'a>(url: &'a str, name: &str) -> Option<&'a str> {
    let q = url.split_once('?')?.1;
    let q = q.split('#').next().unwrap_or(q);
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == name).then_some(v)
    })
}

/// 取 URL 的 path 部分（`https://host/path?x` → `/path`）。
fn url_path_of(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    match rest.find('/') {
        Some(i) => &rest[i..],
        None => "/",
    }
}

/// 队列项是否就是"当前这条"：精确 URL；或 mid 参数相同；或短链 `/s/<hash>` path 相同。
fn item_matches(item_url: &str, current_url: Option<&str>, current_mid: Option<&str>) -> bool {
    if let Some(cu) = current_url {
        if item_url == cu {
            return true;
        }
    }
    if let Some(cm) = current_mid.filter(|m| !m.is_empty()) {
        if url_query_param(item_url, "mid") == Some(cm) {
            return true;
        }
    }
    if let Some(cu) = current_url {
        let cp = url_path_of(cu);
        if cp.starts_with("/s/") && url_path_of(item_url) == cp {
            return true;
        }
    }
    false
}

/// 最小 HTML 转义（对齐 Python `html.escape` 的默认行为：& < > 及引号）。
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_relay_inject() {
        let s = build_relay_script(Some("https://mp.weixin.qq.com/s?x=1"), 1000);
        assert!(s.contains(MARKER) && s.contains("location"));
        let html = "<html><body><p>hi</p></body></html>";
        let out = inject_into_html(html, &s);
        assert!(out.contains(MARKER));
        assert!(out.find(MARKER).unwrap() < out.find("</body>").unwrap());
        // 幂等：已注入不重复
        assert_eq!(inject_into_html(&out, &s), out);
        assert!(already_injected(Some(&out)));
        // 末条：不跳转
        let last = build_relay_script(None, 1000);
        assert!(!last.contains("location.href"));
        // 无 body 也能追加
        assert!(inject_into_html("<div>x</div>", &s).contains(MARKER));
    }

    #[test]
    fn test_relay_csp_and_page() {
        let keys = ["Content-Type", "content-security-policy"];
        assert_eq!(
            relax_csp_headers(keys),
            vec!["content-security-policy".to_string()]
        );
        let page = build_relay_page(&["u1".to_string(), "u2".to_string()], 1000, "t");
        assert!(page.contains("u1") && page.contains("u2") && page.contains("接力"));
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// 末条回落：给了待命页地址才 `location.replace` 回去（随机停留同文章页），没给 / 空串只打收尾标记；有下一条时不回落。
    #[test]
    fn test_relay_tail_returns_home_only_when_given() {
        let home = "http://127.0.0.1:8787/";
        let s = build_relay_script_home(None, Some(home), 500, 900);
        assert!(s.contains("__mpider_relay_done"), "{s}");
        assert!(s.contains("location.replace") && s.contains(home), "{s}");
        assert!(s.contains("var d=500+"), "{s}");
        let plain = build_relay_script_home(None, None, 500, 900);
        assert!(plain.contains("__mpider_relay_done") && !plain.contains("location.replace"));
        assert!(!build_relay_script_home(None, Some(""), 500, 900).contains("location.replace"));
        let next =
            build_relay_script_home(Some("https://mp.weixin.qq.com/s/x"), Some(home), 500, 900);
        assert!(
            next.contains("/s/x") && !next.contains("location.replace"),
            "{next}"
        );
        // 老入口等价于不回落
        assert_eq!(build_relay_script_range(None, 500, 900), plain);
    }

    #[test]
    fn test_relay_script_random_dwell_range() {
        let sc = build_relay_script_range(Some("https://x/s?a=1&b=2"), 2000, 4000);
        // 下限 + Math.random()*(区间+1)
        assert!(sc.contains("var d=2000+Math.floor(Math.random()*2001)"));
        // URL 经 JSON 转义成字面量（& = 原样，不会拼断语法）
        assert!(sc.contains(r#"var u="https://x/s?a=1&b=2";"#));
        // max < min → 固定 min
        let fixed = build_relay_script_range(Some("u"), 3000, 100);
        assert!(fixed.contains("var d=3000+Math.floor(Math.random()*1)"));
        // 下限 300
        assert!(build_relay_script(Some("u"), 10).contains("var d=300+"));
    }

    #[test]
    fn test_seed_script_fixed_dwell_no_floor() {
        // 种子页脚本：固定停留、无 300 下限、不随机
        let sc = build_seed_script(Some("https://x/s?a=1&b=2"), 100);
        assert!(sc.contains("var d=100;"), "{sc}");
        assert!(!sc.contains("Math.random"));
        assert!(sc.contains(r#"var u="https://x/s?a=1&b=2";"#));
        assert!(sc.contains(MARKER));
        assert!(build_seed_script(Some("u"), -5).contains("var d=0;"));
        assert!(build_seed_script(None, 100).contains("__mpider_relay_done"));
    }

    #[test]
    fn test_relay_queue_advance() {
        let q = RelayQueue::new();
        assert_eq!(q.set(&s(&["u0", "u1", "u2"])), 3);
        assert_eq!(q.pending_count(), 3);
        // 第一次推进：标记 u0，返回 u1，u1 成为在途
        assert_eq!(q.advance(Some("u0"), None).as_deref(), Some("u1"));
        assert_eq!(q.inflight().as_deref(), Some("u1"));
        // 重复同页（u0 已 opened）：不重复推进，返回下一条未打开 u1
        assert_eq!(q.advance(Some("u0"), None).as_deref(), Some("u1"));
        // 按 mid 匹配推进：必须是 mid 参数相等，不是子串（900 不能命中 mid=2659000）
        q.set(&s(&[
            "https://x/s?mid=2659000&idx=1",
            "https://x/s?__biz=B&mid=900&idx=1",
        ]));
        assert_eq!(
            q.advance(None, Some("900")).as_deref(),
            Some("https://x/s?mid=2659000&idx=1")
        );
        assert_eq!(q.pending_count(), 1);
        // 末条推进：返回 None，在途清空
        assert_eq!(q.advance(None, Some("2659000")), None);
        assert!(q.inflight().is_none());
        q.clear();
        assert_eq!(q.pending_count(), 0);

        // Clone 共享同一状态
        let q2 = q.clone();
        q.set(&s(&["a", "b"]));
        assert_eq!(q2.pending_count(), 2);
    }

    #[test]
    fn test_relay_queue_stray_page_does_not_mark() {
        // 队列外的页面（用户误开别的文章 / 入口页 302 落地页）：不标记任何一条，只给出下一条。
        let q = RelayQueue::new();
        q.set(&s(&[
            "https://x/s?__biz=A&mid=1&idx=1",
            "https://x/s?__biz=A&mid=2&idx=1",
        ]));
        assert_eq!(
            q.advance(Some("https://x/s?__biz=Z&mid=777&idx=1"), Some("777"))
                .as_deref(),
            Some("https://x/s?__biz=A&mid=1&idx=1")
        );
        assert_eq!(q.pending_count(), 2, "旧逻辑会把 mid=1 误标已打开而漏抓");
    }

    #[test]
    fn test_relay_advance_marks_inflight_on_shortlink_302() {
        // 短链 302 落地成长链：落地 `/s?__biz=..&mid=..` 对不上队列里的 `/s/<hash>`（path/mid 都不同），
        // 但它是在途链接——兜底把 inflight 标记已打开，避免死循环（复现 WEB-9 现象）。
        let q = RelayQueue::new();
        q.set(&s(&[
            "https://mp.weixin.qq.com/s/HASH1",
            "https://mp.weixin.qq.com/s/HASH2",
        ]));
        // 前一页把 HASH1 记为在途（advance 无匹配也会 set inflight；这里用 bootstrap_next 直接置）
        assert_eq!(
            q.bootstrap_next().as_deref(),
            Some("https://mp.weixin.qq.com/s/HASH1")
        );
        assert_eq!(
            q.inflight().as_deref(),
            Some("https://mp.weixin.qq.com/s/HASH1")
        );
        // HASH1 落地长链 /s?__biz=A&mid=1（path=/s，对不上 /s/HASH1）→ 兜底标记在途 HASH1
        assert_eq!(
            q.advance(
                Some("https://mp.weixin.qq.com/s?__biz=A&mid=1&idx=1"),
                Some("1")
            )
            .as_deref(),
            Some("https://mp.weixin.qq.com/s/HASH2")
        );
        assert_eq!(q.pending_count(), 1, "HASH1 应被标记，仅剩 HASH2");
        // HASH2 同理落地长链 → 标记在途 HASH2 → 队列清空（done 得以成立）
        assert_eq!(
            q.advance(
                Some("https://mp.weixin.qq.com/s?__biz=B&mid=2&idx=1"),
                Some("2")
            ),
            None
        );
        assert_eq!(q.pending_count(), 0);
    }

    #[test]
    fn test_relay_resolved_bizs_scopes_to_queue_items() {
        // 回报只认队列各项解析到的号：短链 302 落地长链，用 advance_with_biz 登记落地 biz；
        // 队列外抓到的号（种子/散号）不在此列。
        let q = RelayQueue::new();
        q.set(&s(&["https://mp.weixin.qq.com/s/TASKHASH"]));
        assert!(q.resolved_bizs().is_empty());
        // 前一页把 TASKHASH 记为在途（如种子入口注入跳它）
        q.bootstrap_next();
        // 落地长链 /s?__biz=TASKBIZ==（对不上 /s/TASKHASH）→ 兜底标记在途 + 登记 TASKBIZ==
        q.advance_with_biz(
            Some("https://mp.weixin.qq.com/s?__biz=TASKBIZ==&mid=9&idx=1"),
            Some("9"),
            Some("TASKBIZ=="),
        );
        assert_eq!(q.resolved_bizs(), vec!["TASKBIZ==".to_string()]);
        assert_eq!(q.pending_count(), 0);
        // 无在途时的散页（种子号落地）不会被登记
        let q2 = RelayQueue::new();
        q2.set(&s(&["https://mp.weixin.qq.com/s/TASKHASH"]));
        q2.advance_with_biz(
            Some("https://mp.weixin.qq.com/s?__biz=SEEDBIZ==&mid=1&idx=1"),
            Some("1"),
            Some("SEEDBIZ=="),
        );
        assert!(
            q2.resolved_bizs().is_empty(),
            "无在途的散页（种子号）不应登记进 resolved_bizs"
        );
        assert_eq!(q2.pending_count(), 1, "散页不消耗队列");
    }

    #[test]
    fn test_relay_queue_push_dedup() {
        // 短链解析场景：种子只有短链，解析出长链后 push 进队，接力据此打开长链。
        let q = RelayQueue::new();
        assert_eq!(q.set(&s(&["https://mp.weixin.qq.com/s/HASH"])), 1);
        let long = "https://mp.weixin.qq.com/s?__biz=B==&mid=1&idx=1&sn=x";
        assert!(q.push(long)); // 新链入队
        assert!(!q.push(long)); // 去重：已在队列
        assert!(!q.push("")); // 空串不入队
        assert_eq!(q.pending_count(), 2);
        // 短链打开后 advance → 返回刚 push 的长链；请求 URL 只要 path 相同即可匹配（scheme 不同也行）
        assert_eq!(
            q.advance(Some("http://mp.weixin.qq.com/s/HASH"), None)
                .as_deref(),
            Some(long)
        );
        assert_eq!(q.pending_count(), 1);
    }

    #[test]
    fn test_relay_first_pending_and_bootstrap_next() {
        // 固定入口页用 bootstrap_next 取本批首条任务链接：记在途但**不标记已打开**。
        let q = RelayQueue::new();
        assert!(q.first_pending().is_none());
        assert!(q.bootstrap_next().is_none());
        q.set(&s(&["u0", "u1"]));
        assert_eq!(q.first_pending().as_deref(), Some("u0"));
        assert_eq!(q.bootstrap_next().as_deref(), Some("u0"));
        assert_eq!(q.inflight().as_deref(), Some("u0"));
        assert_eq!(q.pending_count(), 2);
        // 打开 u0 后，first_pending 变 u1
        q.advance(Some("u0"), None);
        assert_eq!(q.first_pending().as_deref(), Some("u1"));
    }

    #[test]
    fn test_relay_stall_requeue_then_isolate() {
        let q = RelayQueue::new();
        q.set(&s(&["u0", "u1", "u2"]));
        // 没有在途：无从重排
        assert_eq!(q.requeue_inflight(), StallAction::NoInflight);
        // 入口页注入 u0 → 在途 u0；u0 卡住 → 放回队尾，重试一次
        q.bootstrap_next();
        assert_eq!(q.requeue_inflight(), StallAction::Requeued("u0".into()));
        assert!(q.inflight().is_none());
        assert_eq!(q.first_pending().as_deref(), Some("u1"));
        assert_eq!(q.rows().last().unwrap().url, "u0");
        // 重点种子 → u1、u2 顺利 → 到 u0 再卡 → 隔离
        q.bootstrap_next(); // 在途 u1
        assert_eq!(q.advance(Some("u1"), None).as_deref(), Some("u2"));
        assert_eq!(q.advance(Some("u2"), None).as_deref(), Some("u0"));
        assert_eq!(q.requeue_inflight(), StallAction::Isolated("u0".into()));
        assert_eq!(q.pending_count(), 0);
        assert_eq!(q.timeouts(), vec!["u0".to_string()]);
        // 在途已被 advance 标过（响应其实到了）：不算卡住
        q.set(&s(&["a"]));
        q.bootstrap_next();
        q.advance(Some("a"), None);
        assert_eq!(q.requeue_inflight(), StallAction::NoInflight);
    }

    #[test]
    fn test_relay_heartbeat_and_verify_reset_on_set() {
        let q = RelayQueue::new();
        assert!(q.last_request_at().is_none());
        q.touch();
        assert!(q.last_request_at().is_some());
        q.mark_verify("bad");
        q.mark_verify("later"); // 只记第一条
        assert_eq!(q.verify_hit().as_deref(), Some("bad"));
        q.set(&s(&["x"]));
        assert!(q.last_request_at().is_none());
        assert!(q.verify_hit().is_none());
    }

    #[test]
    fn test_url_helpers() {
        assert_eq!(
            url_query_param("https://x/s?__biz=B&mid=12&idx=1#f", "mid"),
            Some("12")
        );
        assert_eq!(url_query_param("https://x/s/HASH", "mid"), None);
        assert_eq!(
            url_path_of("https://mp.weixin.qq.com/s/HASH?x=1"),
            "/s/HASH"
        );
        assert_eq!(url_path_of("https://mp.weixin.qq.com"), "/");
        assert!(item_matches("https://a/s?mid=5&idx=1", None, Some("5")));
        assert!(!item_matches("https://a/s?mid=55&idx=1", None, Some("5")));
        assert!(item_matches(
            "https://a/s/H",
            Some("https://a/s/H?from=x"),
            None
        ));
        assert!(!item_matches(
            "https://a/s?mid=1",
            Some("https://a/s?mid=2"),
            Some("")
        ));
    }
}
