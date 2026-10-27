//! ⭐ 可行性核心：MITM 抓凭证 + TLS 直通（对齐 Python `proxy_addon.py` 的钩子语义）。
//!
//! 用 hudsucker（hyper 1 + rustls + tokio）起一个本地 MITM 代理，运行期用 rcgen 生成的
//! 根 CA（见 [`crate::ca`]）动态签叶证书。三件事：
//! 1. **抓凭证**：对 host==`mp.weixin.qq.com` 的请求，从 URL query + 表单 body + Cookie
//!    抽凭证入库（[`crate::proxy_addon::extract_credentials`]），命中 `/s?` 记文章骨架。
//! 2. **接力注入**：对 `/s` 的 HTML 响应、以及**固定入口页**（种子链接的主文档；现在种子是本机
//!    种子入口服务 [`crate::seedserver`]，其页面自带接力脚本、且回环地址通常不经代理，这里的入口页
//!    分支只在种子 host 配成局域网 IP 等经代理的情况下兜底）删 CSP 头、注入“跳下一条”的接力脚本
//!    （[`crate::relay`]），下一条从进程内 [`RelayQueue`] 取。入口页只起跳、不落号/文章；任务文章页
//!    再逐跳接力。注入有四道闸（对齐参考项目 wx-shortlink-worker）：接力开启且队列非空、UA 含
//!    `MicroMessenger`（同机 Chrome 不注入）、只注入主文档（`Sec-Fetch-Dest: document`，iframe 不注入
//!    以免多个定时器并存）、脚本插在 `</body>` 前。命中人机验证页则不注入并记 `verify_hit`，由
//!    orchestrator 整批中止。
//! 3. **TLS 直通**：hudsucker 0.25 原生支持 `should_intercept_tls(client_hello)` 钩子
//!    —— 返回 `false` 时它会裸 TCP `copy_bidirectional` 直通（等价 mitmproxy 的
//!    `tls_clienthello.ignore_connection`）。对固定证书主机与**无 SNI** 的连接直通，
//!    避免弄断微信主协议（mmtls）。
//!
//! ## 可行性结论（写在代码里，便于对照）
//! hudsucker 0.25 **直接支持按 SNI 选择性直通**：`HttpHandler::should_intercept_tls`
//! 拿到 `rustls::server::ClientHello`（`server_name()` 即 SNI），返回 `false` 即直通。
//! 因此**无需**自写 CONNECT 分流层、也**无需** tls-parser 手动解析 ClientHello（备选路径若
//! 未来真要在 hudsucker 之前再做一层 SNI 预判，可另加 tls-parser，本阶段不引入）。
//!
//! ### 已知降级项
//! - hudsucker 0.25 的 `HttpHandler` **没有** “客户端拒绝我方证书(pinning)” 的回调
//!   （Python 的 `tls_failed_client`）。因此“运行期学习拒证主机→记忆直通”无法自动触发；
//!   [`PassthroughDecider::learn`] 仍保留手动/外部喂入的接口，静态固定名单
//!   （`is_wechat_pinned_host`）+ 无 SNI 直通两条主规则已覆盖微信主协议。

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::Result;
use http_body_util::BodyExt;
use hudsucker::certificate_authority::RcgenAuthority;
use hudsucker::hyper::http::response::Parts as ResponseParts;
use hudsucker::hyper::{header, Method, Request, Response};
use hudsucker::rustls::crypto::aws_lc_rs;
use hudsucker::rustls::server::ClientHello;
use hudsucker::{decode_response, Body, HttpContext, HttpHandler, Proxy, RequestOrResponse};
use regex::Regex;
use tracing::{debug, error, info};

use crate::applog::{self, Stage};
use crate::ca::CaMaterial;
use crate::model::CredentialFields;
use crate::proxy_addon::{
    extract_credentials, has_session, is_wechat_pinned_host, long_url_from_meta,
    parse_account_avatar_from_html, parse_account_name_from_html, parse_article_meta_from_html,
    parse_s_url, TARGET_HOST,
};
use crate::relay::{self, RelayQueue};
use crate::store::Store;

/// **旧的外网固定入口种子链接**（2026-09-04 ～ 09-07 用；现仅测试 / 兼容保留）。
///
/// 取 URL 的 path（解析失败返回 None）——用于识别"当前页是否固定入口链接"。
fn url_path(u: &str) -> Option<String> {
    url::Url::parse(u).ok().map(|p| p.path().to_string())
}

/// 取 URL 的 host（解析失败返回 None）——固定入口链接按 host 识别（种子是独立域名）。
fn url_host(u: &str) -> Option<String> {
    url::Url::parse(u)
        .ok()
        .and_then(|p| p.host_str().map(str::to_string))
}

/// 从一条**已解密**的 `mp.weixin.qq.com` 请求里抓凭证 + 记文章骨架（`handle_request` 的
/// 核心，抽出便于脱代理单测）。返回本条命中的 mid（供响应接力关联）。
///
/// 语义对齐 Python `proxy_addon.request`：只有含真正会话材料（`has_session`）才写凭证；
/// 命中 `/s?` 才记文章骨架（sn + content_url）。
pub fn capture_request(
    store: &Store,
    url: &str,
    cookie: Option<&str>,
    body: Option<&str>,
) -> Option<String> {
    capture_request_with(store, None, url, cookie, body)
}

/// [`capture_request`] 的完整版：带接力队列时，抓到的 `uin` 与**激活微信号**不一致会
/// `RelayQueue::mark_abort` 请求整批中止（编排器 / 续期的等待循环据此收场），凭证不入库。
pub fn capture_request_with(
    store: &Store,
    relay: Option<&RelayQueue>,
    url: &str,
    cookie: Option<&str>,
    body: Option<&str>,
) -> Option<String> {
    let creds = extract_credentials(url, body, cookie);
    if let Some(biz) = creds.biz.clone() {
        if has_session(&creds) {
            // 多微信号：按 uin 自动登记 / 比对（首次登记并设为当前 / 一致 / 属于别的号）。
            if let Some(uin) = creds.uin.as_deref().filter(|u| !u.is_empty()) {
                use crate::model::WxBind;
                let mismatch = |alias: String, captured_alias: String| {
                    let reason =
                        format!("登录的是微信号『{captured_alias}』，当前激活的是『{alias}』");
                    if relay.map(|r| r.abort_reason().is_none()).unwrap_or(false) {
                        applog::error(
                            Stage::Capture,
                            format!("⛔ {reason}：凭证不入库，本批中止。请在「微信号管理」激活『{captured_alias}』后重试，或在微信客户端切回『{alias}』"),
                        );
                        crate::notify::notify(
                            crate::notify::Kind::Task,
                            format!("{reason}，本批采集已中止；凭证未入库"),
                        );
                    }
                    if let Some(r) = relay {
                        r.mark_abort(&reason);
                    }
                };
                match store.wx_note_captured_uin(uin) {
                    Ok(WxBind::Mismatch {
                        alias,
                        captured_alias,
                    }) => {
                        mismatch(alias, captured_alias);
                        return None;
                    }
                    Ok(WxBind::Registered {
                        id,
                        activated: false,
                    }) => {
                        let captured_alias = store
                            .wx_get(id)
                            .ok()
                            .flatten()
                            .map(|a| a.alias)
                            .unwrap_or_else(|| format!("微信号 {id}"));
                        let alias = store
                            .wx_active()
                            .ok()
                            .flatten()
                            .map(|a| a.alias)
                            .unwrap_or_default();
                        applog::info(
                            Stage::Capture,
                            format!("已登记新的微信号『{captured_alias}』（未激活）"),
                        );
                        mismatch(alias, captured_alias);
                        return None;
                    }
                    Ok(WxBind::Registered {
                        id,
                        activated: true,
                    }) => {
                        // 首次运行前的标定落在旧的单文件上：归到这个号名下，并让后续点击走它的文件。
                        crate::rpa::migrate_legacy_click_cache(id);
                        crate::rpa::set_active_wx_id(Some(id));
                        let alias = store
                            .wx_get(id)
                            .ok()
                            .flatten()
                            .map(|a| a.alias)
                            .unwrap_or_else(|| format!("微信号 {id}"));
                        applog::info(Stage::Capture, format!("已登记微信号『{alias}』并设为当前"));
                    }
                    Ok(WxBind::Same) => {}
                    Err(e) => applog::warn(Stage::Capture, format!("微信号登记失败：{e}")),
                }
            }
            let fields = CredentialFields {
                uin: creds.uin.clone(),
                key: creds.key.clone(),
                pass_ticket: creds.pass_ticket.clone(),
                wxtoken: creds.wxtoken.clone(),
                x5: creds.x5.clone(),
                appmsg_token: creds.appmsg_token.clone(),
                cookie: cookie.map(str::to_string),
                extra: None,
            };
            let _ = store.upsert_account(&biz, None, None);
            match store.upsert_credential(&biz, &fields) {
                Err(e) => {
                    error!(error = %e, "upsert_credential failed");
                    applog::error(Stage::Capture, format!("凭证写库失败：{e}"));
                }
                // 只在真正换了 key 时记（文章页会反复发同 key 的请求，不重复刷日志）。
                Ok(true) => {
                    info!(biz = %biz, "captured credential");
                    applog::info(
                        Stage::Capture,
                        format!(
                            "凭证获取成功：{}（key {}，有效期约 {} 分钟）",
                            store.account_label(&biz),
                            creds
                                .key
                                .as_deref()
                                .map(applog::key_preview)
                                .unwrap_or_else(|| "无".into()),
                            store.cred_ttl() / 60
                        ),
                    );
                }
                Ok(false) => {}
            }
        }
    }

    let mut mid = None;
    if let Some(art) = parse_s_url(url) {
        mid = Some(art.mid.clone());
        let _ = store.upsert_account(&art.biz, None, None);
        let fields = crate::model::ArticleFields {
            sn: art.sn.clone(),
            content_url: Some(art.content_url.clone()),
            ..Default::default()
        };
        if let Err(e) = store.upsert_article(&art.biz, &art.mid, art.idx, &fields) {
            error!(error = %e, "upsert_article failed");
        } else {
            info!(biz = %art.biz, mid = %art.mid, idx = art.idx, "captured article");
        }
    }
    mid
}

