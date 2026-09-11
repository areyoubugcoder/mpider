//! 限流探测工具（detail_probe）：对公开 /s 页做**严格串行**的间隔阶梯测试，
//! 量化微信对无凭证抓取的限流阈值（多少篇 / 什么间隔触发验证页、多久恢复）。
//!
//! 与 GUI 的批量补详情共用同一套判定（article-md 的 VerifyPage/Unavailable），
//! 但**不写库**——只读 articles 表拿 URL 池，结果落 CSV，纯测量不影响数据。
//!
//! 用法（典型跑法）：
//! ```text
//! # 单发探测：当前 IP 是否处于限流态（exit 0=正常，2=限流）
//! cargo run -p mpider-core --example detail_probe -- --db <db> --probe
//!
//! # 间隔阶梯：依次按 10s/5s/3s/2s 各跑至多 40 篇；限流则等恢复再进下一档
//! cargo run -p mpider-core --example detail_probe -- --db <db> \
//!     --staircase 10000,5000,3000,2000 --phase-max 40 --csv probe.csv
//! ```

use std::io::Write as _;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

/// 与 GUI 批量补详情一致的 UA（detail.rs）。
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36";

/// 单次请求的判定结果。
#[derive(Debug, Clone, PartialEq)]
enum Outcome {
    /// 正常文章页（含回退纯文本的情况），附正文字符数。
    Ok(usize),
    /// 微信人机验证页 —— 限流信号。
    Verify,
    /// 文章本身不可用（已删除/违规），不算限流。
    Unavailable,
    /// HTTP/网络错误，附说明。
    Err(String),
}

impl Outcome {
    fn tag(&self) -> &'static str {
        match self {
            Outcome::Ok(_) => "OK",
            Outcome::Verify => "VERIFY",
            Outcome::Unavailable => "UNAVAIL",
            Outcome::Err(_) => "ERR",
        }
    }
}

struct Args {
    db: String,
    probe: bool,
    interval_ms: Option<u64>,
    staircase: Vec<u64>,
    phase_max: usize,
    recover_probe_s: u64,
    max_recover_min: u64,
    csv: Option<String>,
    /// 只用已成功抓过详情的 URL（排除坏链假阳性，测纯 IP 限流）。
    only_done: bool,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        db: String::new(),
        probe: false,
        interval_ms: None,
        staircase: Vec::new(),
        phase_max: 40,
        recover_probe_s: 300,
        max_recover_min: 120,
        csv: None,
        only_done: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut val = || it.next().context(format!("参数 {k} 缺少取值"));
        match k.as_str() {
            "--db" => a.db = val()?,
            "--probe" => a.probe = true,
            "--interval-ms" => a.interval_ms = Some(val()?.parse()?),
            "--staircase" => {
                a.staircase = val()?
                    .split(',')
                    .map(|s| s.trim().parse::<u64>())
                    .collect::<Result<_, _>>()?;
            }
            "--phase-max" => a.phase_max = val()?.parse()?,
            "--recover-probe-s" => a.recover_probe_s = val()?.parse()?,
            "--max-recover-min" => a.max_recover_min = val()?.parse()?,
            "--csv" => a.csv = Some(val()?),
            "--only-done" => a.only_done = true,
            other => bail!("未知参数：{other}"),
        }
    }
    if a.db.is_empty() {
        bail!("必须提供 --db <mpider.db 路径>");
    }
    Ok(a)
}

/// 只读打开库，取 URL 池：未补详情的排前面，其后是已完成的（限流测试可重复抓）。
fn load_url_pool(db: &str, only_done: bool) -> Result<Vec<String>> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("打不开库：{db}"))?;
    // only_done：只取已成功抓过详情的（URL 已验证有效），排除坏链（无效 sn 的短链
    // 会稳定返回验证页/未知错误，混进池里会被误判成 IP 限流）。
    let sql = if only_done {
        "SELECT content_url FROM articles \
         WHERE is_deleted=0 AND content_url IS NOT NULL AND content_url != '' \
           AND detail_done = 1 \
         ORDER BY id ASC"
    } else {
        "SELECT content_url FROM articles \
         WHERE is_deleted=0 AND content_url IS NOT NULL AND content_url != '' \
           AND detail_done >= 0 \
         ORDER BY detail_done ASC, id ASC"
    };
    let mut stmt = conn.prepare(sql)?;
    let urls: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    if urls.is_empty() {
        bail!("articles 表里没有可用的 content_url");
    }
    Ok(urls)
}

/// 当前时刻（东八区 HH:MM:SS + epoch 秒），供日志/CSV。
fn now() -> (u64, String) {
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let t = epoch + 8 * 3600;
    let hms = format!("{:02}:{:02}:{:02}", t / 3600 % 24, t / 60 % 60, t % 60);
    (epoch, hms)
}

