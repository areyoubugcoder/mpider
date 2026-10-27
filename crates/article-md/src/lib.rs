//! 微信公众号文章 HTML → Markdown 独立解析库。
//!
//! 移植自 Python 开源项目 `wechat-article-parser` 0.0.6（PyPI，MIT 许可；归属声明见仓库根目录
//! `THIRD-PARTY-NOTICES.md`）：
//! 输入 `(url, html)`，输出结构化 [`ArticleResult`]（元数据 + 正文 Markdown + 图片列表）。
//! **纯解析、无网络**：抓取 / 并发 / 节流 / 入库都由调用方（mpider-core `detail` 模块）负责，
//! 因此本库可独立复用（CLI / 服务端 / 其他项目）。
//!
//! 支持的版式（与 Python 版逐一对齐，按探测顺序）：
//! 1. 富文本 `div.rich_media_content`（绝大多数图文）
//! 2. 转载页 `div.original_page`
//! 3. 纯文本 `p#js_text_desc`
//! 4. 视频分享 `div#js_common_share_desc_wrap`
//! 5. 图片轮播 `div.share_media_swiper_content`（小红书风格）
//! 6. 全屏布局 `div#js_fullscreen_layout_padding`（appmsg_type 10002）
//!
//! 微信返回人机验证页时返回 [`ParseError::VerifyPage`]（调用方据此停止本批并提示限流）。

mod content;
mod meta;
mod text;

use scraper::{Html, Selector};

/// 文章解析结果（字段语义对齐 Python `ArticleResult`；数值 id 统一用字符串存储层友好形态）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArticleResult {
    /// 公众号 __biz（base64 形态，`mp_id_b64`）。
    pub biz: Option<String>,
    /// 公众号名称。
    pub mp_name: Option<String>,
    /// 公众号微信号（alias）。
    pub mp_alias: Option<String>,
    /// 文章短 id（URL /s/ 后 22 位）。
    pub article_id: Option<String>,
    pub mid: Option<String>,
    pub idx: Option<i64>,
    pub sn: Option<String>,
    pub title: Option<String>,
    /// 封面图（og:image）。
    pub cover: Option<String>,
    /// 摘要（og:description，≤2048 字符）。
    pub digest: Option<String>,
    /// 正文 Markdown（提取失败时为 None，调用方可回退纯文本方案）。
    pub markdown: Option<String>,
    /// 发布时间（epoch 秒）。
    pub publish_time: Option<i64>,
    /// 正文图片（640px 标准化）。
    pub images: Vec<String>,
}

/// 解析失败类别。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// 微信返回了人机验证页（IP 被限流）：本篇没有内容，且应停止本批继续抓。
    VerifyPage,
    /// 微信返回了 weui 提示页（如「此内容暂时无法查看」「该内容已被发布者删除」）：
    /// 该文**永久不可用**，调用方应标记后不再重试。附带页面上的原因文案。
    Unavailable(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::VerifyPage => {
                write!(f, "WeChat returned a verification page (IP rate-limited)")
            }
            ParseError::Unavailable(reason) => write!(f, "article unavailable: {reason}"),
        }
    }
}

impl std::error::Error for ParseError {}

fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("selector 字面量应合法")
}

/// 从 URL 提取文章短 id（`https://mp.weixin.qq.com/s/<22位>`）。
fn extract_article_id(url: &str) -> Option<String> {
    let parts: Vec<&str> = url.split('/').collect();
    if parts.len() == 5 && parts[4].len() == 22 {
        Some(parts[4].to_string())
    } else {
        None
    }
}

/// 是否为微信人机验证页（限流 / 风控）：引用了 `verify.js`，或前 3000 字节出现 `register_code`。
///
/// 纯判定、无网络；`parse_html` 据此返回 [`ParseError::VerifyPage`]，MITM 接力也用它在
/// 响应层识别验证页（命中即中止本批，不再往下跳）。
pub fn is_verify_page(html: &str) -> bool {
    // 只看前 3000 字节；按字符边界回退，避免切在多字节汉字中间 panic。
    let mut end = html.len().min(3000);
    while end > 0 && !html.is_char_boundary(end) {
        end -= 1;
    }
    let head = &html[..end];
    html.contains("secitptpage/template/verify.js") || head.contains("register_code")
}