/// 直通决策（对齐 Python `tls_clienthello` 的判定优先级）。
///
/// 1) **无 SNI 一律直通**（微信 mmtls 主协议不带 SNI，若误 MITM 会弄断微信）。
/// 2) SNI 命中固定证书主机后缀（`is_wechat_pinned_host`）。
/// 3) SNI 命中运行期学习到的拒证主机（`learn` 喂入）。
#[derive(Clone, Default)]
pub struct PassthroughDecider {
    learned: Arc<Mutex<HashSet<String>>>,
}

impl PassthroughDecider {
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否应直通（跳过 MITM）。
    pub fn should_passthrough(&self, sni: Option<&str>) -> bool {
        match sni {
            None => true, // 无 SNI 一律直通
            Some("") => true,
            Some(s) => {
                if is_wechat_pinned_host(Some(s)) {
                    return true;
                }
                self.learned.lock().expect("decider mutex").contains(s)
            }
        }
    }

    /// 记住一个“客户端拒绝我方证书”的主机，下次直通（供外部/日志学习喂入）。
    pub fn learn(&self, host: &str) {
        self.learned
            .lock()
            .expect("decider mutex")
            .insert(host.to_string());
    }
}

/// handle_request 记下的"本条请求"，供 handle_response 关联。
///
/// hudsucker 的响应钩子不给请求，只给 `client_addr`；而它**每个请求都 clone 一份 handler**
/// （`InternalProxy::clone().proxy(req)`），同一 clone 上先后调 `handle_request` /
/// `handle_response`，所以直接存在 handler 自己的字段里即可——**不能**按 `client_addr` 共享
/// map 关联：hudsucker 默认开 HTTP/2，微信 WebView 同一连接多流并发，上一页卸载时的上报
/// 请求会覆盖掉 `/s` 的记录，`/s` 响应被当成非文章页不注入，接力静默断掉。
#[derive(Clone, Debug, Default)]
pub struct LastReq {
    pub host: String,
    pub path: String,
    pub url: String,
    pub mid: Option<String>,
    /// UA 含 `MicroMessenger`（微信内置浏览器）；否则是同机别的浏览器，不注入、不计心跳。
    pub wechat_ua: bool,
    /// 主文档导航（`Sec-Fetch-Dest: document`；没带该头的老内核视为主文档）。
    pub is_document: bool,
}

impl LastReq {
    /// 由请求头算出关联信息（抽出便于单测）。
    pub fn from_parts(
        host: &str,
        path: &str,
        url: &str,
        mid: Option<String>,
        user_agent: Option<&str>,
        sec_fetch_dest: Option<&str>,
    ) -> Self {
        Self {
            host: host.to_string(),
            path: path.to_string(),
            url: url.to_string(),
            mid,
            wechat_ua: user_agent
                .map(|ua| ua.contains("MicroMessenger"))
                .unwrap_or(false),
            is_document: sec_fetch_dest
                .map(|d| d.eq_ignore_ascii_case("document"))
                .unwrap_or(true),
        }
    }

    /// 是否 `mp.weixin.qq.com` 的 `/s` 文章页（长链 `/s?` 或短链 `/s/<hash>`）。
    pub fn is_article(&self) -> bool {
        self.host == TARGET_HOST && (self.path == "/s" || self.path.starts_with("/s/"))
    }
}

/// MITM 抓凭证 + 接力注入的 HTTP 处理器。
#[derive(Clone)]
pub struct CaptureHandler {
    store: Arc<Store>,
    decider: PassthroughDecider,
    relay_enabled: bool,
    relay_dwell_ms: i64,
    /// 停留上限（随机区间 `[relay_dwell_ms, relay_dwell_max_ms]`；小于下限则固定停留）。
    relay_dwell_max_ms: i64,
    /// 种子入口页 → 本批首条的固定等待（毫秒），与文章页的随机区间分开配置。
    seed_dwell_ms: i64,
    /// 进程内接力队列（与 orchestrator 共享同一实例）。
    relay: RelayQueue,
    /// 固定入口链接的 host（种子是独立域名，按 host 识别入口页；见 [`Self::is_bootstrap`]）。
    /// None = 无固定入口（老流程/测试）。
    bootstrap_host: Option<String>,
    /// 固定入口链接的 path。仅当种子 host 恰好是 `mp.weixin.qq.com`（历史/测试）时才用它精确到某篇文章，
    /// 避免把该 host 的所有文章都当入口；种子是独立域名时不看 path（该域名任意主文档都是入口）。
    bootstrap_path: Option<String>,
    /// 观测/测试用：记录本处理器解密看到的每条请求 URL（证明 MITM 生效）。
    seen: Arc<Mutex<Vec<String>>>,
    /// 本条请求的关联信息（每请求一份 handler clone，见 [`LastReq`]）。
    last: Option<LastReq>,
    /// 种子入口页的 biz（从其 HTML 解析、见到即记；跨 clone 共享）。种子页仅作接力入口，其
    /// 自身随后发出的 `getappmsgext`（带 key）凭证**必须跳过**，否则种子链接所属的那个公众号会
    /// 被当成本批成果误采、误报，并会提前满足短链批收尾判据。None = 尚未见到种子页 / 无固定入口。
    seed_biz: Arc<Mutex<Option<String>>>,
    /// 请求追踪（调试）：把每条解密到的 `mp.weixin.qq.com` 请求的**路径**（不含参数）与响应概况写进环节日志，
    /// 供「手动抓凭证代理」（[`crate::manualcap`]）验证某类链接在微信内置浏览器里是否经代理、是否带凭证。
    /// 正常采集不开（每篇文章页会带几十条子请求，会刷满日志）。
    trace_requests: bool,
    /// 人工模式的待命页地址（种子链接）：本批接力到末条后把内置浏览器送回去待命，下一批由它自动接力
    /// （见 `seedserver` 模块文档）。`None`（Windows 自动点击 / 手动抓凭证）= 末条只打收尾标记。
    resident_home: Option<String>,
}

