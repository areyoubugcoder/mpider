//! 文本解码辅助：对齐 Python 版 `_decode_hex_escapes` / `_decode_text` / `_normalize_image_url`。

use regex::Regex;
use std::sync::LazyLock;

/// `\xHH` 十六进制转义 → 对应字符。
pub fn decode_hex_escapes(text: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\x([0-9a-fA-F]{2})").unwrap());
    RE.replace_all(text, |c: &regex::Captures<'_>| {
        let v = u32::from_str_radix(&c[1], 16).unwrap_or(0);
        char::from_u32(v).map(String::from).unwrap_or_default()
    })
    .into_owned()
}

/// HTML 实体反转义：命名常用实体 + 数字实体（覆盖微信页面实际出现的集合）。
pub fn html_unescape(text: &str) -> String {
    static NUM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"&#(x[0-9a-fA-F]+|\d+);").unwrap());
    let s = NUM
        .replace_all(text, |c: &regex::Captures<'_>| {
            let body = &c[1];
            let v = if let Some(hex) = body.strip_prefix('x') {
                u32::from_str_radix(hex, 16).ok()
            } else {
                body.parse::<u32>().ok()
            };
            v.and_then(char::from_u32)
                .map(String::from)
                .unwrap_or_else(|| c[0].to_string())
        })
        .into_owned();
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// 对齐 Python `_decode_text`：hex 转义 → 实体 → 换行处理 → 空白折叠。
pub fn decode_text(text: &str, preserve_newlines: bool) -> String {
    if text.is_empty() {
        return String::new();
    }
    let t = decode_hex_escapes(text);
    let t = html_unescape(&t);
    let t = t.replace('\r', "");
    let t = if preserve_newlines {
        t.replace('\n', "<br>")
    } else {
        t.replace('\n', "")
    };
    static WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
    WS.replace_all(&t, " ").into_owned()
}

/// 微信图片链接标准化为 640px 宽度版本（截到第 5 段路径 + `/640`）。
pub fn normalize_image_url(url: &str) -> String {
    let parts: Vec<&str> = url.split('/').collect();
    if parts.len() >= 5 {
        format!("{}/640", parts[..5].join("/"))
    } else {
        url.to_string()
    }
}

/// 百分号编码解码（对齐 Python `urllib.parse.unquote`，按 UTF-8 组装）。
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 移除 HTML 标签，只留纯文本（摘要清洗用，粗粒度即可）。
pub fn strip_html_tags(text: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").unwrap());
    RE.replace_all(text, "").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_entities() {
        assert_eq!(decode_hex_escapes(r"a\x41b"), "aAb");
        assert_eq!(html_unescape("A&amp;B&lt;C&#65;&#x42;"), "A&B<CAB");
    }

    #[test]
    fn decode_text_newlines() {
        assert_eq!(decode_text("a\r\nb", true), "a<br>b");
        assert_eq!(decode_text("a\nb  c", false), "ab c");
    }

    #[test]
    fn normalize_640() {
        assert_eq!(
            normalize_image_url("https://mmbiz.qpic.cn/mmbiz_jpg/AAA/BBB/640?wx_fmt=jpeg"),
            "https://mmbiz.qpic.cn/mmbiz_jpg/AAA/640"
        );
        assert_eq!(normalize_image_url("https://a/b"), "https://a/b");
    }

    #[test]
    fn percent_roundtrip() {
        assert_eq!(percent_decode("%E4%B8%AD%20x"), "中 x");
    }
}