/// 将原始 HTML 解析为 [`ArticleResult`]（主入口，对齐 Python `_parse_html`）。
pub fn parse_html(url: &str, html: &str) -> Result<ArticleResult, ParseError> {
    if is_verify_page(html) {
        return Err(ParseError::VerifyPage);
    }

    let doc = Html::parse_document(html);
    let mut r = ArticleResult {
        article_id: extract_article_id(url),
        ..Default::default()
    };

    meta::extract_og_meta(&doc, &mut r);
    let scripts = meta::collect_scripts(&doc);

    // 1) 富文本
    if let Some(node) = doc.select(&sel("div.rich_media_content")).next() {
        for s in &scripts {
            meta::extract_rich_text_meta(s, &mut r);
        }
        content::extract_rich_media(&node.html(), &mut r);
        return Ok(r);
    }

    // 2) 转载页
    if let Some(node) = doc.select(&sel("div.original_page")).next() {
        for s in &scripts {
            meta::extract_rich_text_meta(s, &mut r);
        }
        content::extract_repost(&doc, &node.html(), &mut r);
        return Ok(r);
    }

    // 3) 纯文本
    if doc.select(&sel("p#js_text_desc")).next().is_some() {
        for s in &scripts {
            meta::extract_swiper_meta(s, &mut r);
        }
        content::extract_plain_text(&scripts, &mut r);
        content::shorten_long_title(&mut r);
        return Ok(r);
    }

    // 4) 视频分享
    if doc
        .select(&sel("div#js_common_share_desc_wrap"))
        .next()
        .is_some()
    {
        for s in &scripts {
            meta::extract_swiper_meta(s, &mut r);
        }
        content::extract_plain_text(&scripts, &mut r);
        return Ok(r);
    }

    // 5) 图片轮播
    if doc
        .select(&sel("div.share_media_swiper_content"))
        .next()
        .is_some()
    {
        for s in &scripts {
            meta::extract_swiper_meta(s, &mut r);
        }
        content::extract_swiper(&scripts, &mut r);
        return Ok(r);
    }

    // 6) 全屏布局
    if doc
        .select(&sel("div#js_fullscreen_layout_padding"))
        .next()
        .is_some()
    {
        for s in &scripts {
            meta::extract_swiper_meta(s, &mut r);
        }
        content::extract_fullscreen(&scripts, &mut r);
        content::shorten_long_title(&mut r);
        if r.markdown.is_none() {
            // 新版「短文」SPA 空壳：页内没有 picture_page_info_list / text_page_info，
            // 正文由前端 JS 带登录态拉取，纯 HTTP 抓不到（换微信 UA 会触发风控验证页）。
            // 报不可用让调用方标记，避免反复重试注定失败的文章。
            return Err(ParseError::Unavailable(
                "暂不支持的版式：微信短文页（正文需客户端渲染，纯 HTTP 抓不到）".to_string(),
            ));
        }
        return Ok(r);
    }

    // 六种版式都未命中：先判是不是 weui 提示页（删除 / 违规 / 暂时无法查看等占位页，
    // HTTP 仍是 200）——这类文章永久不可用，报给调用方标记，避免反复重抓。
    if html.contains("weui-msg") || html.contains("window.cgiData") {
        return Err(ParseError::Unavailable(unavailable_reason(html)));
    }

    // 兜底：尽力提取元数据（正文留空，调用方可回退）。
    for s in &scripts {
        meta::extract_rich_text_meta(s, &mut r);
        meta::extract_swiper_meta(s, &mut r);
    }
    Ok(r)
}

