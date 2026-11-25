//! Import the public, rendered article fragment copied in the signed-in WeChat reader.
use anyhow::{ensure, Context, Result};
use scraper::{ElementRef, Html, Selector};
use std::sync::LazyLock;

use super::comments::{merge_comments, ArticleComment};
use super::interactions::ArticleInteractions;
use crate::model::ArticleRow;

static ITEMS: LazyLock<Selector> = LazyLock::new(|| Selector::parse(".js_comment_item").unwrap());
static REPLIES: LazyLock<Selector> = LazyLock::new(|| Selector::parse(".js_reply_item").unwrap());
static BODIES: LazyLock<Selector> = LazyLock::new(|| {
    Selector::parse(".discuss_message .actual-content, .discuss_message").unwrap()
});
static AUTHORS: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(".discuss_user_info_inner").unwrap());
static LIKES: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(".discuss_opr_praise .praise_num").unwrap());
static HEADER: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(".appmsg_comment_con .cmt_mod_header_title").unwrap());

fn source_matches(source: &str, article: &ArticleRow) -> bool {
    let Ok(url) = url::Url::parse(source.trim()) else {
        return false;
    };
    if url.scheme() != "https"
        || url.host_str() != Some("mp.weixin.qq.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
    {
        return false;
    }
    if url.path() == "/s" {
        let mut identity = url
            .query_pairs()
            .filter(|(key, _)| matches!(key.as_ref(), "__biz" | "mid" | "idx"));
        let mut biz = None;
        let mut mid = None;
        let mut idx = None;
        for (key, value) in identity.by_ref() {
            let target = match key.as_ref() {
                "__biz" => &mut biz,
                "mid" => &mut mid,
                "idx" => &mut idx,
                _ => unreachable!(),
            };
            if target.replace(value).is_some() {
                return false;
            }
        }
        return biz.as_deref() == Some(article.biz.as_str())
            && mid.as_deref() == article.mid.as_deref()
            && mid.is_some()
            && idx.as_deref().and_then(|idx| idx.parse::<i64>().ok()) == article.idx
            && idx.is_some()
            && article.idx.is_some();
    }
    url.path().starts_with("/s/")
        && article
            .content_url
            .as_deref()
            .and_then(|link| url::Url::parse(link).ok())
            .is_some_and(|saved| saved.host_str() == url.host_str() && saved.path() == url.path())
}

fn scoped<'a>(root: ElementRef<'a>, selector: &Selector) -> Vec<ElementRef<'a>> {
    root.select(selector)
        .filter(|element| {
            element
                .ancestors()
                .filter_map(ElementRef::wrap)
                .find(|ancestor| {
                    ancestor
                        .value()
                        .classes()
                        .any(|class| matches!(class, "js_comment_item" | "js_reply_item"))
                })
                .is_some_and(|owner| owner.id() == root.id())
        })
        .collect()
}

fn exact_count(text: &str) -> Option<i64> {
    let digits: String = text
        .trim()
        .chars()
        .filter(|c| *c != ',' && !c.is_whitespace())
        .collect();
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn from_item(root: ElementRef<'_>, reply: bool, depth: u8) -> Option<ArticleComment> {
    let attr = if reply {
        "data-reply-id"
    } else {
        "data-content-id"
    };
    let id = root.value().attr(attr)?;
    if id.is_empty() || id == "0" || id.len() > 128 {
        return None;
    }
    let bodies = scoped(root, &BODIES);
    let body = bodies
        .iter()
        .find(|node| {
            node.value()
                .classes()
                .any(|class| class == "actual-content")
        })
        .or_else(|| bodies.first())?;
    let content = body.text().collect::<String>().trim().to_owned();
    if content.is_empty() {
        return None;
    }
    let author = scoped(root, &AUTHORS)
        .first()
        .and_then(|node| node.text().map(str::trim).find(|part| !part.is_empty()))
        .unwrap_or_default()
        .to_owned();
    let likes = scoped(root, &LIKES)
        .first()
        .and_then(|node| exact_count(&node.text().collect::<String>()));
    let is_author = {
        static BADGE: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse(".discuss_author_info").unwrap());
        !scoped(root, &BADGE).is_empty()
    };
    let replies = if depth < 12 {
        scoped(root, &REPLIES)
            .into_iter()
            .filter_map(|item| from_item(item, true, depth + 1))
            .collect()
    } else {
        Vec::new()
    };
    Some(ArticleComment {
        reply_total: None,
        reply_max_id: None,
        content_parts: Vec::new(),
        id: id.to_owned(),
        author,
        content,
        images: vec![],
        likes,
        created_at: None,
        is_author,
        reply_to: None,
        replies,
    })
}