impl CaptureHandler {
    // 参数与 `CaptureConfig` 一一对应，拆成 builder 反而绕；调用点只有 `start_on` 与测试。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<Store>,
        decider: PassthroughDecider,
        relay_enabled: bool,
        relay_dwell_ms: i64,
        relay_dwell_max_ms: i64,
        seed_dwell_ms: i64,
        relay: RelayQueue,
        bootstrap_seed: Option<String>,
    ) -> Self {
        Self {
            store,
            decider,
            relay_enabled,
            relay_dwell_ms,
            relay_dwell_max_ms,
            seed_dwell_ms,
            relay,
            bootstrap_host: bootstrap_seed.as_deref().and_then(url_host),
            bootstrap_path: bootstrap_seed.as_deref().and_then(url_path),
            seen: Arc::new(Mutex::new(Vec::new())),
            last: None,
            seed_biz: Arc::new(Mutex::new(None)),
            trace_requests: false,
            resident_home: None,
        }
    }

    /// 设置 / 清除待命页回落地址（见 [`Self::resident_home`]）。
    pub fn with_resident_home(mut self, home: Option<String>) -> Self {
        self.resident_home = home.filter(|h| !h.trim().is_empty());
        self
    }

    /// 开 / 关请求追踪（见 [`Self::trace_requests`]）。
    pub fn with_trace(mut self, on: bool) -> Self {
        self.trace_requests = on;
        self
    }

    /// 本处理器迄今解密看到的请求 URL（观测/测试）。
    pub fn seen_urls(&self) -> Vec<String> {
        self.seen.lock().expect("seen mutex").clone()
    }

    /// 这条请求是否命中**固定入口页**。种子是独立域名时按 host 识别（该域名任意主文档都是入口，
    /// 对末尾斜杠/子路径更健壮）；仅当种子 host 恰是 `mp.weixin.qq.com`（历史/测试）时才附加 path
    /// 精确匹配，避免把该 host 的普通文章误当入口。
    fn is_bootstrap(&self, last: &LastReq) -> bool {
        match (
            self.bootstrap_host.as_deref(),
            self.bootstrap_path.as_deref(),
        ) {
            (Some(h), path) => {
                last.host == h && (h != TARGET_HOST || path == Some(last.path.as_str()))
            }
            _ => false,
        }
    }

    /// 记住种子账号 biz（幂等）：其后续带 key 的凭证请求要跳过，不作采集目标。
    fn set_seed_biz(&self, biz: &str) {
        if biz.is_empty() {
            return;
        }
        let mut g = self.seed_biz.lock().expect("seed_biz mutex");
        if g.as_deref() != Some(biz) {
            info!(biz = %biz, "记录种子账号 biz（种子仅作接力入口，不采集）");
            *g = Some(biz.to_string());
        }
    }

    /// 从种子入口页 HTML 记住种子账号 biz（种子页直接返回 HTML 时走这条）。
    fn note_seed_biz(&self, html: &str) {
        if let Some(biz) = parse_article_meta_from_html(Some(html))
            .get("biz")
            .filter(|s| !s.is_empty())
        {
            self.set_seed_biz(biz);
        }
    }

    /// 从 URL 的 `__biz` 记住种子账号 biz（种子短链 `/s/<hash>` **302 跳长链**时，其响应非 HTML、
    /// 走不到 [`Self::note_seed_biz`]，但 3xx 的 `Location` 头带 `__biz=`，从中取）。
    fn note_seed_biz_from_url(&self, url: &str) {
        if let Some(biz) = extract_credentials(url, None, None)
            .biz
            .filter(|b| !b.is_empty())
        {
            self.set_seed_biz(&biz);
        }
    }

    /// 抓凭证 + 记文章骨架（`capture_request`），但**跳过种子账号自身的凭证**。
    ///
    /// 种子页仅用于拉起浏览器 + 注入接力；其页面 JS 随后会发 `getappmsgext`（带 key），
    /// 若照单全收，种子号会被 `bizs_captured_since` 当成本批成果误采、误报，并提前满足短链批
    /// 收尾判据。种子号 biz 在 [`Self::note_seed_biz`] 里（种子页响应时）已先记下，故此处能命中。
    /// 种子号的凭证请求都不是 `/s`（无文章骨架），跳过即返回 `None`。
    fn capture_skip_seed(
        &self,
        url: &str,
        cookie: Option<&str>,
        body: Option<&str>,
    ) -> Option<String> {
        if let Some(seed) = self.seed_biz.lock().expect("seed_biz mutex").clone() {
            let creds = extract_credentials(url, body, cookie);
            if has_session(&creds) && creds.biz.as_deref() == Some(seed.as_str()) {
                info!(biz = %seed, "跳过种子账号凭证（种子仅作接力入口，不采集）");
                return None;
            }
        }
        capture_request_with(&self.store, Some(&self.relay), url, cookie, body)
    }

    /// 删掉响应头里的 CSP（放行内联脚本）。
    fn relax_csp(parts: &mut ResponseParts) {
        let keys: Vec<String> = parts
            .headers
            .keys()
            .map(|k| k.as_str().to_string())
            .collect();
        for k in relay::relax_csp_headers(keys.iter().map(String::as_str)) {
            parts.headers.remove(&k);
        }
    }

    /// 处理一个 `/s` 文章页的**明文 HTML**（`handle_response` 解压读 body 之后的全部逻辑，
    /// 抽出便于脱代理单测）：抓公众号展示信息、短链解析入队、验证页识别、接力推进与注入。
    /// 返回要回给浏览器的 HTML（可能已注入）；`parts` 里的 CSP / content-length 会被相应删掉。
    pub fn relay_response(
        &self,
        last: &LastReq,
        parts: &mut ResponseParts,
        html: String,
    ) -> String {
        // content-length 因解压/注入而变，去掉让 hyper 重算。
        parts.headers.remove(header::CONTENT_LENGTH);
        // 经代理的 `/s` 文章页一律禁缓存（2026-09-08）：同一条链接会被反复打开（巡检每轮取同一号的最新一篇、
        // 集中续期重开同一批、换页重试），一旦 WebView 从磁盘缓存回放，代理就看不到这次请求——抓不到凭证、
        // 注不进接力脚本，只能等看门狗停滞超时。以前靠 RPA 拉起后 Ctrl+F5 硬刷兜这个坑，现在改在响应头上兜。
        parts.headers.insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        );

        // 四道闸之三（第四道"插在 body 尾部"在 inject_into_html 里）：接力开着、微信 UA、主文档。
        let inject_ok = self.relay_enabled && last.wechat_ua && last.is_document;

        // 固定入口页：不是任务文章 —— 不落号/文章、不 push、不 advance；只把 location.href
        // 注入成"本批第一条任务链接"（记在途、不标已打开），形成"入口 → 任务链接"的接力起点。
        // 种子入口页按 host 识别（见 [`Self::is_bootstrap`]）：本机种子服务地址，或用户配成局域网 IP 时经代理的那条路径。
        let is_bootstrap = self.is_bootstrap(last);
        if is_bootstrap {
            // 先记下种子账号 biz：其页面 JS 随后发的 getappmsgext（带 key）要在 handle_request
            // 里被 capture_skip_seed 识别并跳过，否则种子号会被误采、误报。
            self.note_seed_biz(&html);
            if !inject_ok || relay::already_injected(Some(&html)) {
                return html;
            }
            let Some(next_url) = self.relay.bootstrap_next() else {
                return html;
            };
            Self::relax_csp(parts);
            // 种子页 → 首条用单独的固定等待（与本机种子服务一致，无 300 下限），之后各篇才走随机区间。
            let script = relay::build_seed_script(Some(&next_url), self.seed_dwell_ms);
            info!(next = %next_url, "固定入口页 → 注入接力跳到本批首条任务链接");
            applog::info(
                Stage::Capture,
                format!(
                    "种子入口页已在微信内置浏览器打开，注入接力脚本跳转到首条任务链接（队列 {} 条）",
                    self.relay.pending_count()
                ),
            );
            return relay::inject_into_html(&html, &script);
        }

        // 人机验证页：既抓不到凭证也不该再往下跳（参考项目：命中验证码整批中止）。
        // 记下命中链接交给 orchestrator 看门狗收尾；不 advance、不注入。
        if article_md::is_verify_page(&html) {
            if inject_ok {
                self.relay.mark_verify(&last.url);
            }
            error!(url = %last.url, "命中人机验证页，接力中止");
            applog::error(
                Stage::Capture,
                "微信返回人机验证页（疑似被限流），接力中止，本批剩余链接记为未完成".to_string(),
            );
            return html;
        }

        // 从文章 HTML 提取 biz/mid/idx/sn（认 `var biz=` 与 `reportOpt:{biz:..}` 两种写法）
        // 与展示信息（名称/头像）。有 biz 就落号（短链页 URL 无 __biz，只能从 HTML 拿），
        // 保证公众号进列表；nickname/avatar 有则补上。
        let meta = parse_article_meta_from_html(Some(&html));
        if let Some(biz) = meta.get("biz") {
            let nickname = parse_account_name_from_html(Some(&html));
            let avatar = parse_account_avatar_from_html(Some(&html));
            match self
                .store
                .upsert_account(biz, nickname.as_deref(), avatar.as_deref())
            {
                Ok(_) => {
                    info!(biz = %biz, ?nickname, has_avatar = avatar.is_some(), "captured account profile")
                }
                Err(e) => error!(error = %e, "upsert_account(display) failed"),
            }
        }

        // 短链解析成签名长链：从 HTML 的 biz/mid(/idx/sn) 拼出规范 /s?__biz=..&sn=..，
        // 落一条文章骨架 + 入队接力。长链页会触发带 key 的凭证请求，从而抓到凭证、可采集该号。
        let is_short = last.path.starts_with("/s/");
        if is_short {
            if let Some(long_url) = long_url_from_meta(&meta) {
                if let (Some(biz), Some(mid)) = (meta.get("biz"), meta.get("mid")) {
                    let idx = meta
                        .get("idx")
                        .and_then(|s| s.parse::<i64>().ok())
                        .unwrap_or(1);
                    let fields = crate::model::ArticleFields {
                        sn: meta.get("sn").filter(|s| !s.is_empty()).cloned(),
                        content_url: Some(long_url.clone()),
                        ..Default::default()
                    };
                    let _ = self.store.upsert_article(biz, mid, idx, &fields);
                }
                if inject_ok && self.relay.push(&long_url) {
                    info!(long = %long_url, "短链解析为长链，入队接力抓凭证");
                    applog::info(
                        Stage::Capture,
                        format!(
                            "短链已解析为 {} 的文章长链，加入接力队列抓凭证",
                            meta.get("biz")
                                .map(|b| self.store.account_label(b))
                                .unwrap_or_else(|| "未知号".into())
                        ),
                    );
                }
            }
        }

        // 不注入（闸没过 / 队列空 / 已注入过）：仅回明文 HTML（账号信息已在上面抓好）。
        // 注意闸没过时**不 advance**：同机 Chrome 打开文章不能消耗队列。
        if !inject_ok || self.relay.pending_count() == 0 || relay::already_injected(Some(&html)) {
            return html;
        }

        // 接力：advance 把当前这条标记已打开，返回下一条未打开（含刚 push 的长链）并记在途。
        // 同时把本页解析到的 biz 登记到被标记的队列项上——回报只认队列各项解析到的号
        // （种子入口页不在队列、散页也不登记，故种子号天然被排除，不再误报）。
        let total_before = self.relay.rows().len();
        let next_url = self.relay.advance_with_biz(
            Some(&last.url),
            last.mid.as_deref(),
            meta.get("biz").map(String::as_str),
        );
        let pending_after = self.relay.pending_count();
        applog::info(
            Stage::Capture,
            format!(
                "已打开任务链接 {}/{}：{}{}",
                total_before.saturating_sub(pending_after),
                total_before,
                meta.get("biz")
                    .map(|b| self.store.account_label(b))
                    .unwrap_or_else(|| "未知号".into()),
                if next_url.is_some() {
                    "，接力跳转下一条"
                } else {
                    "，队列已清空"
                }
            ),
        );
        Self::relax_csp(parts);
        // 末条：人工模式把浏览器送回待命页（下一批由它自动接力），其它模式只打收尾标记。
        if next_url.is_none() && self.resident_home.is_some() {
            applog::info(
                Stage::Capture,
                "本批接力已到末条，微信内置浏览器将回到待命页等待下一批".to_string(),
            );
        }
        let script = relay::build_relay_script_home(
            next_url.as_deref(),
            self.resident_home.as_deref(),
            self.relay_dwell_ms,
            self.relay_dwell_max_ms,
        );
        debug!(mid = ?last.mid, next = ?next_url, "relay inject");
        relay::inject_into_html(&html, &script)
    }
}

