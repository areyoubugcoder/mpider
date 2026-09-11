//! 用本地 HTML 文件验证解析：`cargo run -p article-md --example parse_file -- <url> <file>`
//! 输出解析结果概要或错误分类（VerifyPage / Unavailable）。

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(url), Some(path)) = (args.next(), args.next()) else {
        eprintln!("用法: parse_file <url> <html文件>");
        std::process::exit(2);
    };
    let html = std::fs::read_to_string(&path).expect("读文件失败");
    match article_md::parse_html(&url, &html) {
        Ok(r) => {
            println!("OK title={:?} mp={:?} biz={:?}", r.title, r.mp_name, r.biz);
            println!(
                "markdown={} chars, images={}",
                r.markdown
                    .as_deref()
                    .map(|m| m.chars().count())
                    .unwrap_or(0),
                r.images.len()
            );
        }
        Err(e) => println!("ERR {e:?}"),
    }
}
