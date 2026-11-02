//! 将本地文章集合同时导出为 Markdown 与经过清洗的离线 HTML；不执行网络请求。
use super::export_html;
#[cfg(test)]
use crate::model::now;
use crate::{ArticleRow, Store};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
pub struct MarkdownExport {
    pub directory: String,
    pub total: usize,
    pub with_body: usize,
    pub missing_body: usize,
    pub partial_images: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ExportProgress {
    pub completed: usize,
    pub total: usize,
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
pub(super) fn source_url(value: &str) -> String {
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

pub(super) fn escaped(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
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
    export_source_with_progress(store, root, biz, destination, &|_| {})
}

pub fn export_source_with_progress(
    store: &Store,
    root: &Path,
    biz: &str,
    destination: &Path,
    on_progress: &dyn Fn(ExportProgress),
) -> Result<MarkdownExport> {
    validate_selection(store, Some(biz), &[])?;
    let articles = store.list_articles(Some(biz), -1, 0, false)?;
    export_rows(store, root, articles, Some(biz), destination, on_progress)
}

pub fn export_articles(
    store: &Store,
    root: &Path,
    ids: &[i64],
    destination: &Path,
) -> Result<MarkdownExport> {
    export_articles_with_progress(store, root, ids, destination, &|_| {})
}

pub fn export_articles_with_progress(
    store: &Store,
    root: &Path,
    ids: &[i64],
    destination: &Path,
    on_progress: &dyn Fn(ExportProgress),
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
    export_rows(store, root, articles, None, destination, on_progress)
}
fn export_rows(
    store: &Store,
    root: &Path,
    mut articles: Vec<ArticleRow>,
    biz: Option<&str>,
    destination: &Path,
    on_progress: &dyn Fn(ExportProgress),
) -> Result<MarkdownExport> {
    ensure!(destination.is_absolute(), "请选择绝对路径的导出目录");
    let destination = destination.canonicalize().context("导出目录不存在")?;
    ensure!(destination.is_dir(), "请选择文件夹");
    ensure!(!articles.is_empty(), "没有可导出的已入库文章");
    let export_ids: Vec<i64> = articles.iter().map(|article| article.id).collect();
    articles.sort_by(|a, b| {
        b.published_at
            .partial_cmp(&a.published_at)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.id.cmp(&b.id))
    });
    let nickname = match biz {
        Some(biz) => source_name(store, biz)?,
        None => "所选文章".into(),
    };
    let years: Vec<_> = articles
        .iter()
        .filter_map(|a| {
            a.published_at
                .and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0))
                .map(|d| (d + chrono::Duration::hours(8)).format("%Y").to_string())
        })
        .collect();
    let span = match (years.iter().min(), years.iter().max()) {
        (Some(a), Some(b)) if a == b => format!("_{a}"),
        (Some(a), Some(b)) => format!("_{a}-{b}"),
        _ => String::new(),
    };
    let stem = format!("{}{span}", export_html::safe_name(&nickname, 40));
    let (stage, published) = (0..1000)
        .find_map(|n| {
            let published = destination.join(if n == 0 {
                stem.clone()
            } else {
                format!("{stem}_{n}")
            });
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
    std::fs::create_dir_all(stage.join("articles"))?;
    std::fs::create_dir_all(stage.join("images"))?;
    on_progress(ExportProgress {
        completed: 0,
        total: result.total,
    });
    let image_name = regex::Regex::new(r"^[a-f0-9]+\.(png|jpg|gif|webp|avif)$")?;
    let md_image =
        regex::Regex::new(r"!\[[^\]]*\]\((images/[a-f0-9]+\.(?:png|jpg|gif|webp|avif))\)")?;
    let remote_md_image = regex::Regex::new(r"!\[[^\]]*\]\((https?://[^\s)]+)\)")?;
    let any_image = regex::Regex::new(r"(!\[[^\]]*\]\()([^\s)]+)(\))")?;
    let mut rows = String::new();
    let mut manifest = String::from("\u{feff}sn,标题,发布时间,原文链接,类型,正文状态\r\n");
    let mut dates = Vec::new();
    for (seq, row) in articles.into_iter().enumerate() {
        let article_nickname = source_name(store, &row.biz)?;
        let mut markdown = None;
        let mut images = HashMap::new();
        let mut raw = store.wb_cached_html(row.id)?;
        let mut partial = false;
        let record = store.wb_archive(row.id)?;
        if let Some(record) = record.filter(|r| ["complete", "partial"].contains(&r.state.as_str()))
        {
            let dir = root.join("archive/articles").join(row.id.to_string());
            // A database copied from another computer may have no matching archive on this machine.
            // In that case use the stored body; never follow the foreign absolute path.
            if dir.exists() {
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
                }
                if raw.as_ref().is_none_or(|s| s.trim().is_empty())
                    && dir.join("original.html").exists()
                {
                    local_regular(&dir.join("original.html"))?;
                    raw = Some(std::fs::read_to_string(dir.join("original.html"))?);
                }
                partial = !record.missing_images.is_empty() || record.state == "partial";
                let source_images = dir.join("images");
                if source_images.exists() {
                    local_dir(&source_images)?;
                    for entry in std::fs::read_dir(source_images)? {
                        let entry = entry?;
                        local_regular(&entry.path())?;
                        let name = entry.file_name().to_string_lossy().into_owned();
                        ensure!(image_name.is_match(&name), "归档图片文件名异常");
                        let bytes = std::fs::read(entry.path())?;
                        let ext = entry
                            .path()
                            .extension()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned();
                        let file = format!("{:x}.{ext}", Sha256::digest(&bytes));
                        let target = stage.join("images").join(&file);
                        if !target.exists() {
                            super::archive::atomic_write(&target, &bytes)?;
                        }
                        images.insert(format!("images/{name}"), format!("../images/{file}"));
                    }
                }
            }
        }
        if markdown.as_ref().is_none_or(|s| s.trim().is_empty()) {
            markdown =
                store
                    .get_article_md(row.id)?
                    .and_then(|(a, m)| if a.is_deleted == 0 { m } else { None });
        }
        let markdown = markdown
            .filter(|m| !m.trim().is_empty())
            .context("文章正文已缺失，请重新下载后导出")?;
        for cap in md_image.captures_iter(&markdown) {
            ensure!(
                images.contains_key(&cap[1]),
                "本地图片缺失，请先补齐归档后重新导出"
            );
        }
        // Remote Markdown links can use a different size/query from the original data-src.
        for cap in remote_md_image.captures_iter(&markdown) {
            if let Some(local) = export_html::local_image(&cap[1], &images) {
                images.insert(export_html::image_key(&cap[1]), local);
            }
        }
        let kind = export_html::article_type(raw.as_deref());
        let title = row
            .title
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("无标题");
        let file = format!("{:04}_{}", seq + 1, export_html::safe_name(title, 60));
        let url = source_url(row.content_url.as_deref().unwrap_or_default());
        let published = row
            .published_at
            .and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0))
            .map(|d| {
                (d + chrono::Duration::hours(8))
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_default();
        if published.len() >= 10 {
            dates.push(published[..10].to_string());
        }
        let original_body = raw
            .as_deref()
            .and_then(|html| export_html::body(html, &images));
        let mut body = original_body.clone().unwrap_or_else(|| {
            let mut html = String::new();
            pulldown_cmark::html::push_html(
                &mut html,
                pulldown_cmark::Parser::new_ext(
                    &markdown,
                    pulldown_cmark::Options::ENABLE_TABLES
                        | pulldown_cmark::Options::ENABLE_STRIKETHROUGH
                        | pulldown_cmark::Options::ENABLE_TASKLISTS,
                ),
            );
            export_html::clean(&html, &images)
        });
        partial |= body.contains("［图片未保存在本地］");
        if partial {
            result.partial_images += 1;
        }
        if body.trim().is_empty() {
            body = "<p>正文暂无可显示内容，请查看 Markdown 或原文。</p>".into();
        }
        let mut md = match original_body {
            Some(ref body) => htmd::convert(body).context("无法生成文章 Markdown")?,
            None => markdown.clone(),
        };
        // Replace only image destinations, keeping normal links and code untouched.
        md = any_image
            .replace_all(&md, |cap: &regex::Captures<'_>| {
                let target = export_html::local_image(&cap[2], &images)
                    .unwrap_or_else(|| cap[2].to_string());
                format!("{}{target}{}", &cap[1], &cap[3])
            })
            .into_owned();
        let doc = format!(
            "# {}\n\n- 公众号：{}\n- 发布时间：{published}\n- 原文：{url}\n\n{}{}\n\n---\n\n{}\n",
            plain(title),
            plain(&article_nickname),
            if partial {
                "> 正文已保存，部分图片未保存在本地。\n\n"
            } else {
                ""
            },
            md,
            include_str!("export_templates/promotion.md").trim()
        );
        super::archive::atomic_write(
            &stage.join("articles").join(format!("{file}.md")),
            doc.as_bytes(),
        )?;
        super::archive::atomic_write(
            &stage.join("articles").join(format!("{file}.html")),
            export_html::article(title, &article_nickname, &published, &url, &kind, &body)
                .as_bytes(),
        )?;
        let mut link = url::Url::parse("https://export.invalid/")?;
        link.path_segments_mut()
            .unwrap()
            .push("articles")
            .push(&format!("{file}.html"));
        let href = link.path().to_string();
        let md_href = href.strip_suffix(".html").unwrap().to_string() + ".md";
        rows.push_str(&format!("<li data-k=\"{}\"><span class=\"d\">{}</span><span class=\"ti\"><a href=\"{}\">{}</a></span><span class=\"ops\"><a class=\"small\" href=\"{}\">md</a> <a class=\"small\" href=\"{}\" target=\"_blank\" rel=\"noopener\">原文</a></span></li>\n", escaped(&title.to_lowercase()), escaped(published.get(..10).unwrap_or_default()), escaped(href.trim_start_matches('/')), escaped(title), escaped(md_href.trim_start_matches('/')), escaped(&url)));
        let sn = store.wb_article_sn(row.id)?.unwrap_or_default();
        manifest.push_str(
            &[sn.as_str(), title, &published, &url, &kind, "成功"]
                .map(export_html::csv)
                .join(","),
        );
        manifest.push_str("\r\n");
        result.with_body += 1;
        on_progress(ExportProgress {
            completed: result.with_body,
            total: result.total,
        });
    }
    dates.sort();
    let range = dates
        .first()
        .zip(dates.last())
        .map(|(a, b)| format!("{a} ~ {b}"))
        .unwrap_or_default();
    let summary = format!(
        "共 {} 篇{} · 成功 {}",
        result.total,
        if range.is_empty() {
            String::new()
        } else {
            format!(" · {range}")
        },
        result.with_body
    );
    let disclaimer =
        "文章仅供个人学习、研究和收藏使用，版权归原作者及公众号所有。请勿用于商业用途或公开传播。";
    let generated = (chrono::Utc::now() + chrono::Duration::hours(8))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let index = export_html::template(
        include_str!("export_templates/index.html"),
        &[
            ("account", &escaped(&nickname)),
            ("summary", &escaped(&summary)),
            ("rows", &rows),
            ("generated", &generated),
            ("disclaimer", disclaimer),
        ],
    );
    super::archive::atomic_write(&stage.join("index.html"), index.as_bytes())?;
    super::archive::atomic_write(&stage.join("manifest.csv"), manifest.as_bytes())?;
    let readme = format!("\u{feff}{nickname} · 公众号文章\r\n共 {} 篇，{range}\r\n\r\n【目录说明】\r\n  index.html      双击用浏览器打开，按发布时间倒序浏览，可搜索标题。\r\n  articles/       每篇文章一个 .html 和一个 .md，文件名格式：序号_标题。\r\n  images/         共享的本地图片，请与 articles/ 保持相对位置。\r\n  manifest.csv    文章清单：sn、标题、发布时间、原文链接、类型、正文状态。\r\n\r\n视频、音乐等保留原文观看或收听提示。未保存的图片显示提示，不自动联网加载。\r\n本次 {} 篇文章存在缺图；可在软件中补齐图片后重新导出。\r\n\r\n【如何另存为 PDF】\r\n用浏览器打开文章，按 Ctrl+P（Mac 为 Command+P），选择另存为 PDF；建议勾选背景图形。\r\n\r\n【版权说明】\r\n{disclaimer}\r\n", result.total, result.partial_images);
    super::archive::atomic_write(&stage.join("README.txt"), readme.as_bytes())?;
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
    use scraper::{Html, Selector};