fn host_of(req: &Request<Body>) -> String {
    if let Some(h) = req.uri().host() {
        return h.to_string();
    }
    req.headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(':').next().unwrap_or(s).trim().to_string())
        .unwrap_or_default()
}

fn build_url(host: &str, req: &Request<Body>) -> String {
    if req.uri().scheme().is_some() && req.uri().host().is_some() {
        return req.uri().to_string();
    }
    let pq = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    format!("https://{host}{pq}")
}

fn header_str(req: &Request<Body>, name: header::HeaderName) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// 请求追踪一行：路径（不含 query）+ 来源（微信内置浏览器 / 其它 UA）+ 主文档 / 子请求 + 是否带凭证参数。
/// 只写路径，URL 参数里有 `key` / `pass_ticket` / `uin`，一律不进日志。
fn trace_request_line(last: &LastReq, url: &str) -> String {
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    let has_key = query.split('&').any(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        k == "key" && !v.is_empty()
    });
    let has_biz = query
        .split('&')
        .any(|kv| kv.starts_with("__biz=") && kv.len() > 6);
    format!(
        "解密到微信请求：{}（{}，{}，{}{}）",
        last.path,
        if last.wechat_ua {
            "微信内置浏览器"
        } else {
            "非微信 UA"
        },
        if last.is_document {
            "主文档"
        } else {
            "子请求"
        },
        if has_key { "带 key" } else { "不带 key" },
        if has_biz { "、带 __biz" } else { "" },
    )
}

/// 响应正文摘录（调试）：折叠空白、去掉 `<style>` 块、截前 400 字；凡 16 位以上的连续 token（key /
/// pass_ticket / uin / 签名之类）一律打成 `***`，再过 [`applog::redact`]。只用于追踪日志。
fn trace_excerpt(text: &str) -> String {
    static STYLE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<style.*?</style>").unwrap());
    static WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
    static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9_%\-]{16,}").unwrap());
    let t = STYLE.replace_all(text, "");
    let t = WS.replace_all(&t, " ");
    let t = TOKEN.replace_all(&t, "***");
    let mut out: String = t.chars().take(400).collect();
    if t.chars().count() > 400 {
        out.push('…');
    }
    applog::redact(&out)
}

