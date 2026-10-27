//! 将本地文章集合同时导出为 Markdown 与经过清洗的离线 HTML；不执行网络请求。
use crate::{model::now, ArticleRow, Store};
use anyhow::{ensure, Context, Result};
use scraper::{Html, Selector};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
pub struct MarkdownExport {
    pub directory: String,
    pub total: usize,
    pub with_body: usize,
    pub missing_body: usize,
    pub partial_images: usize,
}
struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn plain(value: &str) -> String {
    value
        .replace(['\r', '\n'], " ")
        .replace(['[', ']', '<', '>'], "")
}
fn local_regular(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        "本地归档文件类型异常"
    );
    Ok(())
}
fn local_dir(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "本地归档目录类型异常"
    );
    Ok(())
}
fn source_name(store: &Store, biz: &str) -> Result<String> {
    Ok(store
        .get_account(biz)?
        .and_then(|a| a.nickname)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| biz.to_string()))
}
fn source_url(value: &str) -> String {
    let Ok(mut url) = url::Url::parse(value) else {
        return String::new();
    };
    if !["https", "http"].contains(&url.scheme()) {
        return String::new();
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_fragment(None);
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| {
            !["key", "pass_ticket", "uin", "wxtoken"].contains(&k.to_ascii_lowercase().as_str())
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.set_query(None);
    if !pairs.is_empty() {
        url.query_pairs_mut().extend_pairs(pairs);
    }
    url.to_string()
}

fn escaped(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// 与 Markdown 使用同一份已解析正文；不导出页面脚本、样式或原始响应。
fn article_html(title: &str, markdown: &str, directory: &Path) -> Result<String> {
    let options = pulldown_cmark::Options::ENABLE_TABLES
        | pulldown_cmark::Options::ENABLE_STRIKETHROUGH
        | pulldown_cmark::Options::ENABLE_TASKLISTS;
    let mut body = String::new();
    pulldown_cmark::html::push_html(
        &mut body,
        pulldown_cmark::Parser::new_ext(markdown, options),
    );
    let parsed = Html::parse_fragment(&body);
    body = parsed.root_element().inner_html();
    let image = regex::Regex::new(r"^images/[a-f0-9]+\.(png|jpg|gif|webp|avif)$")?;
    for node in parsed.select(&Selector::parse("img").unwrap()) {
        let src = node.value().attr("src").unwrap_or_default();
        let replacement = if image.is_match(src) && directory.join(src).is_file() {
            format!(
                "<img src=\"{}\" alt=\"{}\" loading=\"lazy\">",
                escaped(src),
                escaped(node.value().attr("alt").unwrap_or_default())
            )
        } else {
            "<p>［图片未保存在本地，HTML 不自动加载远程图片］</p>".into()
        };
        body = body.replace(&node.html(), &replacement);
    }
    let body = ammonia::Builder::default()
        .url_relative(ammonia::UrlRelative::PassThrough)
        .attribute_filter(|element, attribute, value| {
            if element == "a" && attribute == "href" {
                let sanitized = source_url(value);
                return (!sanitized.is_empty()).then(|| sanitized.into());
            }
            Some(value.into())
        })
        .clean(&body)
        .to_string();
    Ok(html_shell(title, &body))
}

fn html_shell(title: &str, body: &str) -> String {
    format!("<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta name=\"referrer\" content=\"no-referrer\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src 'self'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\"><title>{}</title><style>body{{max-width:840px;margin:32px auto;padding:0 20px;font:16px/1.8 system-ui;overflow-wrap:anywhere}}img{{max-width:100%;height:auto}}pre{{overflow:auto;padding:12px;background:#f5f5f5}}code{{font-family:monospace}}table{{border-collapse:collapse}}td,th{{border:1px solid #ddd;padding:6px 10px}}a{{color:#245a9a}}blockquote{{margin-left:0;padding-left:16px;border-left:3px solid #ddd;color:#666}}</style></head><body>{body}</body></html>", escaped(title))
}

/// 打开目录选择前验证全部正文，避免用户选完路径才发现无法导出。
pub fn validate_selection(store: &Store, biz: Option<&str>, ids: &[i64]) -> Result<()> {
    let ids = match biz {
        Some(biz) => store
            .list_articles(Some(biz), -1, 0, false)?
            .into_iter()
            .map(|a| a.id)
            .collect(),
        None => ids.to_vec(),
    };
    ensure!(
        !ids.is_empty() && ids.len() <= 100000,
        "请选择一至十万篇文章"
    );
    let states = store.wb_article_body_states(&ids)?;
    ensure!(
        ids.iter().all(|id| states.get(id) == Some(&true)),
        "所选文章有未下载的正文，请先下载后导出"
    );
    Ok(())
}

/// 发布到新建的唯一子目录；失败只清理本次暂存目录，不覆盖用户已有文件。
pub fn export_source(
    store: &Store,
    root: &Path,
    biz: &str,
    destination: &Path,
) -> Result<MarkdownExport> {
    validate_selection(store, Some(biz), &[])?;
    let articles = store.list_articles(Some(biz), -1, 0, false)?;
    export_rows(store, root, articles, Some(biz), destination)
}

pub fn export_articles(
    store: &Store,
    root: &Path,
    ids: &[i64],
    destination: &Path,
) -> Result<MarkdownExport> {
    validate_selection(store, None, ids)?;
    let ids: std::collections::BTreeSet<i64> = ids.iter().copied().collect();
    ensure!(
        !ids.is_empty() && ids.len() <= 100000,
        "请选择一至十万篇文章"
    );
    let mut articles = Vec::new();
    for id in ids {
        let (article, _) = store.get_article_md(id)?.context("文章已删除或不存在")?;
        ensure!(article.is_deleted == 0, "文章已删除或不存在");
        articles.push(article);
    }
    export_rows(store, root, articles, None, destination)
}
fn export_rows(
    store: &Store,
    root: &Path,
    articles: Vec<ArticleRow>,
    biz: Option<&str>,
    destination: &Path,
) -> Result<MarkdownExport> {
    ensure!(destination.is_absolute(), "请选择绝对路径的导出目录");
    let destination = destination.canonicalize().context("导出目录不存在")?;
    ensure!(destination.is_dir(), "请选择文件夹");
    ensure!(!articles.is_empty(), "没有可导出的已入库文章");
    let export_ids: Vec<i64> = articles.iter().map(|article| article.id).collect();
    let hash = format!("{:x}", Sha256::digest(biz.unwrap_or("selected").as_bytes()));
    let stem = format!("mp-articles-{}-{}", &hash[..12], (now() * 1000.) as u64);
    let (stage, published) = (0..1000)
        .find_map(|n| {
            let published = destination.join(format!("{stem}-{n}"));
            let stage = destination.join(format!(".{stem}-{n}.tmp"));
            if published.exists() {
                return None;
            }
            match std::fs::create_dir(&stage) {
                Ok(()) => Some(Ok((stage, published))),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(e) => Some(Err(e)),
            }
        })
        .context("无法分配导出目录")??;
    let cleanup = Staging(stage.clone());
    let full_path = published.display().to_string();
    // Windows 的长路径前缀用于磁盘操作；界面与资源管理器使用普通路径。
    let directory = if let Some(path) = full_path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{path}")
    } else {
        full_path
            .strip_prefix(r"\\?\")
            .unwrap_or(&full_path)
            .to_string()
    };
    let mut result = MarkdownExport {
        directory,
        total: articles.len(),
        with_body: 0,
        missing_body: 0,
        partial_images: 0,
    };
    let nickname = match biz {
        Some(biz) => source_name(store, biz)?,
        None => "所选文章".into(),
    };
    let mut index = format!(
        "# {}\n\n共 {} 篇已下载文章。\n\n",
        plain(&nickname),
        articles.len()
    );
    let mut html_index = format!(
        "<h1>{}</h1><p>共 {} 篇已下载文章，同时提供 Markdown 和 HTML。</p><ul>",
        escaped(&nickname),
        articles.len()
    );
    let mut manifest = Vec::new();
    let image_name = regex::Regex::new(r"^[a-f0-9]+\.(png|jpg|gif|webp|avif)$")?;
    let image_link =
        regex::Regex::new(r"!\[[^\]]*\]\((images/[a-f0-9]+\.(?:png|jpg|gif|webp|avif))\)")?;
    for row in articles {
        let article_nickname = source_name(store, &row.biz)?;
        let target = stage.join("articles").join(row.id.to_string());
        std::fs::create_dir_all(&target)?;
        let record = store.wb_archive(row.id)?;
        let mut state = "missing_body";
        let mut missing_images = 0;
        let mut markdown = None;
        if let Some(record) = record.filter(|r| ["complete", "partial"].contains(&r.state.as_str()))
        {
            let dir = root.join("archive/articles").join(row.id.to_string());
            // 任一层链接都不跟随；只拷贝已知正文和图片，不导出原始响应或保护材料。
            for ancestor in [
                root.to_path_buf(),
                root.join("archive"),
                root.join("archive/articles"),
                dir.clone(),
            ] {
                local_dir(&ancestor)?;
            }
            ensure!(
                Path::new(&record.directory).canonicalize()? == dir.canonicalize()?,
                "归档目录不匹配"
            );
            let md = dir.join("article.md");
            if md.exists() {
                local_regular(&md)?;
                markdown = Some(std::fs::read_to_string(md)?);
                state = if record.state == "complete" {
                    "complete"
                } else {
                    "partial"
                };
                missing_images = record.missing_images.len();
                let images = dir.join("images");
                if images.exists() {
                    local_dir(&images)?;
                    for entry in std::fs::read_dir(images)? {
                        let entry = entry?;
                        local_regular(&entry.path())?;
                        let name = entry.file_name();
                        ensure!(
                            image_name.is_match(&name.to_string_lossy()),
                            "归档图片文件名异常"
                        );
                        std::fs::create_dir_all(target.join("images"))?;
                        std::fs::copy(entry.path(), target.join("images").join(name))?;
                    }
                }
                for cap in image_link.captures_iter(markdown.as_deref().unwrap_or_default()) {
                    ensure!(
                        target.join(&cap[1]).is_file(),
                        "本地图片缺失，请先补齐归档后重新导出"
                    );
                }
            }
        }
        if markdown.as_ref().is_none_or(|s| s.trim().is_empty()) {
            markdown = store.get_article_md(row.id)?.and_then(|(a, m)| {
                if a.is_deleted == 0 {
                    m.filter(|s| !s.trim().is_empty())
                } else {
                    None
                }
            });
            if markdown.is_some() {
                state = "stored_markdown";
            }
        }
        let has_body = markdown.as_ref().is_some_and(|s| !s.trim().is_empty());
        ensure!(has_body, "文章正文已缺失，请重新下载后导出");
        result.with_body += 1;
        if state == "partial" {
            result.partial_images += 1;
        }
        let title = plain(row.title.as_deref().unwrap_or("无标题"));
        let url = source_url(row.content_url.as_deref().unwrap_or_default());
        let published_at = row
            .published_at
            .and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0))
            .map(|d| d.to_rfc3339())
            .unwrap_or_else(|| "未知".into());
        let mut doc = format!(
            "# {title}\n\n- 公众号：{}\n- 作者：{}\n- 发布时间：{published_at}\n- 原文：{url}\n\n",
            plain(&article_nickname),
            plain(row.author.as_deref().unwrap_or("未知"))
        );
        if state == "stored_markdown" {
            doc.push_str("> 已有本地正文，图片可能仍为远程链接。\n\n");
        }
        if state == "partial" {
            doc.push_str(&format!(
                "> 正文已保存，仍有 {missing_images} 张图片未下载。\n\n"
            ));
        }
        doc.push_str(
            markdown
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("> 正文尚未下载或不可用。请在软件中下载正文后重新导出。"),
        );
        super::archive::atomic_write(&target.join("article.md"), doc.as_bytes())?;
        super::archive::atomic_write(
            &target.join("article.html"),
            article_html(&title, &doc, &target)?.as_bytes(),
        )?;
        html_index.push_str(&format!("<li>{} · <a href=\"articles/{}/article.html\">HTML</a> · <a href=\"articles/{}/article.md\">Markdown</a>{}</li>", escaped(&title), row.id, row.id, if state == "partial" { "（正文已保存，缺图）" } else { "" }));
        index.push_str(&format!(
            "- [{title}](articles/{}/article.md) · {}\n",
            row.id,
            if has_body {
                if state == "partial" {
                    "正文已保存，缺图"
                } else {
                    "正文已保存"
                }
            } else {
                "正文缺失"
            }
        ));
        manifest.push(json!({"article_id":row.id,"biz":row.biz,"nickname":article_nickname,"title":row.title,"author":row.author,"published_at":row.published_at,"source_url":url,"state":state,"missing_images":missing_images,"markdown":format!("articles/{}/article.md",row.id),"html":format!("articles/{}/article.html",row.id)}));
    }
    html_index.push_str("</ul>");
    super::archive::atomic_write(
        &stage.join("index.html"),
        html_shell(&nickname, &html_index).as_bytes(),
    )?;
    super::archive::atomic_write(&stage.join("index.md"), index.as_bytes())?;
    super::archive::atomic_write(
        &stage.join("manifest.json"),
        &serde_json::to_vec_pretty(
            &json!({"format_version":2,"formats":["markdown","html"],"created_at":now(),"source":{"biz":biz,"nickname":nickname},"summary":result,"articles":manifest}),
        )?,
    )?;
    // 复制期间用户可能删除文章；发布前重新核对整个固定集合。
    validate_selection(store, None, &export_ids)?;
    ensure!(!published.exists(), "导出目录已存在，请重试");
    std::fs::rename(&stage, &published)?;
    drop(cleanup);
    store.system_log("export","info","文章 Markdown 和 HTML 导出完成",json!({"biz":biz,"count":result.total,"completed":result.with_body,"partial":result.partial_images,"formats":["markdown","html"]}))?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{workbench::ArchiveRecord, ArticleFields};

    #[test]
    fn export_requires_all_bodies_and_preserves_local_images_and_legacy_markdown() {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_account("FAKE", Some("虚构公众号 / CON"), None)
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "mpider-md-test-{}-{}",
            std::process::id(),
            (now() * 1e6) as u64
        ));
        let output = root.join("中文导出路径");
        std::fs::create_dir_all(&output).unwrap();
        for n in 0..76 {
            store.upsert_article("FAKE", &n.to_string(),1,&ArticleFields{
                title:Some(format!("标题: * / CON {n}")),
                content_md: if n==1 {Some("旧库正文".into())} else {None},
                content_url:Some("https://mp.weixin.qq.com/s?__biz=FAKE&mid=1&key=SECRET&pass_ticket=PRIVATE&uin=ID".into()),
                ..Default::default()
            }).unwrap();
        }
        store
            .upsert_article("OTHER", "1", 1, &ArticleFields::default())
            .unwrap();
        let id = store.list_articles(Some("FAKE"), -1, 0, false).unwrap()[0].id;
        let local = root.join("workbench/archive/articles").join(id.to_string());
        std::fs::create_dir_all(local.join("images")).unwrap();
        std::fs::write(
            local.join("article.md"),
            "本地正文\n\n![图](images/abc.png)",
        )
        .unwrap();
        std::fs::write(local.join("images/abc.png"), b"synthetic-image").unwrap();
        std::fs::write(local.join("original.html"), "不应导出原始HTML").unwrap();
        store
            .wb_save_archive(&ArchiveRecord {
                article_id: id,
                state: "partial".into(),
                directory: local.display().to_string(),
                missing_images: vec!["https://example.test/missing.png".into()],
                updated_at: now(),
                error: None,
            })
            .unwrap();
        std::fs::write(output.join("user-file.md"), "保留用户文件").unwrap();
        assert!(export_source(&store, &root.join("workbench"), "FAKE", &output).is_err());
        assert_eq!(std::fs::read_dir(&output).unwrap().count(), 1);
        for n in 0..76 {
            store
                .upsert_article(
                    "FAKE",
                    &n.to_string(),
                    1,
                    &ArticleFields {
                        content_md: Some("已下载正文".into()),
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        let first = export_source(&store, &root.join("workbench"), "FAKE", &output).unwrap();
        assert_eq!(first.total, 76);
        assert_eq!(first.with_body, 76);
        assert_eq!(first.missing_body, 0);
        assert_eq!(first.partial_images, 1);
        let dir = Path::new(&first.directory);
        assert_eq!(
            std::fs::read(dir.join(format!("articles/{id}/images/abc.png"))).unwrap(),
            b"synthetic-image"
        );
        assert!(!dir.join(format!("articles/{id}/original.html")).exists());
        let html =
            std::fs::read_to_string(dir.join(format!("articles/{id}/article.html"))).unwrap();
        assert!(html.contains("本地正文"));
        assert!(html.contains("src=\"images/abc.png\""));
        assert!(html.contains("1 张图片未下载"));
        assert!(!html.contains("SECRET"));
        assert!(!html.contains("PRIVATE"));
        let html_index = std::fs::read_to_string(dir.join("index.html")).unwrap();
        assert!(html_index.contains(&format!("articles/{id}/article.html")));
        assert!(html_index.contains(&format!("articles/{id}/article.md")));
        let manifest = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
        assert!(!manifest.contains("SECRET"));
        assert!(!manifest.contains("PRIVATE"));
        let value: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(value["format_version"], 2);
        assert_eq!(value["formats"], json!(["markdown", "html"]));
        assert_eq!(value["articles"].as_array().unwrap().len(), 76);
        assert!(value["articles"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["state"] != "missing_body"));
        assert!(value["articles"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| dir.join(a["html"].as_str().unwrap()).is_file()));
        let second = export_source(&store, &root.join("workbench"), "FAKE", &output).unwrap();
        assert_ne!(first.directory, second.directory);
        assert_eq!(
            std::fs::read_to_string(output.join("user-file.md")).unwrap(),
            "保留用户文件"
        );
        assert!(store.wb_requests(0.).unwrap().is_empty());
        // 错误的归档路径不能导出，也不遗留本次临时产物。
        let before = std::fs::read_dir(&output).unwrap().count();
        let mut bad = store.wb_archive(id).unwrap().unwrap();
        bad.directory = output.display().to_string();
        store.wb_save_archive(&bad).unwrap();
        assert!(export_source(&store, &root.join("workbench"), "FAKE", &output).is_err());
        assert_eq!(std::fs::read_dir(&output).unwrap().count(), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn html_renders_markdown_and_blocks_active_content_and_external_images() {
        let root = std::env::temp_dir().join(format!(
            "mpider-html-test-{}-{}",
            std::process::id(),
            (now() * 1e6) as u64
        ));
        std::fs::create_dir_all(root.join("images")).unwrap();
        std::fs::write(root.join("images/abc.png"), b"synthetic").unwrap();
        let markdown = r#"# 标题

**粗体**与[原文](https://mp.weixin.qq.com/s?mid=1&KEY=PRIVATE_KEY&pass_ticket=PRIVATE_TICKET#PRIVATE_FRAGMENT)

|列甲|列乙|
|---|---|
|正文|内容|

```rust
let value = "<script>";
```

![本地图](images/abc.png)
![远程图](https://example.test/pixel.png)
<IMG SRC='images/abc.png' srcset='https://example.test/track.png 2x' onerror='PRIVATE_HANDLER'>
<img src='../../private/secret.png'>
<script>PRIVATE_SCRIPT</script><iframe src='https://example.test/frame'></iframe>
<a href='javascript:PRIVATE_JS'>无效链接</a><input autofocus onfocus='PRIVATE_FOCUS'>
"#;
        let html = article_html("<测试 & 标题>", markdown, &root).unwrap();
        assert!(html.contains("<title>&lt;测试 &amp; 标题&gt;</title>"));
        assert!(html.contains("<strong>粗体</strong>"));
        assert!(html.contains("<table>"));
        assert!(html.contains("<code"));
        let parsed = Html::parse_document(&html);
        let images: Vec<_> = parsed.select(&Selector::parse("img").unwrap()).collect();
        assert_eq!(images.len(), 2);
        assert!(images
            .iter()
            .all(|img| img.value().attr("src") == Some("images/abc.png")));
        assert!(images
            .iter()
            .all(|img| img.value().attr("srcset").is_none()));
        assert!(html.contains("图片未保存在本地"));
        assert!(!html.contains("PRIVATE_"));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<iframe"));
        assert!(html.contains("Content-Security-Policy"));
        assert!(html.contains("default-src 'none'"));
        assert!(html.contains("https://mp.weixin.qq.com/s?mid=1"));
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
#[test]
fn selected_export_deduplicates_cross_source_ids_and_rejects_deleted_rows_before_writing() {
    use crate::ArticleFields;
    let store = Store::open_in_memory().unwrap();
    store.upsert_account("A", Some("虚构甲"), None).unwrap();
    store.upsert_account("B", Some("虚构乙"), None).unwrap();
    let a = store
        .upsert_article(
            "A",
            "1",
            1,
            &ArticleFields {
                content_md: Some("甲正文".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let b = store
        .upsert_article("B", "2", 1, &ArticleFields::default())
        .unwrap();
    let root = std::env::temp_dir().join(format!(
        "mpider-selected-test-{}-{}",
        std::process::id(),
        (now() * 1e6) as u64
    ));
    std::fs::create_dir_all(&root).unwrap();
    let before = std::fs::read_dir(&root).unwrap().count();
    assert!(validate_selection(&store, None, &[a, b]).is_err());
    assert!(export_articles(&store, &root.join("workbench"), &[a, b], &root).is_err());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), before);
    store
        .upsert_article(
            "B",
            "2",
            1,
            &ArticleFields {
                content_md: Some("乙正文".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let result = export_articles(&store, &root.join("workbench"), &[a, b, a], &root).unwrap();
    assert_eq!(result.total, 2);
    assert_eq!(result.with_body, 2);
    assert_eq!(result.missing_body, 0);
    let doc = std::fs::read_to_string(
        Path::new(&result.directory).join(format!("articles/{a}/article.md")),
    )
    .unwrap();
    assert!(doc.contains("公众号：虚构甲"));
    assert!(!doc.contains("公众号：所选文章"));
    store.wb_delete_articles(vec![b]).unwrap();
    let before = std::fs::read_dir(&root).unwrap().count();
    assert!(export_articles(&store, &root.join("workbench"), &[a, b], &root).is_err());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), before);
    std::fs::remove_dir_all(root).unwrap();
}
