//! 元数据提取：og: meta 标签 + 内嵌 `<script>` 里的 JS 变量。
//! 正则与字段语义逐条对齐 Python 版 `_extract_meta` / `_extract_rich_text_meta` / `_extract_swiper_meta`。

use regex::Regex;
use scraper::{Html, Selector};
use std::sync::LazyLock;

use crate::text::{decode_text, strip_html_tags};
use crate::ArticleResult;

fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("selector 字面量应合法")
}

/// 收集所有 `<script type="text/javascript">` 的文本（版式元数据都埋在这里）。
pub fn collect_scripts(doc: &Html) -> Vec<String> {
    doc.select(&sel(r#"script[type="text/javascript"]"#))
        .map(|n| n.text().collect::<String>())
        .collect()
}

/// og:title / og:image / og:description → 标题 / 封面 / 摘要（≤2048 字符）。
pub fn extract_og_meta(doc: &Html, r: &mut ArticleResult) {
    let meta = |prop: &str| -> Option<String> {
        let s = sel(&format!(r#"meta[property="{prop}"]"#));
        let v = doc.select(&s).next()?.value().attr("content")?.trim();
        if v.is_empty() {
            None
        } else {
            Some(v.to_string())
        }
    };
    if let Some(v) = meta("og:title") {
        r.title = Some(strip_html_tags(&decode_text(&v, false)));
    }
    if let Some(v) = meta("og:image") {
        r.cover = Some(v);
    }
    if let Some(v) = meta("og:description") {
        let v = strip_html_tags(&decode_text(&v, false));
        r.digest = Some(v.chars().take(2048).collect());
    }
}

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).unwrap());
    };
}

/// 富文本文章 script 元数据：nickname / oriCreateTime / __allowLoadResFromMp 变量块。
pub fn extract_rich_text_meta(script: &str, r: &mut ArticleResult) {
    if script.contains("var hd_head_img") {
        re!(NICK, r#"var nickname = htmlDecode\((.*)\);"#);
        if let Some(c) = NICK.captures(script) {
            let v = c[1].trim().trim_matches('"').trim_matches('\'').to_string();
            if !v.is_empty() {
                r.mp_name = Some(v);
            }
        }
        re!(ALIAS, r#"alias: '([^']+)'"#);
        if let Some(c) = ALIAS.captures(script) {
            r.mp_alias = Some(c[1].to_string());
        }
    }

    if script.contains("var oriCreateTime") {
        re!(CT, r#"var oriCreateTime = '(\d+)'"#);
        if let Some(c) = CT.captures(script) {
            r.publish_time = c[1].parse().ok();
        }
    }

    // biz/mid/idx/sn 变量块：逐条 `var name = ...;`，取第一个非空双引号字面量。
    if script.contains("window.__allowLoadResFromMp") {
        re!(VAR, r#"var\s+(\w+)\s*=\s*(.*?);"#);
        re!(LIT, r#""(.*?)""#);
        for c in VAR.captures_iter(script) {
            let name = &c[1];
            let value = LIT
                .captures_iter(&c[2])
                .map(|l| l[1].to_string())
                .find(|s| !s.trim().is_empty())
                .unwrap_or_default();
            match name {
                "biz" if !value.is_empty() => r.biz = Some(value),
                "mid" if !value.is_empty() => r.mid = Some(value),
                "idx" => r.idx = value.parse().ok().or(r.idx),
                "sn" if !value.is_empty() => r.sn = Some(value),
                _ => {}
            }
        }
    }
}

/// 轮播 / 纯文本 / 视频分享页 script 元数据（`window.__initCgiDataConfig` 的 d.* 字段）。
pub fn extract_swiper_meta(script: &str, r: &mut ArticleResult) {
    if script.contains("window.__initCgiDataConfig =") {
        re!(NICK, r#"d\.nick_name.*?:\s*'([^']+)'"#);
        if let Some(c) = NICK.captures(script) {
            let v = c[1].trim_matches('"').trim_matches('\'').to_string();
            if !v.is_empty() {
                r.mp_name = Some(v);
            }
        }
        re!(BIZ, r#"d\.biz.*?:\s*'([^']+)'"#);
        if let Some(c) = BIZ.captures(script) {
            r.biz = Some(c[1].to_string());
        }
        re!(MID, r#"d\.mid.*?:\s*'([^']+)'"#);
        if let Some(c) = MID.captures(script) {
            r.mid = Some(c[1].to_string());
        }
        re!(IDX, r#"d\.idx.*?:\s*'([^']+)'"#);
        if let Some(c) = IDX.captures(script) {
            r.idx = c[1].parse().ok().or(r.idx);
        }
        re!(SN, r#"d\.sn.*?:\s*'([^']+)'"#);
        if let Some(c) = SN.captures(script) {
            r.sn = Some(c[1].to_string());
        }
        re!(CT, r#"d\.create_time.*?:\s*'([^']+)'"#);
        if let Some(c) = CT.captures(script) {
            r.publish_time = c[1].parse().ok().or(r.publish_time);
        }
        if r.article_id.is_none() {
            re!(LINK, r#"d\.msg_link.*?:\s*'([^']+)'"#);
            if let Some(c) = LINK.captures(script) {
                let parts: Vec<&str> = c[1].split('/').collect();
                if parts.len() == 5 && parts[4].len() == 22 {
                    r.article_id = Some(parts[4].to_string());
                }
            }
        }
    }

    if script.contains("window.alias =") {
        re!(ALIAS, r#"window.alias = "([^"]+)""#);
        if let Some(c) = ALIAS.captures(script) {
            r.mp_alias = Some(c[1].to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swiper_meta_fields() {
        let script = r#"
          window.__initCgiDataConfig = {};
          d.nick_name = d.nick_name : '轮播号';
          d.biz = d.biz : 'QUJD';
          d.mid = d.mid : '123';
          d.idx = d.idx : '2';
          d.sn = d.sn : 'sn9';
          d.create_time = d.create_time : '1690000000';
        "#;
        let mut r = ArticleResult::default();
        extract_swiper_meta(script, &mut r);
        assert_eq!(r.mp_name.as_deref(), Some("轮播号"));
        assert_eq!(r.biz.as_deref(), Some("QUJD"));
        assert_eq!(r.mid.as_deref(), Some("123"));
        assert_eq!(r.idx, Some(2));
        assert_eq!(r.sn.as_deref(), Some("sn9"));
        assert_eq!(r.publish_time, Some(1_690_000_000));
    }
}