/// 请求追踪：`profile_ext` 响应概况。解压读 body 判断是否内嵌首屏列表（`var msgList`）/ 是否
/// 「请在微信客户端打开链接」验证页 / 是否 JSON（`getmsg`），再把明文 body 原样回填。
async fn trace_profile_response(last: &LastReq, res: Response<Body>) -> Response<Body> {
    let status = res.status().as_u16();
    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if res.status().is_redirection() {
        let loc = res
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(|l| l.split('?').next().unwrap_or(l).to_string())
            .unwrap_or_default();
        applog::info(
            Stage::Capture,
            format!("{} 响应：HTTP {status} 重定向 → {loc}", last.path),
        );
        return res;
    }
    let res = match decode_response(res) {
        Ok(r) => r,
        Err(e) => {
            applog::warn(
                Stage::Capture,
                format!("{} 响应：HTTP {status}，解压失败：{e}", last.path),
            );
            return Response::new(Body::empty());
        }
    };
    let (parts, body) = res.into_parts();
    let bytes = match body.collect().await {
        Ok(c) => c.to_bytes(),
        Err(e) => {
            applog::warn(
                Stage::Capture,
                format!("{} 响应：HTTP {status}，读取正文失败：{e}", last.path),
            );
            return Response::from_parts(parts, Body::empty());
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    let summary = if text.trim_start().starts_with('{') {
        let ret = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("ret").and_then(|r| r.as_i64()));
        match ret {
            Some(r) => format!("JSON，ret={r}"),
            None => "JSON".to_string(),
        }
    } else if text.contains("var msgList") || text.contains("msgList") {
        "HTML，内嵌首屏文章列表 msgList".to_string()
    } else if text.contains("请在微信客户端打开") {
        "HTML，验证页「请在微信客户端打开链接」（请求没带有效凭证）".to_string()
    } else {
        // 没见 msgList 的小页面（跳板 / 提示页）：附打码摘录，看它是跳转脚本、协议交接还是提示文案。
        format!("HTML，未见 msgList；摘录：{}", trace_excerpt(&text))
    };
    applog::info(
        Stage::Capture,
        format!(
            "{} 响应：HTTP {status}，{} 字节，{summary}（{}）",
            last.path,
            bytes.len(),
            content_type.split(';').next().unwrap_or("")
        ),
    );
    Response::from_parts(parts, Body::from(bytes.to_vec()))
}

impl HttpHandler for CaptureHandler {
    async fn handle_request(
        &mut self,
        _ctx: &HttpContext,
        req: Request<Body>,
    ) -> RequestOrResponse {
        // hudsucker 会先把 CONNECT 请求交给本钩子（隧道建立之前）。CONNECT 本身不携带
        // 凭证、也不是“解密内容”，必须原样放行（返回 Request）让 hudsucker 建隧道 / 决定
        // MITM 或直通；若在这里返回 Response 会短路隧道、弄断后续 TLS。
        if req.method() == Method::CONNECT {
            return req.into();
        }

        let host = host_of(&req);
        let path = req.uri().path().to_string();
        let url = build_url(&host, &req);

        // 记录“解密看到的请求”（证明 MITM 生效）。
        self.seen.lock().expect("seen mutex").push(url.clone());

        let user_agent = header_str(&req, header::USER_AGENT);
        let sec_fetch_dest = req
            .headers()
            .get("sec-fetch-dest")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        // 非目标 host：不动请求，仅登记关联信息后原样转发。
        if host != TARGET_HOST {
            let last = LastReq::from_parts(
                &host,
                &path,
                &url,
                None,
                user_agent.as_deref(),
                sec_fetch_dest.as_deref(),
            );
            // 心跳：**固定入口页**（种子独立域名的主文档）的加载也计心跳——它证明"点种子拉起了浏览器"，
            // 让 orchestrator 看门狗的"点空"计时在种子页一到就复位（与旧微信文章种子加载即 /s 心跳
            // 的时序一致）；其后到首篇文章的停留间隙由 stall 判据覆盖。
            if self.relay_enabled && last.wechat_ua && last.is_document && self.is_bootstrap(&last)
            {
                self.relay.touch();
            }
            // 请求追踪：非微信域名也记 host + 路径（看跳板页跳去了哪里；只记路径不记参数）。
            if self.trace_requests {
                applog::info(
                    Stage::Capture,
                    format!(
                        "解密到其它域名请求：{}{}（{}，{}）",
                        host,
                        last.path,
                        if last.wechat_ua {
                            "微信内置浏览器"
                        } else {
                            "非微信 UA"
                        },
                        if last.is_document {
                            "主文档"
                        } else {
                            "子请求"
                        }
                    ),
                );
            }
            self.last = Some(last);
            return req.into();
        }

        let cookie = header_str(&req, header::COOKIE);
        let content_type = header_str(&req, header::CONTENT_TYPE)
            .unwrap_or_default()
            .to_lowercase();
        let is_form = content_type.contains("form-urlencoded");

        // 仅对目标 host 的表单 POST 缓冲 body（其它请求不消费 body，转发保持原样）。
        let (req, body_text): (Request<Body>, Option<String>) = if is_form {
            let (parts, body) = req.into_parts();
            match body.collect().await {
                Ok(collected) => {
                    let bytes = collected.to_bytes();
                    let text = String::from_utf8_lossy(&bytes).to_string();
                    (
                        Request::from_parts(parts, Body::from(bytes.to_vec())),
                        Some(text),
                    )
                }
                Err(e) => {
                    error!(error = %e, "collect form body failed");
                    (Request::from_parts(parts, Body::empty()), None)
                }
            }
        } else {
            (req, None)
        };

        // 抓凭证 + 记文章骨架（核心逻辑抽成 capture_request，便于脱代理单测）。
        // 但跳过种子账号自身的凭证——种子页仅作接力入口，不作采集目标（见 capture_skip_seed）。
        let mid = self.capture_skip_seed(&url, cookie.as_deref(), body_text.as_deref());

        let last = LastReq::from_parts(
            &host,
            &path,
            &url,
            mid,
            user_agent.as_deref(),
            sec_fetch_dest.as_deref(),
        );
        if self.trace_requests {
            applog::info(Stage::Capture, trace_request_line(&last, &url));
        }
        // 心跳：微信 WebView 的主文档 `/s` 请求（orchestrator 看门狗据此判断点空 / 停滞）。
        if self.relay_enabled && last.is_article() && last.wechat_ua && last.is_document {
            self.relay.touch();
        }
        self.last = Some(last);
        req.into()
    }

    async fn handle_response(&mut self, _ctx: &HttpContext, res: Response<Body>) -> Response<Body> {
        // 关联到发起这次响应的请求（同一 handler clone 上 handle_request 刚记下的）。
        let Some(last) = self.last.take() else {
            return res;
        };
        // 请求追踪（调试）：`profile_ext`（公众号主页 / 列表接口）的响应概况——状态码、大小、是否内嵌
        // 首屏列表 `msgList`、是否验证页。只在追踪开着时读 body（解压后原样回填，页面不受影响）。
        if self.trace_requests && last.path.starts_with("/mp/profile_ext") {
            return trace_profile_response(&last, res).await;
        }
        // 处理 mp.weixin 的 /s 文章页，或**固定入口页**（种子独立域名的主文档，需注入接力起跳脚本）。
        let is_boot = self.is_bootstrap(&last);
        if !last.is_article() && !is_boot {
            return res;
        }
        // 历史种子（微信文章短链）302 跳长链（`/s/<hash>` → `/s?__biz=..`）：其响应非 HTML，走不到下面
        // relay_response 的 bootstrap 分支记不下种子 biz。此处从 3xx 的 Location（带 __biz=）取，
        // 供 capture_skip_seed 跳过种子号凭证。种子已改独立域名后此路径不再触发（种子非微信 host、
        // 直接返回 200 HTML），仅为兼容历史/测试保留。
        if is_boot && res.status().is_redirection() {
            if let Some(loc) = res
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
            {
                self.note_seed_biz_from_url(loc);
            }
        }
        let is_html = res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_lowercase().contains("html"))
            .unwrap_or(false);
        if !is_html {
            return res;
        }

        // 解压响应（wechat /s 常为 gzip）拿明文 HTML：抓公众号展示信息（名称/头像）+
        // （可选）接力注入都要用。对齐 Python response 钩子：每个 /s 文章页都读一次 HTML。
        let res = match decode_response(res) {
            Ok(r) => r,
            Err(e) => {
                error!(error = %e, "decode_response failed");
                return Response::new(Body::empty());
            }
        };
        // 先读 body 拿明文 HTML —— 短链 /s/<hash> 的请求 URL 无 __biz，必须从 HTML 里
        // 解析出 biz/mid/idx/sn，据此拼长链并入队接力（故 advance 在读 body 之后再算 next）。
        let (mut parts, body) = res.into_parts();
        let html = match body.collect().await {
            Ok(c) => String::from_utf8_lossy(&c.to_bytes()).to_string(),
            Err(e) => {
                error!(error = %e, "collect response body failed");
                return Response::from_parts(parts, Body::empty());
            }
        };
        let out = self.relay_response(&last, &mut parts, html);
        Response::from_parts(parts, Body::from(out))
    }

    /// ⭐ TLS 直通判定：返回 `false` 时 hudsucker 裸 TCP 直通该连接（不 MITM）。
    fn should_intercept_tls(
        &mut self,
        _ctx: &HttpContext,
        client_hello: ClientHello<'_>,
    ) -> impl std::future::Future<Output = bool> + Send {
        // 先在 async 块外用 SNI 算好决策，避免把借用 client_hello 的引用带过 await。
        let passthrough = self.decider.should_passthrough(client_hello.server_name());
        async move { !passthrough }
    }
}

/// 启动 MITM 代理所需的配置。
pub struct CaptureConfig {
    pub ca: CaMaterial,
    pub store: Arc<Store>,
    pub relay_enabled: bool,
    /// 每条链接停留下限（毫秒）。
    pub relay_dwell_ms: i64,
    /// 停留上限（毫秒）；`<= relay_dwell_ms` 即固定停留。每跳在区间内随机，避免固定节拍。
    pub relay_dwell_max_ms: i64,
    /// 种子入口页 → 本批首条的固定等待（毫秒）。只在 `is_bootstrap` 兜底分支（种子 host 是局域网 IP、
    /// 请求经代理）用到；本机种子服务那条路径由 [`crate::seedserver::SeedServerConfig::seed_dwell_ms`] 负责。
    pub seed_dwell_ms: i64,
    pub decider: PassthroughDecider,
    /// 进程内接力队列（与 orchestrator 共享同一实例；orchestrator 起代理前已写入本批链接）。
    pub relay: RelayQueue,
    /// 固定入口种子链接（真机传本机种子服务地址 [`crate::seedserver::seed_url`]）。
    /// 经代理命中其页面即注入接力跳到本批首条任务链接（本机服务已注入的
    /// 页面不会二次注入）；None = 无固定入口（老流程直接开任务链接 / 测试）。
    pub bootstrap_seed: Option<String>,
    /// 人工模式的待命页地址（种子链接）；`None` = 末条不回落。见 [`CaptureHandler::with_resident_home`]。
    pub resident_home: Option<String>,
    /// 请求追踪（调试，默认关）：见 [`CaptureHandler::with_trace`]。
    pub trace_requests: bool,
}

/// 运行中的 MITM 代理句柄。`shutdown` 触发优雅关闭。
pub struct CaptureProxy {
    /// 代理实际监听地址（用 127.0.0.1:0 时是系统分配的端口）。
    pub addr: SocketAddr,
    handler: CaptureHandler,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<()>,
}

impl CaptureProxy {
    /// 本代理迄今解密看到的请求 URL（观测/测试）。
    pub fn seen_urls(&self) -> Vec<String> {
        self.handler.seen_urls()
    }

    /// 优雅关闭代理并等待任务结束。
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }
}

/// 确保进程级 rustls 默认 crypto provider 已安装（幂等；已安装则忽略）。
pub fn install_default_crypto() {
    let _ = aws_lc_rs::default_provider().install_default();
}

