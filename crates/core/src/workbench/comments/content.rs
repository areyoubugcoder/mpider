//! Convert comment markup to inert text/link parts, including replies already saved locally.
use super::ArticleComment;
use scraper::{ElementRef, Html, Node};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CommentPart {
    pub text: String,
    pub url: Option<String>,
}
fn append(parts: &mut Vec<CommentPart>, text: &str, url: Option<&str>) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = parts.last_mut().filter(|p| p.url.as_deref() == url) {
        last.text.push_str(text);
    } else {
        parts.push(CommentPart {
            text: text.into(),
            url: url.map(str::to_owned),
        });
    }
}
fn walk(
    element: ElementRef<'_>,
    parts: &mut Vec<CommentPart>,
    inherited: Option<&str>,
    depth: usize,
) {
    if depth > 64 {
        return;
    }
    let name = element.value().name();
    if matches!(
        name,
        "script" | "style" | "iframe" | "object" | "embed" | "svg" | "math" | "template"
    ) {
        return;
    }
    if name == "br" {
        append(parts, "\n", inherited);
        return;
    }
    let target = (name == "a")
        .then(|| element.value().attr("href"))
        .flatten()
        .and_then(|value| url::Url::parse(value).ok())
        .filter(|u| {
            matches!(u.scheme(), "https" | "http")
                && u.host_str().is_some()
                && u.username().is_empty()
                && u.password().is_none()
        });
    let link = target.as_ref().map(url::Url::as_str).or(inherited);
    for child in element.children() {
        match child.value() {
            Node::Text(text) => append(parts, &text.text, link),
            Node::Element(_) => {
                if let Some(child) = ElementRef::wrap(child) {
                    walk(child, parts, link, depth + 1);
                }
            }
            _ => {}
        }
    }
    if matches!(name, "p" | "div" | "li" | "blockquote") {
        append(parts, "\n", None);
    }
}
impl ArticleComment {
    pub(crate) fn render_content(&mut self) {
        let html = Html::parse_fragment(&self.content);
        let mut parts = Vec::new();
        walk(html.root_element(), &mut parts, None, 0);
        self.content_parts = parts;
        for reply in &mut self.replies {
            reply.render_content();
        }
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn saved_markup_becomes_text_and_safe_links_without_executing_html() {
        let store = crate::Store::open_in_memory().unwrap();
        let id = store
            .upsert_article("B", "1", 1, &Default::default())
            .unwrap();
        let row = super::super::parse_comment(&serde_json::json!({"content_id":"1","content":"主留言", "reply_new":{"reply_list":[{"reply_id":"2","content":"推荐：<a class='album' href='https://mp.weixin.qq.com/mp/appmsgalbum?a=1&amp;b=2'>投资心得</a><br>下一行<script>alert(1)</script><img src='https://invalid.test/image'><a href='javascript:alert(1)'>文字</a>"}]}}), 0).unwrap();
        store
            .save_article_interactions_and_comments(id, &Default::default(), &[row])
            .unwrap();
        let page = store.article_comments_page(id, None, 20).unwrap();
        let parts = &page.items[0].replies[0].content_parts;
        assert_eq!(
            parts.iter().map(|p| p.text.as_str()).collect::<String>(),
            "推荐：投资心得\n下一行文字"
        );
        let links: Vec<_> = parts.iter().filter_map(|p| p.url.as_deref()).collect();
        assert_eq!(links, ["https://mp.weixin.qq.com/mp/appmsgalbum?a=1&b=2"]);
    }
}
