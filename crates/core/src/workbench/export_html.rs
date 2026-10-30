//! 离线正文与交付模板：保留微信正文的内联排版，结构与 mphistory 一致。
use super::markdown_export::{escaped, source_url};
use scraper::{Html, Selector};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
};

static SCRIPTS: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?is)<script\b[^>]*>.*?</script\s*>").unwrap());
static CSS_URL: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"(?i)url\(\s*["']?([^"')]+)["']?\s*\)"#).unwrap());

pub(super) fn image_key(value: &str) -> String {
    url::Url::parse(value)
        .map(|u| format!("{}{}", u.host_str().unwrap_or_default(), u.path()))
        .unwrap_or_else(|_| value.into())
}

pub(super) fn local_image(value: &str, images: &HashMap<String, String>) -> Option<String> {
    if images.values().any(|local| local == value) {
        return Some(value.to_string());
    }
    images
        .get(value)
        .or_else(|| images.get(&image_key(value)))
        .cloned()
        .or_else(|| {
            let normalized = if value.starts_with("//") {
                format!("https:{value}")
            } else {
                value.replace("http://", "https://")
            };
            // The archive downloader stores article-md's 640px CDN variant, while
            // the original page commonly uses /0 or /1000 with extra query parameters.
            let archive_variant = url::Url::parse(&normalized)
                .ok()
                .filter(|u| u.host_str() == Some("mmbiz.qpic.cn"))
                .map(|_| {
                    format!(
                        "{}/640",
                        normalized.split('/').take(5).collect::<Vec<_>>().join("/")
                    )
                });
            for url in [
                Some(value),
                Some(normalized.as_str()),
                archive_variant.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                let hash = format!("{:x}", Sha256::digest(url.as_bytes()));
                for ext in ["png", "jpg", "gif", "webp", "avif"] {
                    if let Some(local) = images.get(&format!("images/{hash}.{ext}")) {
                        return Some(local.clone());
                    }
                }
            }
            None
        })
}

fn clean_style(style: &str, images: &HashMap<String, String>) -> String {
    style
        .split(';')
        .filter_map(|declaration| {
            let (property, value) = declaration.split_once(':')?;
            let property = property.trim().to_ascii_lowercase();
            let value = value.trim();
            // CSS escape/comment tricks and active/positioning features have no place in an offline article.
            if !property.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                || value.contains(['\\', '<', '>'])
            {
                return None;
            }
            let lower = value.to_ascii_lowercase();
            if [
                "expression",
                "javascript:",
                "@import",
                "/*",
                "behavior",
                "-moz-binding",
            ]
            .iter()
            .any(|s| lower.contains(s))
                || ["position", "z-index", "behavior", "-moz-binding"].contains(&property.as_str())
                || (property == "visibility" && lower.starts_with("hidden"))
                || (property == "opacity"
                    && lower
                        .trim_end_matches("!important")
                        .trim()
                        .parse::<f32>()
                        .is_ok_and(|n| n == 0.))
            {
                return None;
            }
            let mut missing = false;
            let rewritten = CSS_URL.replace_all(value, |cap: &regex::Captures<'_>| {
                if let Some(local) = local_image(cap[1].trim(), images) {
                    format!("url(\"{local}\")")
                } else {
                    missing = true;
                    "none".into()
                }
            });
            if missing {
                None
            } else {
                Some(format!("{property}: {rewritten}"))
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// 只提取正文；原页面脚本、头部、追踪元素和会话参数不会进入交付物。
pub(super) fn body(raw: &str, images: &HashMap<String, String>) -> Option<String> {
    let stripped = SCRIPTS.replace_all(raw, "");
    let doc = Html::parse_document(&stripped);
    let fragment = [
        "#js_content",
        "#js_image_desc",
        "#js_text_desc",
        "#js_share_content",
        ".rich_media_content",
        "#js_image_content",
    ]
    .iter()
    .find_map(|s| {
        doc.select(&Selector::parse(s).unwrap())
            .next()
            .filter(|e| {
                !e.text().collect::<String>().trim().is_empty()
                    || e.select(&Selector::parse("img,video,iframe").unwrap())
                        .next()
                        .is_some()
            })
            .map(|e| e.inner_html())
    })?;
    Some(clean(&fragment, images))
}

fn decoded_text(value: &str) -> String {
    // Attribute values can contain a second layer of entities (e.g. audio titles).
    let text = value.replace('<', "&lt;").replace('>', "&gt;");
    Html::parse_fragment(&text).root_element().text().collect()
}

pub(super) fn clean(fragment: &str, images: &HashMap<String, String>) -> String {
    let doc = Html::parse_fragment(fragment);
    // Work on canonical serialized HTML so replacement also handles mixed-case and unquoted attributes.
    let mut output = doc.root_element().inner_html();
    let media = Selector::parse("iframe,video,mpvideosnap,mp-common-videosnap,mpvideo,audio,mpvoice,mp-common-mpaudio,mpaudio,qqmusic,mp-common-profile,img").unwrap();
    // Reverse order ensures nested replacements cannot leave active children behind.
    for el in doc.select(&media).collect::<Vec<_>>().into_iter().rev() {
        let name = el.value().name();
        let replacement = if name == "img" {
            let raw = el
                .value()
                .attr("data-src")
                .filter(|s| !s.is_empty())
                .or_else(|| el.value().attr("src"))
                .unwrap_or_default();
            if let Some(local) = local_image(raw, images) {
                format!(
                    "<img src=\"{}\" alt=\"{}\" style=\"{}\" loading=\"lazy\">",
                    escaped(&local),
                    escaped(el.value().attr("alt").unwrap_or_default()),
                    escaped(&clean_style(
                        el.value().attr("style").unwrap_or_default(),
                        images
                    ))
                )
            } else {
                "<span class=\"mph-media\">［图片未保存在本地］</span>".into()
            }
        } else if name == "mp-common-profile" {
            format!(
                "<p class=\"mph-media\">[公众号名片] {}</p>",
                escaped(el.value().attr("data-nickname").unwrap_or_default())
            )
        } else if [
            "audio",
            "mpvoice",
            "mp-common-mpaudio",
            "mpaudio",
            "qqmusic",
        ]
        .contains(&name)
        {
            format!(
                "<p class=\"mph-media\">[音频] {} 请在原文中收听</p>",
                escaped(&decoded_text(
                    el.value()
                        .attr("name")
                        .or_else(|| el.value().attr("music_name"))
                        .unwrap_or_default()
                ))
            )
        } else {
            "<p class=\"mph-media\">[视频] 请在原文中观看</p>".into()
        };
        output = output.replace(&el.html(), &replacement);
    }
    let images = images.clone();
    let locals: HashSet<String> = images.values().cloned().collect();
    ammonia::Builder::default()
        .add_tags(["section"])
        .add_generic_attributes(["style", "class", "align"])
        .add_tag_attributes("img", ["loading"])
        .add_clean_content_tags([
            "style",
            "noscript",
            "template",
            "form",
            "iframe",
            "object",
            "embed",
            "mp-style-type",
        ])
        .url_relative(ammonia::UrlRelative::PassThrough)
        .attribute_filter(move |tag, attr, value| {
            if attr == "style" {
                return Some(clean_style(value, &images).into());
            }
            if tag == "img" && attr == "src" {
                return locals.contains(value).then(|| value.into());
            }
            if tag == "a" && attr == "href" {
                let sanitized = source_url(value);
                return (!sanitized.is_empty()).then(|| sanitized.into());
            }
            Some(value.into())
        })
        .clean(&output)
        .to_string()
}

pub(super) fn safe_name(name: &str, max: usize) -> String {
    let s: String = name
        .chars()
        .filter_map(|c| {
            if ['\t', '\r', '\n'].contains(&c) {
                Some(' ')
            } else if c.is_control()
                || ('\u{200b}'..='\u{200f}').contains(&c)
                || ['\u{2028}', '\u{2029}', '\u{feff}'].contains(&c)
            {
                None
            } else if "\\/:*?\"<>|".contains(c) {
                Some('_')
            } else {
                Some(c)
            }
        })
        .collect();
    let s = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect::<String>()
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .to_string();
    if s.is_empty() {
        return "无标题".into();
    }
    let upper = s.to_ascii_uppercase();
    if ["CON", "PRN", "AUX", "NUL"].contains(&upper.as_str())
        || regex::Regex::new(r"^(COM|LPT)\d$")
            .unwrap()
            .is_match(&upper)
    {
        format!("_{s}")
    } else {
        s
    }
}

pub(super) fn template(source: &str, values: &[(&str, &str)]) -> String {
    static FIELD: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"\{\{([a-zA-Z]+)\}\}").unwrap());
    FIELD
        .replace_all(source, |c: &regex::Captures<'_>| {
            values
                .iter()
                .find(|(k, _)| *k == &c[1])
                .map(|(_, v)| *v)
                .unwrap_or("")
                .to_string()
        })
        .into_owned()
}

pub(super) fn article(
    title: &str,
    account: &str,
    published: &str,
    url: &str,
    kind: &str,
    body: &str,
) -> String {
    template(
        include_str!("export_templates/article.html"),
        &[
            ("title", &escaped(title)),
            ("account", &escaped(account)),
            ("publishTime", &escaped(published)),
            ("url", &escaped(url)),
            (
                "type",
                &if kind == "文章" {
                    String::new()
                } else {
                    format!("<span>{}</span>", escaped(kind))
                },
            ),
            ("body", body),
            (
                "promotion",
                include_str!("export_templates/promotion.html").trim(),
            ),
        ],
    )
}

pub(super) fn article_type(raw: Option<&str>) -> String {
    static TYPE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"[^_a-z]item_show_type\s*[:=]\s*['"]?(\d+)"#).unwrap()
    });
    match raw
        .and_then(|s| TYPE.captures(s))
        .map(|c| c[1].to_string())
        .as_deref()
    {
        Some("5") => "视频".into(),
        Some("7") => "音乐".into(),
        Some("8") => "图片".into(),
        Some("10") => "纯文字".into(),
        Some("0") | None => "文章".into(),
        Some(n) => format!("其他({n})"),
    }
}

pub(super) fn csv(value: &str) -> String {
    let s = if value.starts_with(['=', '+', '@', '-', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.to_string()
    };
    if s.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}