/// 在 `127.0.0.1:0`（或指定端口）起一个 MITM 代理任务，返回句柄（含实际端口）。
pub async fn start(cfg: CaptureConfig) -> Result<CaptureProxy> {
    start_on(cfg, "127.0.0.1:0").await
}

/// 在指定地址起 MITM 代理。
pub async fn start_on(cfg: CaptureConfig, bind: &str) -> Result<CaptureProxy> {
    install_default_crypto();

    let listener = tokio::net::TcpListener::bind(bind).await?;
    let addr = listener.local_addr()?;

    let authority: RcgenAuthority = cfg.ca.authority()?;
    let handler = CaptureHandler::new(
        cfg.store,
        cfg.decider,
        cfg.relay_enabled,
        cfg.relay_dwell_ms,
        cfg.relay_dwell_max_ms,
        cfg.seed_dwell_ms,
        cfg.relay,
        cfg.bootstrap_seed,
    )
    .with_trace(cfg.trace_requests)
    .with_resident_home(cfg.resident_home);
    let handler_for_handle = handler.clone();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let proxy = Proxy::builder()
        .with_listener(listener)
        .with_ca(authority)
        .with_rustls_connector(aws_lc_rs::default_provider())
        .with_http_handler(handler)
        .with_graceful_shutdown(async move {
            let _ = rx.await;
        })
        .build()?;

    let join = tokio::spawn(async move {
        if let Err(e) = proxy.start().await {
            error!(error = %e, "mitm proxy exited with error");
        }
    });

    Ok(CaptureProxy {
        addr,
        handler: handler_for_handle,
        shutdown_tx: Some(tx),
        join,
    })
}

#[cfg(test)]
mod tests {
    /// 单测用的独立域名种子入口（非微信域名）。
    const SEED_BOOTSTRAP_URL: &str = "https://seed.example";

    #[test]
    fn test_trace_excerpt_masks_long_tokens_and_collapses() {
        let html = "<html><style>body{}</style><script>\n  location.href=\"https://x.qq.com/a?key=abcdef0123456789abcdef&uin=MTIz\";\n</script></html>";
        let out = super::trace_excerpt(html);
        assert!(!out.contains("abcdef0123456789abcdef"), "{out}");
        assert!(out.contains("location.href"), "{out}");
        assert!(!out.contains("<style>"), "{out}");
        assert!(!out.contains('\n'), "{out}");
        // uin 是短 token，靠 redact 打码
        assert!(out.contains("uin=***"), "{out}");
    }

    use super::*;
    use std::sync::{Arc, Mutex};

    use hudsucker::hyper::StatusCode;
    use hudsucker::rcgen::{
        BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose,
    };
    use hudsucker::rustls::crypto::aws_lc_rs;
    use hudsucker::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use hudsucker::rustls::ServerConfig;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    use crate::ca::generate_ca;
    use crate::store::Store;

    #[test]
    fn test_passthrough_decider() {
        let d = PassthroughDecider::new();
        // 无 SNI / 空 SNI 直通
        assert!(d.should_passthrough(None));
        assert!(d.should_passthrough(Some("")));
        // 固定证书主机直通
        assert!(d.should_passthrough(Some("long.weixin.qq.com")));
        assert!(d.should_passthrough(Some("mmtls.weixin.qq.com")));
        // 目标 host 绝不直通（要解密）
        assert!(!d.should_passthrough(Some("mp.weixin.qq.com")));
        // 普通主机默认不直通（会被 MITM）
        assert!(!d.should_passthrough(Some("example.com")));
        // 运行期学习后直通
        d.learn("pinned.example.com");
        assert!(d.should_passthrough(Some("pinned.example.com")));
    }

    #[test]
    fn test_capture_request_writes_store() {
        let store = Store::open_in_memory().unwrap();
        // 带真实会话材料的请求（cookie < query < body 合并）
        let url = "https://mp.weixin.qq.com/mp/getappmsgext?__biz=BIZ==&x5=0";
        let mid = capture_request(
            &store,
            url,
            Some("uin=U; key=K"),
            Some("appmsg_token=AT&pass_ticket=PT"),
        );
        assert!(mid.is_none()); // 非 /s，不记文章
        let cred = store.get_credential("BIZ==").unwrap().unwrap();
        assert_eq!(cred.key.as_deref(), Some("K"));
        assert_eq!(cred.appmsg_token.as_deref(), Some("AT"));
        assert_eq!(cred.pass_ticket.as_deref(), Some("PT"));
        assert_eq!(cred.uin.as_deref(), Some("U"));

        // /s? 文章骨架
        let s_url = "https://mp.weixin.qq.com/s?__biz=BIZ==&mid=2650&idx=2&sn=abc";
        let mid2 = capture_request(&store, s_url, None, None);
        assert_eq!(mid2.as_deref(), Some("2650"));
        let arts = store.list_articles(Some("BIZ=="), 10, 0, false).unwrap();
        assert_eq!(arts.len(), 1);
        assert_eq!(arts[0].mid.as_deref(), Some("2650"));
        assert_eq!(arts[0].idx, Some(2));

        // 占位请求（无会话材料）不写凭证
        let store2 = Store::open_in_memory().unwrap();
        capture_request(
            &store2,
            "https://mp.weixin.qq.com/mp/x?__biz=B2==&uin=&key=&wxtoken=777",
            None,
            None,
        );
        assert!(store2.get_credential("B2==").unwrap().is_none());
    }

    /// 种子账号的凭证必须被跳过：种子页仅作接力入口，其自身 getappmsgext 不得入库/被采。
    /// 复现实测问题：本批目标是另一个号，结果上报的却是种子链接所属的公众号。
    #[test]
    fn test_capture_skips_seed_account_credential() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let handler = CaptureHandler::new(
            store.clone(),
            PassthroughDecider::new(),
            true,
            2500,
            3500,
            1000,
            RelayQueue::new(),
            Some(SEED_BOOTSTRAP_URL.to_string()),
        );

        // 模拟种子入口页 HTML 被解析：记下种子账号 biz。
        handler.note_seed_biz(r#"<script>var biz = "SEEDBIZ==";</script>"#);

        // 种子号自身带 key 的凭证请求 —— 应被跳过，不入库。
        let seed_cred =
            "https://mp.weixin.qq.com/mp/getappmsgext?__biz=SEEDBIZ==&uin=U&key=K&pass_ticket=P";
        assert!(handler.capture_skip_seed(seed_cred, None, None).is_none());
        assert!(
            store.get_credential("SEEDBIZ==").unwrap().is_none(),
            "种子号凭证不应入库（种子仅作接力入口）"
        );

        // 任务号的凭证 —— 应正常抓取入库。
        let task_cred =
            "https://mp.weixin.qq.com/mp/getappmsgext?__biz=TASKBIZ==&uin=U&key=K2&pass_ticket=P";
        handler.capture_skip_seed(task_cred, None, None);
        assert!(
            store.get_credential("TASKBIZ==").unwrap().is_some(),
            "任务号凭证应正常入库"
        );
    }

    /// 种子短链 302 跳长链：从 Location 的 __biz 记种子 biz，其凭证同样被跳过。
    /// 复现 WEB-9：种子 /s/<hash> 302 到 /s?__biz=..，走不到 HTML 解析，靠 Location 兜底。
    #[test]
    fn test_seed_biz_from_redirect_location_skips_credential() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let handler = CaptureHandler::new(
            store.clone(),
            PassthroughDecider::new(),
            true,
            2500,
            3500,
            1000,
            RelayQueue::new(),
            Some(SEED_BOOTSTRAP_URL.to_string()),
        );

        // 模拟种子短链 302 的 Location（相对 URL 也能取 __biz）。
        handler.note_seed_biz_from_url("/s?__biz=SEEDBIZ==&mid=2247483&idx=1&sn=abcdef");