/// 从提示页里抠出原因文案（如「此内容暂时无法查看」「该内容已被发布者删除」）。
fn unavailable_reason(html: &str) -> String {
    use regex::Regex;
    use std::sync::LazyLock;
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"[\p{Han}][\p{Han}，、]{1,30}?(?:无法查看|已被发布者删除|已删除|已被屏蔽|被屏蔽|违规|已迁移|发送失败|涉嫌侵权)").unwrap()
    });
    RE.find(html)
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "内容无法查看".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RICH: &str = r#"<html><head>
      <meta property="og:title" content="测试标题&amp;A" />
      <meta property="og:image" content="https://mmbiz.qpic.cn/c/x/640" />
      <meta property="og:description" content="摘要<b>加粗</b>" />
    </head><body>
      <script type="text/javascript">
        window.__allowLoadResFromMp = true;
        var biz = "MzA5MTIxNzU=" || "";
        var mid = "2650000001" || "";
        var idx = "1" || "";
        var sn = "abcdef0123" || "";
      </script>
      <script type="text/javascript">
        var hd_head_img = "https://mmbiz.qpic.cn/head/0";
        var nickname = htmlDecode("测试公众号");
        var oriCreateTime = '1700000000';
      </script>
      <div class="rich_media_content" id="js_content">
        <p>第一段<span>内联</span></p>
        <img data-src="https://mmbiz.qpic.cn/mmbiz_jpg/AAA/BBB/640?wx_fmt=jpeg" />
        <img src="data:image/gif;base64,xx" />
        <p>第二段</p>
      </div>
    </body></html>"#;

    #[test]
    fn rich_media_full_flow() {
        let r = parse_html("https://mp.weixin.qq.com/s/AbCdEfGhIjKlMnOpQrStUv", RICH).unwrap();
        assert_eq!(r.article_id.as_deref(), Some("AbCdEfGhIjKlMnOpQrStUv"));
        assert_eq!(r.title.as_deref(), Some("测试标题&A"));
        assert_eq!(r.mp_name.as_deref(), Some("测试公众号"));
        assert_eq!(r.biz.as_deref(), Some("MzA5MTIxNzU="));
        assert_eq!(r.mid.as_deref(), Some("2650000001"));
        assert_eq!(r.idx, Some(1));
        assert_eq!(r.sn.as_deref(), Some("abcdef0123"));
        assert_eq!(r.publish_time, Some(1_700_000_000));
        assert_eq!(r.digest.as_deref(), Some("摘要加粗"));
        // 图片：lazy data-src 标准化为 640；data: 占位图被剔除
        assert_eq!(r.images, vec!["https://mmbiz.qpic.cn/mmbiz_jpg/AAA/640"]);
        let md = r.markdown.expect("应有 markdown");
        assert!(md.contains("第一段"), "markdown: {md}");
        assert!(md.contains("第二段"));
        assert!(md.contains("https://mmbiz.qpic.cn/mmbiz_jpg/AAA/640"));
        assert!(!md.contains("data:image"));
    }

    #[test]
    fn verify_page_detected() {
        let html = r#"<html><script src="/mp/secitptpage/template/verify.js"></script></html>"#;
        assert_eq!(parse_html("u", html).unwrap_err(), ParseError::VerifyPage);
    }

    #[test]
    fn unavailable_page_detected() {
        // 模拟微信 weui 提示页（真实样式：window.cgiData + weui-msg，正文版式全缺席）
        let html = r#"<html><body><div class="weui-msg"></div><script>
          window.cgiData = { config: '{"desc":"&lt;span&gt;此内容暂时无法查看&lt;/span&gt;"}' }
        </script></body></html>"#;
        match parse_html("u", html).unwrap_err() {
            ParseError::Unavailable(reason) => assert_eq!(reason, "此内容暂时无法查看"),
            other => panic!("应判为 Unavailable，实际 {other:?}"),
        }
    }

    #[test]
    fn fullscreen_spa_shell_is_unavailable() {
        // 新版短文页：命中全屏容器，但没有任何内容脚本（SPA 空壳）
        let html = r#"<html><body>
          <div id="js_fullscreen_layout_padding" class="fullscreen-layout-padding"></div>
          <div id="app"></div>
        </body></html>"#;
        match parse_html("u", html).unwrap_err() {
            ParseError::Unavailable(reason) => assert!(reason.contains("短文页"), "{reason}"),
            other => panic!("应判为 Unavailable，实际 {other:?}"),
        }
    }

    #[test]
    fn fallback_only_meta() {
        let html = r#"<html><body><script type="text/javascript">
          var hd_head_img = "https://x/head";
          var nickname = htmlDecode("兜底号");
        </script></body></html>"#;
        let r = parse_html("u", html).unwrap();
        assert_eq!(r.mp_name.as_deref(), Some("兜底号"));
        assert!(r.markdown.is_none());
    }
}