    fn temp(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mpider-export-{name}-{}-{}",
            std::process::id(),
            (now() * 1e6) as u64
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn export_requires_all_bodies_and_uses_reference_layout_with_shared_images() {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_account("FAKE", Some("虚构公众号"), None)
            .unwrap();
        let root = temp("layout");
        let output = root.join("导出");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join("user-file"), "保留").unwrap();
        for n in 0..76 {
            store.upsert_article("FAKE", &n.to_string(), 1, &ArticleFields {
                title: Some(format!("标题: * / CON {n}")),
                content_md: if n == 0 { None } else { Some("旧库正文".into()) },
                content_url: Some("https://mp.weixin.qq.com/s?mid=1&key=SECRET&pass_ticket=PRIVATE&uin=ID".into()),
                published_at: Some(1792885307.),
                ..Default::default()
            }).unwrap();
        }
        assert!(export_source(&store, &root.join("workbench"), "FAKE", &output).is_err());
        assert_eq!(std::fs::read_dir(&output).unwrap().count(), 1);
        let id = store.upsert_article("FAKE", "0", 1, &ArticleFields {
            content_md: Some("本地正文\n\n![图](images/abc.png)".into()),
            content_html: Some(r#"<script>SECRET_RAW</script><div id="js_content" style="visibility:hidden"><section style="text-align:center;color:rgb(1,2,3)"><strong>原始排版正文</strong><img data-src="https://example.test/photo" style="width:45%"></section></div>"#.into()),
            ..Default::default()
        }).unwrap();
        let archive = root.join("workbench/archive/articles").join(id.to_string());
        std::fs::create_dir_all(archive.join("images")).unwrap();
        let hash = format!("{:x}", Sha256::digest(b"https://example.test/photo"));
        std::fs::write(
            archive.join(format!("images/{hash}.png")),
            b"synthetic-image",
        )
        .unwrap();
        std::fs::write(archive.join("images/abc.png"), b"synthetic-image").unwrap();
        std::fs::write(
            archive.join("article.md"),
            "本地正文\n\n![图](images/abc.png)",
        )
        .unwrap();
        std::fs::write(archive.join("original.html"), "PRIVATE_ORIGINAL").unwrap();
        store
            .wb_save_archive(&ArchiveRecord {
                article_id: id,
                state: "complete".into(),
                directory: archive.display().to_string(),
                missing_images: vec![],
                updated_at: now(),
                error: None,
            })
            .unwrap();
        let progress = std::cell::RefCell::new(Vec::new());
        let result = export_source_with_progress(
            &store,
            &root.join("workbench"),
            "FAKE",
            &output,
            &|update| {
                let stage = std::fs::read_dir(&output)
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .find(|p| p.to_string_lossy().ends_with(".tmp"))
                    .unwrap();
                let html = std::fs::read_dir(stage.join("articles"))
                    .unwrap()
                    .filter(|e| {
                        e.as_ref()
                            .unwrap()
                            .path()
                            .extension()
                            .is_some_and(|e| e == "html")
                    })
                    .count();
                assert_eq!(html, update.completed);
                progress.borrow_mut().push(update);
            },
        )
        .unwrap();
        assert_eq!(
            *progress.borrow(),
            (0..=76)
                .map(|completed| ExportProgress {
                    completed,
                    total: 76
                })
                .collect::<Vec<_>>()
        );
        assert_eq!(result.total, 76);
        assert_eq!(result.partial_images, 0);
        let dir = Path::new(&result.directory);
        let mut entries = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(
            entries,
            [
                "README.txt",
                "articles",
                "images",
                "index.html",
                "manifest.csv"
            ]
        );
        assert_eq!(
            std::fs::read_dir(dir.join("articles")).unwrap().count(),
            152
        );
        assert_eq!(std::fs::read_dir(dir.join("images")).unwrap().count(), 1);
        let file = dir.join("articles/0001_标题_ _ _ CON 0.html");
        let html = std::fs::read_to_string(file).unwrap();
        assert!(html.contains("原始排版正文"));
        assert!(html.contains("text-align: center"));
        assert!(html.contains("width: 45%"));
        assert!(html.contains("../index.html"));
        assert!(!html.contains("SECRET"));
        assert!(!html.contains("PRIVATE"));
        let md = std::fs::read_to_string(dir.join("articles/0001_标题_ _ _ CON 0.md")).unwrap();
        assert!(md.contains("原始排版正文"));
        assert!(md.contains("../images/"));
        let index = std::fs::read_to_string(dir.join("index.html")).unwrap();
        assert!(index.contains("id=\"q\""));
        assert!(index.contains("共 76 篇"));
        let csv = std::fs::read_to_string(dir.join("manifest.csv")).unwrap();
        assert_eq!(csv.lines().count(), 77);
        assert!(!csv.contains("SECRET"));
        assert!(store.wb_requests(0.).unwrap().is_empty());
        let second = export_source(&store, &root.join("workbench"), "FAKE", &output).unwrap();
        assert_ne!(second.directory, result.directory);
        assert_eq!(
            std::fs::read_to_string(output.join("user-file")).unwrap(),
            "保留"
        );
        let before = std::fs::read_dir(&output).unwrap().count();
        let mut bad = store.wb_archive(id).unwrap().unwrap();
        bad.directory = output.display().to_string();
        store.wb_save_archive(&bad).unwrap();
        assert!(export_source(&store, &root.join("workbench"), "FAKE", &output).is_err());
        assert_eq!(std::fs::read_dir(&output).unwrap().count(), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn html_preserves_styles_and_backgrounds_and_blocks_active_content() {
        let images = HashMap::from([
            ("images/abc.png".into(), "../images/abc.png".into()),
            ("https://example.test/bg".into(), "../images/bg.png".into()),
        ]);
        let raw = r#"<script>PRIVATE_SCRIPT</script><div id="js_content"><section style="text-align:center; color:red; visibility:hidden; opacity:0; background-image:url('https://example.test/bg'); padding:12px"><strong>粗体</strong><span style="font-size:24px">大字</span><img data-src="images/abc.png" srcset="https://track.test/pixel" onerror="PRIVATE_HANDLER" style="width:45%"><img src="../../private/secret.png"><iframe src="https://track.test/frame"></iframe><a href="https://mp.weixin.qq.com/s?mid=1&amp;KEY=PRIVATE_KEY&amp;pass_ticket=PRIVATE_TICKET#PRIVATE_FRAGMENT">链接</a><a href="javascript:PRIVATE_JS">无效</a><form><input autofocus onfocus="PRIVATE_FOCUS"></form><div style="background:url('https://track.test/pixel'); position:fixed">正文</div></section></div>"#;
        let body = export_html::body(raw, &images).unwrap();
        let html = export_html::article("<测试 & 标题>", "虚构号", "", "", "文章", &body);
        assert!(html.contains("<title>&lt;测试 &amp; 标题&gt;</title>"));
        assert!(html.contains("text-align: center"));
        assert!(html.contains("font-size: 24px"));
        assert!(html.contains("../images/bg.png"));
        assert!(!html.contains("visibility"));
        assert!(!html.contains("opacity"));
        assert!(!html.contains("PRIVATE_"));
        assert!(!html.contains("track.test"));
        assert!(!html.contains("<script"));
        assert!(!html.contains("<iframe"));
        assert!(!html.contains("<input"));
        assert!(!html.contains("position: fixed"));
        assert!(html.contains("https://mp.weixin.qq.com/s?mid=1"));
        assert!(html.contains("图片未保存在本地"));
        let doc = Html::parse_document(&html);
        let imgs = doc
            .select(&Selector::parse("img").unwrap())
            .collect::<Vec<_>>();
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].value().attr("src"), Some("../images/abc.png"));
        assert!(imgs[0].value().attr("srcset").is_none());
        assert!(html.contains("Content-Security-Policy"));
        let legacy_url = "https://mmbiz.qpic.cn/mmbiz_jpg/FAKE/640";
        let legacy_hash = format!("{:x}", Sha256::digest(legacy_url.as_bytes()));
        let legacy_images = HashMap::from([(
            format!("images/{legacy_hash}.jpg"),
            "../images/local.jpg".into(),
        )]);
        assert_eq!(
            export_html::local_image(
                "https://mmbiz.qpic.cn/mmbiz_jpg/FAKE/0?wx_fmt=jpeg#imgIndex=1",
                &legacy_images
            )
            .as_deref(),
            Some("../images/local.jpg")
        );
        let audio = export_html::clean(
            r#"<mp-common-mpaudio name="夜读&amp;nbsp;|&amp;nbsp;&amp;lt;标题&amp;gt;"></mp-common-mpaudio>"#,
            &images,
        );
        assert_eq!(
            Html::parse_fragment(&audio)
                .root_element()
                .text()
                .collect::<String>(),
            "[音频] 夜读\u{a0}|\u{a0}<标题> 请在原文中收听"
        );
        assert!(!audio.contains("&amp;nbsp;"));
    }

    #[test]
    fn selected_export_deduplicates_ids_handles_foreign_cache_and_escapes_file_links() {
        let store = Store::open_in_memory().unwrap();
        store.upsert_account("A", Some("虚构甲"), None).unwrap();
        store.upsert_account("B", Some("虚构乙"), None).unwrap();
        let a = store
            .upsert_article(
                "A",
                "1",
                1,
                &ArticleFields {
                    title: Some("标题#与%空 格.html".into()),
                    content_md: Some("甲正文".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let b = store
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
        store
            .wb_save_archive(&ArchiveRecord {
                article_id: a,
                state: "complete".into(),
                directory: "C:\\foreign\\archive".into(),
                missing_images: vec![],
                updated_at: now(),
                error: None,
            })
            .unwrap();
        let root = temp("selected");
        let result = export_articles(&store, &root.join("workbench"), &[a, b, a], &root).unwrap();
        assert_eq!(result.total, 2);
        let index =
            std::fs::read_to_string(Path::new(&result.directory).join("index.html")).unwrap();
        assert!(index.contains("%23"));
        assert!(index.contains("%25"));
        assert!(index.contains(".html.md"));
        let md = std::fs::read_to_string(
            Path::new(&result.directory).join("articles/0001_标题#与%空 格.html.md"),
        )
        .unwrap();
        assert!(md.contains("公众号：虚构甲"));
        assert!(md.contains("甲正文"));
        store.wb_delete_articles(vec![b]).unwrap();
        let before = std::fs::read_dir(&root).unwrap().count();
        assert!(export_articles(&store, &root.join("workbench"), &[a, b], &root).is_err());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), before);
        std::fs::remove_dir_all(root).unwrap();
    }
}