/// 抓一篇并判定（直连）。
async fn fetch_classify(client: &reqwest::Client, url: &str) -> (Outcome, u128) {
    let started = Instant::now();
    let resp = client.get(url).send().await;
    let elapsed = started.elapsed().as_millis();
    let html = match resp {
        Ok(r) => {
            let status = r.status();
            if !status.is_success() {
                return (Outcome::Err(format!("HTTP {status}")), elapsed);
            }
            match r.text().await {
                Ok(t) => t,
                Err(e) => return (Outcome::Err(format!("读响应失败:{e}")), elapsed),
            }
        }
        Err(e) => return (Outcome::Err(format!("请求失败:{e}")), elapsed),
    };
    let out = match article_md::parse_html(url, &html) {
        Err(article_md::ParseError::VerifyPage) => Outcome::Verify,
        Err(article_md::ParseError::Unavailable(_)) => Outcome::Unavailable,
        Ok(p) => Outcome::Ok(p.markdown.map(|m| m.chars().count()).unwrap_or(0)),
    };
    (out, elapsed)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;
    let pool = load_url_pool(&args.db, args.only_done)?;
    println!("URL 池 {} 条（未补详情的排前）", pool.len());

    // 与 detail.rs 相同的 client 配置：同 UA、no_proxy、15s 超时。
    let client = reqwest::Client::builder()
        .user_agent(UA)
        .timeout(Duration::from_secs(15))
        .no_proxy()
        .build()?;

    let mut csv: Option<std::fs::File> = match &args.csv {
        Some(p) => {
            let new = !std::path::Path::new(p).exists();
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)?;
            if new {
                writeln!(
                    f,
                    "epoch,hms,phase_interval_ms,seq,outcome,elapsed_ms,detail"
                )?;
            }
            Some(f)
        }
        None => None,
    };
    let mut cursor = 0usize; // URL 池游标（循环复用）
    let record = |csv: &mut Option<std::fs::File>,
                  interval: u64,
                  seq: usize,
                  out: &Outcome,
                  elapsed: u128| {
        let (epoch, hms) = now();
        let detail = match out {
            Outcome::Ok(n) => format!("{n}字"),
            Outcome::Err(e) => e.replace(',', ";"),
            _ => String::new(),
        };
        println!(
            "[{hms}] 间隔{interval}ms #{seq} {} {elapsed}ms {detail}",
            out.tag()
        );
        if let Some(f) = csv {
            let _ = writeln!(
                f,
                "{epoch},{hms},{interval},{seq},{},{elapsed},{detail}",
                out.tag()
            );
        }
    };

    // ---- 单发探测模式 ----
    if args.probe {
        let url = &pool[0];
        let (out, elapsed) = fetch_classify(&client, url).await;
        record(&mut csv, 0, 0, &out, elapsed);
        std::process::exit(if out == Outcome::Verify { 2 } else { 0 });
    }

    // ---- 阶梯 / 单档模式 ----
    let phases: Vec<u64> = if !args.staircase.is_empty() {
        args.staircase.clone()
    } else {
        vec![args
            .interval_ms
            .context("需要 --interval-ms 或 --staircase 或 --probe")?]
    };

    for (pi, interval) in phases.iter().copied().enumerate() {
        let (_, hms) = now();
        println!(
            "\n===== 阶段 {}/{}：间隔 {interval}ms，至多 {} 篇（{hms}）=====",
            pi + 1,
            phases.len(),
            args.phase_max
        );
        let mut ok = 0usize;
        let mut seq = 0usize;
        let mut limited = false;
        while ok < args.phase_max {
            seq += 1;
            let url = &pool[cursor % pool.len()];
            cursor += 1;
            let (out, elapsed) = fetch_classify(&client, url).await;
            record(&mut csv, interval, seq, &out, elapsed);
            match out {
                Outcome::Verify => {
                    limited = true;
                    break;
                }
                Outcome::Ok(_) => ok += 1,
                // 不可用/出错不计入成功，也不视为限流，继续。
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(interval)).await;
        }
        println!(
            "===== 阶段 {}/{} 结束：成功 {ok} 篇，共发 {seq} 请求，{}=====",
            pi + 1,
            phases.len(),
            if limited {
                "触发限流 ⛔"
            } else {
                "未触发限流 ✅"
            }
        );

        // 限流 → 等恢复（周期单发探测），恢复才进下一档；超时则终止全程。
        if limited && pi + 1 < phases.len() {
            let wait_started = Instant::now();
            loop {
                if wait_started.elapsed() > Duration::from_secs(args.max_recover_min * 60) {
                    println!("恢复等待超过 {} 分钟仍限流，终止。", args.max_recover_min);
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_secs(args.recover_probe_s)).await;
                let url = &pool[cursor % pool.len()];
                cursor += 1;
                let (out, elapsed) = fetch_classify(&client, url).await;
                record(&mut csv, u64::MAX, 0, &out, elapsed); // interval=MAX 标记恢复探测
                if out != Outcome::Verify {
                    let mins = wait_started.elapsed().as_secs() / 60;
                    println!("✅ 限流已恢复（等待约 {mins} 分钟），进入下一档。");
                    break;
                }
            }
        }
    }
    println!("\n全部阶段完成。");
    Ok(())
}