        let seed_cred =
            "https://mp.weixin.qq.com/mp/getappmsgext?__biz=SEEDBIZ==&uin=U&key=K&pass_ticket=P";
        assert!(handler.capture_skip_seed(seed_cred, None, None).is_none());
        assert!(
            store.get_credential("SEEDBIZ==").unwrap().is_none(),
            "302 落地的种子号凭证也应被跳过"
        );
    }

    // -- 接力响应处理（脱代理）：四道闸 / 入口页 / 验证页 / 在途 --

    const WX_UA: &str =
        "Mozilla/5.0 (Windows NT 10.0; WOW64) AppleWebKit/537.36 Chrome/107 MicroMessenger/7.0.20";
    const ARTICLE_HTML: &str = "<html><body>var biz=\"AAA==\";<p>正文</p></body></html>";

    fn handler_with(relay: &RelayQueue, bootstrap: Option<&str>) -> CaptureHandler {
        CaptureHandler::new(
            Arc::new(Store::open_in_memory().unwrap()),
            PassthroughDecider::new(),
            true,
            2000,
            3000,
            1000,
            relay.clone(),
            bootstrap.map(str::to_string),
        )
    }

    fn parts_html() -> ResponseParts {
        let (parts, _) = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CONTENT_LENGTH, "123")
            .header("content-security-policy", "script-src 'none'")
            .body(())
            .unwrap()
            .into_parts();
        parts
    }

    fn article_req(url: &str, ua: Option<&str>, dest: Option<&str>) -> LastReq {
        let path = url_path(url).unwrap();
        let mid = parse_s_url(url).map(|a| a.mid);
        LastReq::from_parts(TARGET_HOST, &path, url, mid, ua, dest)
    }

    /// 固定入口页请求（种子独立域名，非微信 host）：host/path 取自 `url`。
    fn seed_req(url: &str, ua: Option<&str>, dest: Option<&str>) -> LastReq {
        let host = url_host(url).unwrap();
        let path = url_path(url).unwrap();
        LastReq::from_parts(&host, &path, url, None, ua, dest)
    }

    #[test]
    fn test_relay_response_injects_and_advances_with_wechat_ua() {
        let relay = RelayQueue::new();
        let u0 = "https://mp.weixin.qq.com/s?__biz=AAA==&mid=1&idx=1&sn=a";
        let u1 = "https://mp.weixin.qq.com/s?__biz=AAA==&mid=2&idx=1&sn=b";
        relay.set(&[u0.to_string(), u1.to_string()]);
        let h = handler_with(&relay, None);
        let mut parts = parts_html();
        let out = h.relay_response(
            &article_req(u0, Some(WX_UA), Some("document")),
            &mut parts,
            ARTICLE_HTML.into(),
        );
        assert!(
            out.contains(relay::MARKER) && out.contains("mid=2"),
            "应注入跳 u1: {out}"
        );
        assert!(
            out.find(relay::MARKER).unwrap() < out.find("</body>").unwrap(),
            "脚本在 body 尾部"
        );
        assert!(out.contains("Math.random()"), "随机停留");
        assert!(
            parts.headers.get("content-security-policy").is_none(),
            "CSP 已摘"
        );
        assert!(parts.headers.get(header::CONTENT_LENGTH).is_none());
        assert_eq!(
            parts
                .headers
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store"),
            "经代理的 /s 文章页一律 no-store，重开同一链接时才走网络"
        );
        assert_eq!(relay.pending_count(), 1);
        assert_eq!(relay.inflight().as_deref(), Some(u1));
        // 末条：注入收尾脚本，不再跳
        let out = h.relay_response(
            &article_req(u1, Some(WX_UA), Some("document")),
            &mut parts_html(),
            ARTICLE_HTML.into(),
        );
        assert!(out.contains("__mpider_relay_done"));
        assert_eq!(relay.pending_count(), 0);
        assert!(relay.inflight().is_none());
    }

    #[test]
    fn test_relay_response_gates_ua_and_iframe() {
        let relay = RelayQueue::new();
        let u0 = "https://mp.weixin.qq.com/s?__biz=AAA==&mid=1&idx=1&sn=a";
        relay.set(&[u0.to_string()]);
        let h = handler_with(&relay, None);
        // 同机 Chrome（无 MicroMessenger）：不注入、不消耗队列、CSP 原样
        let mut parts = parts_html();
        let out = h.relay_response(
            &article_req(u0, Some("Mozilla/5.0 Chrome/120"), Some("document")),
            &mut parts,
            ARTICLE_HTML.into(),
        );
        assert!(!out.contains(relay::MARKER));
        assert!(parts.headers.get("content-security-policy").is_some());
        assert_eq!(relay.pending_count(), 1);
        // 微信 UA 但是 iframe：不注入
        let out = h.relay_response(
            &article_req(u0, Some(WX_UA), Some("iframe")),
            &mut parts_html(),
            ARTICLE_HTML.into(),
        );
        assert!(!out.contains(relay::MARKER));
        assert_eq!(relay.pending_count(), 1);
        // 没带 Sec-Fetch-Dest 的老内核：视为主文档，正常注入
        let out = h.relay_response(
            &article_req(u0, Some(WX_UA), None),
            &mut parts_html(),
            ARTICLE_HTML.into(),
        );
        assert!(out.contains(relay::MARKER));
        assert_eq!(relay.pending_count(), 0);
    }

    #[test]
    fn test_relay_response_bootstrap_page_jumps_to_first_pending() {
        let relay = RelayQueue::new();
        let u0 = "https://mp.weixin.qq.com/s?__biz=AAA==&mid=1&idx=1&sn=a";
        relay.set(&[u0.to_string()]);
        let h = handler_with(&relay, Some(SEED_BOOTSTRAP_URL));
        // 种子入口页（独立域名，非微信文章）：其 HTML 不含微信 biz、也不该落号。
        let seed_html = "<html><body><p>seed</p></body></html>";
        let req = seed_req(SEED_BOOTSTRAP_URL, Some(WX_UA), Some("document"));
        let store_before = h.store.list_accounts().unwrap().len();
        let out = h.relay_response(&req, &mut parts_html(), seed_html.into());
        assert!(out.contains("mid=1"), "入口页应跳本批首条");
        assert!(out.contains(relay::MARKER));
        // 入口页：不标已打开、只记在途；不落号
        assert_eq!(relay.pending_count(), 1);
        assert_eq!(relay.inflight().as_deref(), Some(u0));
        assert_eq!(h.store.list_accounts().unwrap().len(), store_before);
        // 入口页被同机 Chrome 打开：不注入、不记在途
        let relay2 = RelayQueue::new();
        relay2.set(&[u0.to_string()]);
        let h2 = handler_with(&relay2, Some(SEED_BOOTSTRAP_URL));
        let out = h2.relay_response(
            &seed_req(SEED_BOOTSTRAP_URL, Some("Chrome"), Some("document")),
            &mut parts_html(),
            seed_html.into(),
        );
        assert!(!out.contains(relay::MARKER));
        assert!(relay2.inflight().is_none());
    }

    #[test]
    fn test_is_bootstrap_matches_seed_host_not_other_hosts() {
        let relay = RelayQueue::new();
        // 独立域名种子：按 host 识别，任意 path 的主文档都是入口。
        let h = handler_with(&relay, Some(SEED_BOOTSTRAP_URL));
        assert!(h.is_bootstrap(&seed_req(SEED_BOOTSTRAP_URL, None, None)));
        assert!(h.is_bootstrap(&seed_req(
            &format!("{SEED_BOOTSTRAP_URL}/x?y=1"),
            None,
            None
        )));
        // 别的普通网站：不是入口。
        assert!(!h.is_bootstrap(&seed_req("https://example.com/", None, None)));
        // 微信文章页也不是入口（种子已不在微信域名上）。
        assert!(!h.is_bootstrap(&article_req(
            "https://mp.weixin.qq.com/s?__biz=A&mid=1&idx=1",
            None,
            None
        )));
        // 历史配置：种子恰是微信文章短链时，仍按 host+path 精确匹配，普通文章不误判为入口。
        let seed = "https://mp.weixin.qq.com/s/HASH";
        let h2 = handler_with(&relay, Some(seed));
        assert!(h2.is_bootstrap(&article_req(seed, None, None)));
        assert!(!h2.is_bootstrap(&article_req(
            "https://mp.weixin.qq.com/s?__biz=A&mid=1&idx=1",
            None,
            None
        )));
    }

    #[test]
    fn test_relay_response_verify_page_stops_relay() {
        let relay = RelayQueue::new();
        let u0 = "https://mp.weixin.qq.com/s?__biz=AAA==&mid=1&idx=1&sn=a";
        let u1 = "https://mp.weixin.qq.com/s?__biz=AAA==&mid=2&idx=1&sn=b";
        relay.set(&[u0.to_string(), u1.to_string()]);
        let h = handler_with(&relay, None);
        let verify = "<html><head><script src=\"https://mp.weixin.qq.com/mp/secitptpage/template/verify.js\"></script></head><body>请完成验证</body></html>";
        let out = h.relay_response(
            &article_req(u0, Some(WX_UA), Some("document")),
            &mut parts_html(),
            verify.into(),
        );
        assert!(!out.contains(relay::MARKER), "验证页不注入");
        assert_eq!(relay.verify_hit().as_deref(), Some(u0));
        assert_eq!(relay.pending_count(), 2, "不 advance");
    }

    #[test]
    fn test_relay_response_short_link_pushes_long_url() {
        let relay = RelayQueue::new();
        let short = "https://mp.weixin.qq.com/s/XBsCJgA6ZlMtLA1H6rPHlA";
        relay.set(&[short.to_string()]);
        let h = handler_with(&relay, None);
        let html = "<html><body><script>var biz=\"MzShort==\";var mid=\"100\";var idx=\"1\";var sn=\"abc\";</script></body></html>";
        let out = h.relay_response(
            &article_req(short, Some(WX_UA), Some("document")),
            &mut parts_html(),
            html.into(),
        );
        // 解析出长链入队，短链本身标已打开，下一跳是长链
        assert!(
            out.contains("__biz=MzShort") && out.contains("mid=100"),
            "{out}"
        );
        assert_eq!(relay.pending_count(), 1);
        assert!(h
            .store
            .list_accounts()
            .unwrap()
            .iter()
            .any(|a| a.biz == "MzShort=="));
    }

    // -- 本地 TLS origin：证明按 SNI 直通时裸 TCP 隧道确实透传了 origin 自己的证书 --

    /// 生成 origin 的 rustls ServerConfig（自建 CA + 叶证书）+ 返回该 CA 的 PEM（供 reqwest 信任）。
    fn make_origin_tls(hostnames: &[&str]) -> (Arc<ServerConfig>, String) {
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "mpider origin test CA");
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();
        let ca_pem = ca_cert.pem();
        let issuer = Issuer::new(ca_params, ca_key);

        let leaf_key = KeyPair::generate().unwrap();
        let mut leaf_params =
            CertificateParams::new(hostnames.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .unwrap();
        leaf_params
            .distinguished_name
            .push(DnType::CommonName, hostnames[0]);
        let leaf_cert = leaf_params.signed_by(&leaf_key, &issuer).unwrap();

        let cert_der: CertificateDer<'static> = leaf_cert.der().clone();
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
        let config = ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)
            .unwrap();
        (Arc::new(config), ca_pem)
    }

    /// 起一个本地 TLS origin，返回 (监听地址, origin CA 的 PEM)。
    async fn spawn_tls_origin(body: &'static str, hostnames: &[&str]) -> (SocketAddr, String) {
        let (config, ca_pem) = make_origin_tls(hostnames);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = TlsAcceptor::from(config);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    if let Ok(mut tls) = acceptor.accept(stream).await {
                        let mut buf = [0u8; 2048];
                        let _ = tls.read(&mut buf).await; // 读掉请求头（best-effort）
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = tls.write_all(resp.as_bytes()).await;
                        let _ = tls.flush().await;
                        let _ = tls.shutdown().await;
                    }
                });
            }
        });
        (addr, ca_pem)
    }

    /// 只返回固定响应、并记录“解密看到的请求 URL”的处理器 —— 证明 MITM 机制成立。
    /// 复用与真实 CaptureProxy 完全相同的 CA / 授权体 / rustls 握手路径，只把上游转发换成
    /// 本地固定响应，使 MITM 解密可脱网、确定性验证（不需要真微信）。
    #[derive(Clone)]
    struct CannedHandler {
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl HttpHandler for CannedHandler {
        async fn handle_request(
            &mut self,
            _ctx: &HttpContext,
            req: Request<Body>,
        ) -> RequestOrResponse {
            // CONNECT 必须放行以建立隧道；只对解密后的真实请求返回固定响应。
            if req.method() == Method::CONNECT {
                return req.into();
            }
            let host = host_of(&req);
            let url = build_url(&host, &req);
            self.seen.lock().unwrap().push(url);
            RequestOrResponse::Response(
                Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from("ok-mitm"))
                    .expect("build response"),
            )
        }
        // should_intercept_tls 用默认实现（true）：对所有 SNI 都 MITM 解密。
    }

    /// ⭐ 证明 MITM 解密成立：reqwest 信任生成的 CA，经代理请求一个 HTTPS，
    /// 断言 200 且处理器确实在明文层拦截到了该请求。
    #[tokio::test]
    async fn test_mitm_decrypts_https_through_proxy() {
        install_default_crypto();
        let ca = generate_ca().unwrap();
        let authority = ca.authority().unwrap();

        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler = CannedHandler { seen: seen.clone() };

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let proxy = Proxy::builder()
            .with_listener(listener)
            .with_ca(authority)
            .with_rustls_connector(aws_lc_rs::default_provider())
            .with_http_handler(handler)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .build()
            .unwrap();
        let jh = tokio::spawn(async move {
            let _ = proxy.start().await;
        });

        let client = reqwest::Client::builder()
            .add_root_certificate(reqwest::Certificate::from_pem(ca.cert_pem.as_bytes()).unwrap())
            .proxy(reqwest::Proxy::all(format!("http://{proxy_addr}")).unwrap())
            .build()
            .unwrap();

        // 目标故意用 mp.weixin.qq.com —— 无需真微信，代理会为该 SNI 现签叶证书并解密。
        let resp = client
            .get("https://mp.weixin.qq.com/s?__biz=MzTEST==&mid=100&idx=1&sn=s")
            .send()
            .await
            .expect("request via mitm proxy");
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "ok-mitm");

        let seen = seen.lock().unwrap().clone();
        assert!(
            seen.iter().any(|u| u.contains("mp.weixin.qq.com")),
            "MITM 应解密并在明文层看到该请求; seen={seen:?}"
        );

        let _ = tx.send(());
        let _ = jh.await;
    }

    /// ⭐ 证明按 SNI 直通成立：对“学习到的固定证书主机”，真实 CaptureProxy 走 TLS 直通
    /// （裸 TCP 隧道），reqwest 与 origin 直接完成 TLS（看到的是 origin 自己的证书，而非
    /// 代理现签的叶证书），处理器**没有**在明文层看到该请求。
    #[tokio::test]
    async fn test_passthrough_tunnels_learned_sni() {
        let (origin_addr, origin_ca_pem) = spawn_tls_origin("hello-origin", &["localhost"]).await;

        let ca = generate_ca().unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        let decider = PassthroughDecider::new();
        decider.learn("localhost"); // 把 localhost 当作“固定证书主机”，应直通

        let proxy = start(CaptureConfig {
            ca,
            store,
            relay_enabled: false,
            relay_dwell_ms: 2500,
            relay_dwell_max_ms: 2500,
            seed_dwell_ms: 1000,
            decider,
            relay: RelayQueue::new(),
            bootstrap_seed: None,
            trace_requests: false,
            resident_home: None,
        })
        .await
        .unwrap();

        // reqwest 只信任 origin 的 CA（不信任代理 CA）；若被 MITM，握手会因证书不受信而失败。
        let client = reqwest::Client::builder()
            .add_root_certificate(reqwest::Certificate::from_pem(origin_ca_pem.as_bytes()).unwrap())
            .proxy(reqwest::Proxy::all(format!("http://{}", proxy.addr)).unwrap())
            .build()
            .unwrap();

        let url = format!("https://localhost:{}/", origin_addr.port());
        let resp = client
            .get(&url)
            .send()
            .await
            .expect("request via passthrough");
        assert_eq!(resp.status(), 200);
        assert!(resp.text().await.unwrap().contains("hello-origin"));

        // 直通证据：MITM 被跳过，处理器没有解密看到这条请求。
        assert!(
            proxy.seen_urls().is_empty(),
            "直通应跳过 MITM；但处理器看到了: {:?}",
            proxy.seen_urls()
        );

        proxy.shutdown().await;
    }

    /// 联网自证（需外网，默认忽略）：真实 `CaptureProxy`（含上游 webpki 连接器）对公网
    /// HTTPS 做 MITM + 转发。运行：`cargo test -p mpider-core -- --ignored real_internet`。
    #[tokio::test]
    #[ignore = "需要外网访问 example.com"]
    async fn test_mitm_forwards_real_internet() {
        let ca = generate_ca().unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        let proxy = start(CaptureConfig {
            ca: ca.clone(),
            store,
            relay_enabled: false,
            relay_dwell_ms: 2500,
            relay_dwell_max_ms: 2500,
            seed_dwell_ms: 1000,
            decider: PassthroughDecider::new(),
            relay: RelayQueue::new(),
            bootstrap_seed: None,
            trace_requests: false,
            resident_home: None,
        })
        .await
        .unwrap();

        let client = reqwest::Client::builder()
            .add_root_certificate(reqwest::Certificate::from_pem(ca.cert_pem.as_bytes()).unwrap())
            .proxy(reqwest::Proxy::all(format!("http://{}", proxy.addr)).unwrap())
            .build()
            .unwrap();

        let resp = client.get("https://example.com/").send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let body = resp.text().await.unwrap();
        assert!(body.to_lowercase().contains("example domain"));
        assert!(
            proxy.seen_urls().iter().any(|u| u.contains("example.com")),
            "MITM 应解密并转发该请求; seen={:?}",
            proxy.seen_urls()
        );
        proxy.shutdown().await;
    }
}
