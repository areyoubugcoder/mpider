//! Interaction counts published in an article page.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ArticleInteractions {
    pub read_count: Option<i64>,
    pub read_display: Option<String>,
    pub like_count: Option<i64>,
    pub watching_count: Option<i64>,
    pub share_count: Option<i64>,
    pub comment_count: Option<i64>,
}

impl ArticleInteractions {
    pub(super) fn parse_html(html: &str) -> Option<Self> {
        use std::sync::LazyLock;

        static ARTICLE_CONTENT: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(r#"(?i)<[a-z][a-z0-9]*[^>]*\sid\s*=\s*["']js_content["']"#).unwrap()
        });
        static SCRIPTS: LazyLock<regex::Regex> =
            LazyLock::new(|| regex::Regex::new(r"(?is)<script\b[^>]*>(.*?)</script\s*>").unwrap());
        static COUNTS: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(
                r#"\b(read_num|old_like_num|like_num|comment_count|share_num)\b\\?["']?\s*[:=]\s*\\?["']?([0-9]+)\b"#,
            )
            .unwrap()
        });
        if !ARTICLE_CONTENT.is_match(html) {
            return None;
        }

        let mut values = [None; 5];
        let mut ambiguous = [false; 5];
        for script in SCRIPTS.captures_iter(html) {
            let source = script.get(1)?.as_str();
            for entry in COUNTS.captures_iter(source) {
                let index = match &entry[1] {
                    "read_num" => 0,
                    "old_like_num" => 1,
                    "like_num" => 2,
                    "comment_count" => 3,
                    "share_num" => 4,
                    _ => continue,
                };
                if let Ok(value) = entry[2].parse::<i64>() {
                    ambiguous[index] |= values[index].is_some_and(|prior| prior != value);
                    values[index] = Some(value);
                }
            }
        }
        for (value, is_ambiguous) in values.iter_mut().zip(ambiguous) {
            if is_ambiguous {
                *value = None;
            }
        }
        let [read_count, like_count, watching_count, comment_count, share_count] = values;
        let metrics = Self {
            read_count,
            read_display: None,
            like_count,
            watching_count,
            share_count,
            comment_count,
        };
        (metrics != Self::default()).then_some(metrics)
    }
}
