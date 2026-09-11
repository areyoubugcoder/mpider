//! 正文内容提取：六种版式各自的内容块 → Markdown。
//! 与 Python 版 `_extract_rich_media_content` 等函数逐一对齐；
//! HTML→Markdown 用 htmd（html5ever 系），DOM 改写用字符串/正则变换实现
//! （scraper 的 DOM 不可变；img 是 void 元素、svg 用配对扫描，正则变换足够稳）。

use regex::Regex;
use scraper::{Html, Selector};
use std::collections::HashSet;
use std::sync::LazyLock;

use crate::text::{decode_text, normalize_image_url, percent_decode};
use crate::ArticleResult;

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).unwrap());
    };
}

/// HTML → Markdown（htmd），折叠 3+ 连续空行；空结果返回 None。
fn to_markdown(html: &str) -> Option<String> {
    let md = htmd::convert(html).ok()?;
    re!(BLANKS, r"\n{3,}");
    let md = BLANKS.replace_all(&md, "\n\n").trim().to_string();
    if md.is_empty() {
        None
    } else {
        Some(md)
    }
}

/// 剥离 `<script>`/`<style>` 块（跨行、大小写不敏感）。
fn strip_script_style(html: &str) -> String {
    re!(
        RE,
        r"(?is)<script\b[^>]*>.*?</script\s*>|<style\b[^>]*>.*?</style\s*>"
    );
    RE.replace_all(html, "").into_owned()
}

/// 富文本：img 标准化（收集 640 图、剔除非 http 占位图）+ svg 背景图转 img，再转 Markdown。
pub fn extract_rich_media(content_html: &str, r: &mut ArticleResult) {
    let mut seen: HashSet<String> = HashSet::new();
    let html = strip_script_style(content_html);
    let html = transform_imgs(&html, &mut r.images, &mut seen);
    let html = transform_svgs(&html, &mut r.images, &mut seen);
    r.markdown = to_markdown(&html);
}