pub(super) fn parse_rendered(
    article: &ArticleRow,
    clipboard: &str,
) -> Result<(ArticleInteractions, Vec<ArticleComment>)> {
    let (headers, body) = clipboard
        .split_once('<')
        .context("剪贴板没有微信文章 HTML")?;
    let source = headers
        .lines()
        .find_map(|line| {
            line.trim_start_matches('\u{feff}')
                .strip_prefix("SourceURL:")
        })
        .context("复制内容没有可核对的文章来源，请在微信原文页按 Ctrl+A、Ctrl+C")?;
    ensure!(
        source_matches(source, article),
        "复制内容来自另一篇文章，请重新复制所选文章"
    );
    let html = Html::parse_fragment(&format!("<{body}"));
    let mut comments = Vec::new();
    merge_comments(
        &mut comments,
        html.select(&ITEMS)
            .filter_map(|item| from_item(item, false, 0)),
    );
    let comment_count = html.select(&HEADER).find_map(|header| {
        let text = header.text().collect::<String>();
        let label = text.trim();
        label
            .strip_prefix("留言")
            .or_else(|| label.strip_prefix("评论"))
            .and_then(exact_count)
    });
    ensure!(
        !comments.is_empty() || comment_count == Some(0),
        "复制内容还没有可读取的留言，请在微信原文中打开留言区后重新复制"
    );
    Ok((
        ArticleInteractions {
            comment_count,
            ..Default::default()
        },
        comments,
    ))
}

#[cfg(windows)]
pub fn import_clipboard(store: &crate::Store, article_id: i64) -> Result<usize> {
    let (article, _) = store.get_article_md(article_id)?.context("文章不存在")?;
    ensure!(article.is_deleted == 0, "文章已删除");
    let html = read_clipboard_html()?;
    let (mut metrics, comments) = parse_rendered(&article, &html)?;
    if let Some(footer) = article.title.as_deref().and_then(read_visible_footer) {
        if metrics.comment_count.is_some() && footer.comment_count == metrics.comment_count {
            metrics.read_count = footer.read_count;
            metrics.read_display = footer.read_display;
            metrics.like_count = footer.like_count;
            metrics.share_count = footer.share_count;
            metrics.watching_count = footer.watching_count;
        }
    }
    store.save_article_interactions_and_comments(article_id, &metrics, &comments)?;
    Ok(comments.len())
}

#[cfg(windows)]
fn read_visible_footer(title: &str) -> Option<ArticleInteractions> {
    use base64::Engine as _;
    use std::os::windows::process::CommandExt;

    let script = include_str!("rendered_metrics.ps1");
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])
        .env("MPIDER_RENDERED_TITLE", title)
        .creation_flags(0x08000000)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| serde_json::from_slice(&output.stdout).ok())
        .flatten()
}

#[cfg(windows)]
fn read_clipboard_html() -> Result<String> {
    use windows::core::w;
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, OpenClipboard, RegisterClipboardFormatW,
    };
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    struct ClipboardGuard;
    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            let _ = unsafe { CloseClipboard() };
        }
    }
    unsafe {
        OpenClipboard(None).context("无法读取剪贴板，请稍后重试")?;
        let _guard = ClipboardGuard;
        let format = RegisterClipboardFormatW(w!("HTML Format"));
        ensure!(format != 0, "无法读取剪贴板的 HTML 格式");
        let data = GetClipboardData(format).context("请先在微信原文页按 Ctrl+A、Ctrl+C")?;
        let memory = HGLOBAL(data.0);
        let size = GlobalSize(memory);
        ensure!(
            size > 0 && size <= 16 * 1024 * 1024,
            "复制的页面内容超出大小上限"
        );
        let pointer = GlobalLock(memory);
        ensure!(!pointer.is_null(), "无法读取复制的页面内容");
        let bytes = std::slice::from_raw_parts(pointer.cast::<u8>(), size);
        let text = std::str::from_utf8(bytes.split(|byte| *byte == 0).next().unwrap_or_default())
            .map(str::to_owned);
        let _ = GlobalUnlock(memory);
        text.context("复制的页面编码无效")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_only_the_selected_article_and_merges_the_drawer_copy() {
        let store = crate::Store::open_in_memory().unwrap();
        let article_id = store
            .upsert_article("B==", "42", 2, &crate::ArticleFields::default())
            .unwrap();
        let (article, _) = store.get_article_md(article_id).unwrap().unwrap();
        let header = "Version:1.0\r\nSourceURL:https://mp.weixin.qq.com/s?__biz=B%3D%3D&mid=42&idx=2&key=opaque\r\n";
        let thread = r#"<li class="js_comment_item" data-content-id="23"><span class="discuss_user_info_inner">读者<span class="discuss_user_ip">北京</span></span><span class="discuss_opr_praise"><span class="praise_num">32</span></span><div class="discuss_message"><div class="actual-content">正文</div></div><div class="js_reply_item" data-reply-id="123"><div class="discuss_author_info"><span class="discuss_user_info_inner">作者甲</span></div><div class="discuss_message"><div class="actual-content">回复</div></div></div></li>"#;
        let fixture = format!("{header}<section class='appmsg_comment_con'><div class='cmt_mod_header_title'>留言 54</div><ol>{thread}{thread}</ol></section>");
        assert!(parse_rendered(&article, &fixture.replace("mid=42", "mid=43")).is_err());
        let (metrics, threads) = parse_rendered(&article, &fixture).unwrap();
        assert_eq!(metrics.comment_count, Some(54));
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].likes, Some(32));
        assert_eq!(threads[0].author, "读者");
        assert_eq!(threads[0].replies.len(), 1);
        assert!(threads[0].replies[0].is_author);
        assert_eq!(threads[0].replies[0].content, "回复");
    }
}