/// `<img>`：优先 src、回退 data-src（微信懒加载）；http 图标准化为 640 并收集，其余剔除。
fn transform_imgs(html: &str, images: &mut Vec<String>, seen: &mut HashSet<String>) -> String {
    re!(IMG, r"(?is)<img\b[^>]*>");
    re!(SRC, r#"(?is)\bsrc\s*=\s*["']([^"']*)["']"#);
    re!(DATA_SRC, r#"(?is)\bdata-src\s*=\s*["']([^"']*)["']"#);
    IMG.replace_all(html, |c: &regex::Captures<'_>| {
        let tag = &c[0];
        let src = SRC
            .captures(tag)
            .map(|m| m[1].to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| DATA_SRC.captures(tag).map(|m| m[1].to_string()));
        match src {
            Some(u) if u.starts_with("http") => {
                let norm = normalize_image_url(&u);
                if seen.insert(norm.clone()) {
                    images.push(norm.clone());
                }
                format!(r#"<img src="{norm}">"#)
            }
            _ => String::new(), // 无链接/占位图：剔除
        }
    })
    .into_owned()
}

/// `<svg>` 承载正文图片（background-image）：整块（含嵌套）替换为其中收集到的 `<img>` 序列。
fn transform_svgs(html: &str, images: &mut Vec<String>, seen: &mut HashSet<String>) -> String {
    // 序列化后的 style 里引号可能是 &quot;，两种写法都要认。
    re!(
        BG,
        r#"background-image[^;"']*?url\(\s*(?:&quot;|"|')?([^"'&)\s]+)"#
    );
    let lower = html.to_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut pos = 0;
    while let Some(rel) = lower[pos..].find("<svg") {
        let start = pos + rel;
        out.push_str(&html[pos..start]);
        // 配对扫描找本 svg（含嵌套）的整块结束位置。
        let mut depth = 0usize;
        let mut cursor = start;
        let end = loop {
            let next_open = lower[cursor + 1..].find("<svg").map(|i| cursor + 1 + i);
            let next_close = lower[cursor + 1..].find("</svg").map(|i| cursor + 1 + i);
            match (next_open, next_close) {
                (Some(o), Some(c)) if o < c => {
                    depth += 1;
                    cursor = o;
                }
                (_, Some(c)) => {
                    if depth == 0 {
                        // 闭合标签的 '>' 之后
                        break lower[c..]
                            .find('>')
                            .map(|g| c + g + 1)
                            .unwrap_or(html.len());
                    }
                    depth -= 1;
                    cursor = c;
                }
                _ => break html.len(), // 未闭合：吞到结尾
            }
        };
        let block = &html[start..end];
        for cap in BG.captures_iter(block) {
            let norm = normalize_image_url(&cap[1]);
            if seen.insert(norm.clone()) {
                images.push(norm.clone());
            }
            out.push_str(&format!(r#"<img src="{norm}">"#));
        }
        pos = end;
    }
    out.push_str(&html[pos..]);
    out
}

/// 转载页：`#js_share_notice` 里 `innerHTML = "..."` 的提示文本 + 原文链接。
pub fn extract_repost(doc: &Html, block_html: &str, r: &mut ArticleResult) {
    re!(INNER, r#"innerHTML = "([^"]+)""#);
    let Some(c) = INNER.captures(block_html) else {
        return;
    };
    let text = decode_text(&c[1], true);
    let mut html = format!("<p>{text}</p>");
    let sel = Selector::parse("span#js_share_source").expect("selector 应合法");
    if let Some(href) = doc
        .select(&sel)
        .next()
        .and_then(|n| n.value().attr("data-url"))
    {
        html.push_str(&format!(r#"<p><a href="{href}">查看原文</a></p>"#));
    }
    r.markdown = to_markdown(&html);
}

/// 纯文本 / 视频分享：`var ContentNoEncode = ... || '...'` 的正文。
pub fn extract_plain_text(scripts: &[String], r: &mut ArticleResult) {
    re!(
        CONTENT,
        r"var ContentNoEncode = window\.a_value_which_never_exists \|\| '([^']+)';"
    );
    for s in scripts {
        if !s.contains("var TextContentNoEncode =") {
            continue;
        }
        if let Some(c) = CONTENT.captures(s) {
            let text = percent_decode(&decode_text(&c[1], true));
            r.markdown = to_markdown(&format!("<p>{text}</p>"));
        }
    }
}

/// `picture_page_info_list` 里正文图片的 cdn_url（排除 watermark_info / share_cover 名下的）。
fn extract_picture_cdn_urls(script: &str) -> Vec<String> {
    re!(
        CDN,
        r"(watermark_info|share_cover)?\s*(?::\s*\{[^}]*?)?\bcdn_url:\s*'([^']*)'"
    );
    let mut seen = HashSet::new();
    let mut urls = Vec::new();
    for c in CDN.captures_iter(script) {
        if c.get(1).is_some() || c[2].is_empty() {
            continue;
        }
        let norm = normalize_image_url(&c[2]);
        if seen.insert(norm.clone()) {
            urls.push(norm);
        }
    }
    urls
}

/// 图片轮播（小红书风格）：cdn 图序列 + `window.desc` 文案。
pub fn extract_swiper(scripts: &[String], r: &mut ArticleResult) {
    re!(DESC, r#"window.desc = "([^"]+)""#);
    for s in scripts {
        if !s.contains("window.picture_page_info_list =") {
            continue;
        }
        r.images = extract_picture_cdn_urls(s);
        let mut parts: Vec<String> = r
            .images
            .iter()
            .map(|u| format!(r#"<img src="{u}" /><br>"#))
            .collect();
        if let Some(c) = DESC.captures(s) {
            parts.push(format!("<p>{}</p>", decode_text(&c[1], true)));
        }
        if !parts.is_empty() {
            r.markdown = to_markdown(&parts.concat());
        }
    }
}

/// 全屏布局（appmsg_type 10002）：cdn 图 + `text_page_info` 的 content(_noencode)。
pub fn extract_fullscreen(scripts: &[String], r: &mut ArticleResult) {
    re!(NOENC, r"(?s)content_noencode:\s*(?:JsDecode\(\s*)?'(.*?)'");
    re!(CONT, r"(?s)content:\s*(?:JsDecode\(\s*)?'(.*?)'");
    for s in scripts {
        if !s.contains("picture_page_info_list") {
            continue;
        }
        let mut parts: Vec<String> = Vec::new();
        r.images = extract_picture_cdn_urls(s);
        for u in &r.images {
            parts.push(format!(r#"<img src="{u}" /><br>"#));
        }
        for re in [&*NOENC, &*CONT] {
            if let Some(c) = re.captures(s) {
                let text = percent_decode(&decode_text(&c[1], true));
                parts.push(format!("<p>{text}</p>"));
                break;
            }
        }
        if !parts.is_empty() {
            r.markdown = to_markdown(&parts.concat());
        }
        return;
    }
}

/// 纯文本/全屏版式的标题常是整段正文：>50 字符时截到第一个句号（仍超长则取前 30 字符）。
pub fn shorten_long_title(r: &mut ArticleResult) {
    if let Some(t) = &r.title {
        if t.chars().count() > 50 {
            let short = t.split('。').next().unwrap_or(t);
            r.title = Some(if short.chars().count() <= 50 {
                short.to_string()
            } else {
                t.chars().take(30).collect()
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imgs_transformed_and_collected() {
        let mut images = Vec::new();
        let mut seen = HashSet::new();
        let html = r#"<p>a</p><img data-src="https://mmbiz.qpic.cn/x/y/640?f=1"><img src="data:image/gif;base64,zz">"#;
        let out = transform_imgs(html, &mut images, &mut seen);
        assert_eq!(images, vec!["https://mmbiz.qpic.cn/x/y/640"]);
        assert!(out.contains(r#"<img src="https://mmbiz.qpic.cn/x/y/640">"#));
        assert!(!out.contains("data:image"));
    }

    #[test]
    fn nested_svg_replaced_with_imgs() {
        let mut images = Vec::new();
        let mut seen = HashSet::new();
        let html = concat!(
            r#"<p>前</p><svg style="background-image: url(&quot;https://mmbiz.qpic.cn/a/b/c?x=1&quot;);">"#,
            r#"<foreignobject><svg style="background-image:url('https://mmbiz.qpic.cn/d/e/f')"></svg></foreignobject>"#,
            r#"</svg><p>后</p>"#
        );
        let out = transform_svgs(html, &mut images, &mut seen);
        assert_eq!(
            images,
            vec![
                "https://mmbiz.qpic.cn/a/b/640",
                "https://mmbiz.qpic.cn/d/e/640"
            ]
        );
        assert!(out.starts_with("<p>前</p>"));
        assert!(out.ends_with("<p>后</p>"));
        assert!(!out.contains("<svg"));
        assert_eq!(out.matches("<img").count(), 2);
    }

    #[test]
    fn cdn_urls_skip_watermark_and_cover() {
        let script = r#"
          watermark_info: { cdn_url: 'https://w/a/t/e/r' },
          cdn_url: 'https://mmbiz.qpic.cn/p/1/x',
          share_cover: { cdn_url: 'https://c/o/v/e/r' },
          cdn_url: 'https://mmbiz.qpic.cn/p/2/y',
        "#;
        let urls = extract_picture_cdn_urls(script);
        assert_eq!(
            urls,
            vec![
                "https://mmbiz.qpic.cn/p/1/640",
                "https://mmbiz.qpic.cn/p/2/640"
            ]
        );
    }

    #[test]
    fn shorten_title() {
        let mut r = ArticleResult {
            title: Some("句子一。".repeat(20)),
            ..Default::default()
        };
        shorten_long_title(&mut r);
        assert_eq!(r.title.as_deref(), Some("句子一"));
    }
}
