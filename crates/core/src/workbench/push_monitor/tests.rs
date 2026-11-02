use super::*;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn link(biz: &str) -> Link {
    normalize(&format!("https://mp.weixin.qq.com/s?__biz={biz}&mid=100&idx=2&sn=0123456789abcdef0123456789abcdef&chksm=0123456789abcdef")).unwrap()
}
fn html(biz: &str) -> String {
    format!(
        r#"<html><head><meta property="og:title" content="虚构文章" /></head><body><script type="text/javascript">
window.__allowLoadResFromMp = true;
var biz = "{biz}" || "";
var mid = "100" || "";
var idx = "2" || "";
var sn = "0123456789abcdef0123456789abcdef" || "";
var hd_head_img = "";
var nickname = htmlDecode("测试号");
var ct = "{}";
</script><div id="js_content" class="rich_media_content"><p>虚构正文</p></div></body></html>"#,
        now() as i64
    )
}
// 既有解析回归显式开启两个独立运行；首次运行编号恰好一致。
fn start_both(store: &Store, config: Config) -> i64 {
    let scan_id = store.push_start(config).unwrap();
    let queue_id = store.push_queue_start().unwrap();
    assert_eq!(scan_id, queue_id);
    queue_id
}
fn setup(config: Config) -> (Store, i64, i64) {
    let store = Store::open_in_memory().unwrap();
    let id = start_both(&store, config);
    store
        .push_scanned(
            id,
            Scan {
                links: vec![link("FAKE_A")],
                processes: 1,
                ..Default::default()
            },
            now(),
        )
        .unwrap();
    let candidate = store.push_next(id, now()).unwrap().unwrap().id;
    (store, id, candidate)
}

#[test]
fn identity_includes_biz_and_input_drops_session_tracking_parameters() {
    assert_ne!(link("FAKE_A").key, link("FAKE_B").key);
    let raw = format!(
        "{}&key=secret&pass_ticket=secret#tracking",
        link("FAKE_A").url
    );
    assert_eq!(normalize(&raw), Some(link("FAKE_A")));
    for raw in [
        "https://mp.weixin.qq.com.evil/s/1234567890123456789012",
        "https://user@mp.weixin.qq.com/s/1234567890123456789012",
        "https://mp.weixin.qq.com:1234/s/1234567890123456789012",
    ] {
        assert!(normalize(raw).is_none());
    }
    assert!(normalize(&format!("{}&mid=200", link("FAKE_A").url)).is_none());
    assert!(normalize(
        &link("FAKE_A")
            .url
            .replace("0123456789abcdef0123456789abcdef", "bad")
    )
    .is_none());
}

#[test]
fn long_checksum_survives_discovery_and_requeues_failed_candidate_without_resetting_attempts() {
    let checksum = "ab".repeat(38);
    let raw = format!("{}&key=discard-me", link("FAKE_A").url);
    let raw = raw.replace("chksm=0123456789abcdef", &format!("chksm={checksum}"));
    let signed = normalize(&raw).unwrap();
    assert!(signed.url.contains(&format!("chksm={checksum}")));
    assert!(!signed.url.contains("key="));
    let utf16: Vec<u8> = raw.encode_utf16().flat_map(u16::to_le_bytes).collect();
    for bytes in [raw.as_bytes(), utf16.as_slice()] {
        assert_eq!(discover(bytes), vec![signed.clone()]);
    }
    let store = Store::open_in_memory().unwrap();
    let id = start_both(&store, Config::default());
    let mut unsigned = link("FAKE_A");
    unsigned.url = unsigned.url.split("&chksm=").next().unwrap().to_owned();
    store
        .push_scanned(
            id,
            Scan {
                links: vec![unsigned],
                ..Default::default()
            },
            now(),
        )
        .unwrap();
    let candidate = store.push_next(id, now()).unwrap().unwrap().id;
    store.push_claim(id, candidate).unwrap();
    store
        .push_result(id, candidate, "http_error", false, 0.)
        .unwrap();
    store
        .push_scanned(
            id,
            Scan {
                links: vec![signed.clone()],
                ..Default::default()
            },
            now(),
        )
        .unwrap();
    let next = store.push_next(id, now()).unwrap().unwrap();
    assert_eq!(next.id, candidate);
    assert_eq!(next.attempts, 1);
    assert_eq!(next.url, signed.url);
    assert_eq!(
        store.push_events(None, 100).unwrap().items[0]
            .reason
            .as_deref(),
        Some("link_updated")
    );
    assert_eq!(store.push_status().unwrap().requests_24h, 0);
}

#[test]
fn legacy_monitor_config_drops_local_limits_and_policy_gets_daily_budget() {
    let config: Config =
        serde_json::from_str(r#"{"hourly_budget":18,"fetch_seconds":30}"#).unwrap();
    let value = serde_json::to_value(config).unwrap();
    assert!(value.get("hourly_budget").is_none());
    assert!(value.get("fetch_seconds").is_none());
    let policy: crate::workbench::Policy = serde_json::from_str(r#"{"body_gap_ms":2500}"#).unwrap();
    assert_eq!(policy.body_gap_ms, 2500);
    assert_eq!(policy.body_daily_budget, 86400);
    assert!(crate::workbench::Policy {
        body_daily_budget: 0,
        ..Default::default()
    }
    .validate()
    .is_err());
}

#[tokio::test]
async fn queued_webpage_is_fetched_while_memory_scan_is_still_running() {
    struct WaitingScanner(Arc<AtomicBool>);
    impl Scanner for WaitingScanner {
        fn scan(&mut self, cancel: &AtomicBool) -> Result<Scan> {
            self.0.store(true, Ordering::Relaxed);
            while !cancel.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(Scan::default())
        }
    }
    let store = Store::open_in_memory().unwrap();
    let id = start_both(&store, Config::default());
    store
        .push_scanned(
            id,
            Scan {
                links: vec![link("FAKE_A")],
                ..Default::default()
            },
            now() - 20.,
        )
        .unwrap();
    let store = Arc::new(store);
    let entered = Arc::new(AtomicBool::new(false));
    let (endpoint, server) = fixture(200, html("FAKE_A")).await;
    let scan_task = tokio::spawn(run_with_scanner(
        store.clone(),
        id,
        WaitingScanner(entered.clone()),
    ));
    let clone = store.clone();
    let task = tokio::spawn(async move { run_queue_at(clone, id, Some(&endpoint)).await });
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !entered.load(Ordering::Relaxed) || store.push_status().unwrap().imported != 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(store.push_run().unwrap().rounds, 1);
    store.push_end(id, "stopped", None).unwrap();
    assert!(store.push_queue_is_active(id).unwrap());
    store.push_queue_end(id, "paused", None).unwrap();
    tokio::time::timeout(Duration::from_secs(3), scan_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(store.push_status().unwrap().requests_24h, 1);
}

#[test]
fn detects_ascii_utf16_xml_and_escaped_links_without_bare_path_noise() {
    let input = link("FAKE_A").url.replace('&', "&amp;");
    let utf16: Vec<u8> = input.encode_utf16().flat_map(u16::to_le_bytes).collect();
    for bytes in [
        input.into_bytes(),
        utf16,
        link("FAKE_A").url.replace('/', "\\/").into_bytes(),
    ] {
        let found = discover(&bytes);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, link("FAKE_A").key);
    }
    assert!(discover(b"garbage/s/1234567890123456789012").is_empty());
}

#[test]
fn webpage_identity_and_risk_are_validated() {
    assert!(!verification_redirect(
        &link("FAKE_A"),
        "/s/verify1234567890123456"
    ));
    let good = metadata(&link("FAKE_A"), &html("FAKE_A")).unwrap();
    assert_eq!(good.idx, 2);
    assert!(metadata(&link("FAKE_A"), &html("FAKE_B")).is_err());
    assert!(
        metadata(&link("FAKE_A"), "<html>环境异常 完成验证后</html>")
            .unwrap_err()
            .to_string()
            .contains("verification_required")
    );
    assert!(metadata(&link("FAKE_A"), "<html>未知页面</html>").is_err());
    assert!(metadata(
        &link("FAKE_A"),
        &html("FAKE_A").replace("虚构正文", "环境异常 安全验证")
    )
    .is_ok());
}

#[test]
fn stop_restart_preserves_queue_and_rejects_late_results() {
    let (store, id, candidate) = setup(Config::default());
    store.push_claim(id, candidate).unwrap();
    store.push_restart().unwrap();
    let meta = metadata(&link("FAKE_A"), &html("FAKE_A")).unwrap();
    store.push_import(id, candidate, &meta, now()).unwrap();
    assert!(store.list_accounts().unwrap().is_empty());
    let next = store.push_queue_start().unwrap();
    assert_ne!(next, id);
    assert_eq!(store.push_next(next, now()).unwrap().unwrap().id, candidate);
    store.push_import(id, candidate, &meta, now()).unwrap();
    assert!(store.list_accounts().unwrap().is_empty());
    store.push_claim(next, candidate).unwrap();
    store.push_import(next, candidate, &meta, now()).unwrap();
    assert_eq!(store.push_status().unwrap().imported, 1);
}

#[test]
fn event_article_and_account_commit_atomically_and_never_touch_sessions() {
    let path = std::env::temp_dir().join(format!(
        "mpider-push-atomic-{}-{}.db",
        std::process::id(),
        (now() * 1000000.) as u64
    ));
    let store = Store::open(&path).unwrap();
    let id = start_both(&store, Config::default());
    store
        .push_scanned(
            id,
            Scan {
                links: vec![link("FAKE_A")],
                ..Default::default()
            },
            now(),
        )
        .unwrap();
    let candidate = store.push_next(id, now()).unwrap().unwrap().id;
    let meta = metadata(&link("FAKE_A"), &html("FAKE_A")).unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    store.push_claim(id, candidate).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_push_log BEFORE INSERT ON app_log WHEN NEW.stage='push_monitor' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
    assert!(store.push_import(id, candidate, &meta, now()).is_err());
    assert!(store.list_accounts().unwrap().is_empty());
    assert_eq!(
        store.push_events(None, 100).unwrap().items[0].outcome,
        "fetching"
    );
    connection
        .execute_batch("DROP TRIGGER fail_push_log")
        .unwrap();
    store.push_import(id, candidate, &meta, now()).unwrap();
    assert!(store.wb_sources().unwrap().is_empty());
    assert_eq!(store.list_articles(None, 10, 0, false).unwrap().len(), 1);
    let text = serde_json::to_string(&store.push_status().unwrap()).unwrap();
    assert!(!text.contains("chksm"));
    assert!(!text.contains("https://"));
    drop(connection);
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[test]
fn across_accounts_imports_are_distinct_and_preexisting_articles_are_reused() {
    let (store, id, candidate) = setup(Config::default());
    let a = metadata(&link("FAKE_A"), &html("FAKE_A")).unwrap();
    store
        .upsert_account("FAKE_A", Some("旧名称"), None)
        .unwrap();
    let article = store
        .upsert_article(
            "FAKE_A",
            "100",
            2,
            &crate::model::ArticleFields {
                content_md: Some("已有正文".into()),
                ..Default::default()
            },
        )
        .unwrap();
    store.push_claim(id, candidate).unwrap();
    store.push_import(id, candidate, &a, now()).unwrap();
    assert_eq!(
        store.push_events(None, 100).unwrap().items[0].article_id,
        Some(article)
    );
    assert_eq!(
        store.push_events(None, 100).unwrap().items[0].outcome,
        "existing"
    );
    store
        .push_scanned(
            id,
            Scan {
                links: vec![link("FAKE_B")],
                ..Default::default()
            },
            now(),
        )
        .unwrap();
    let b = store.push_next(id, now()).unwrap().unwrap();
    store.push_claim(id, b.id).unwrap();
    store
        .push_import(
            id,
            b.id,
            &metadata(&link("FAKE_B"), &html("FAKE_B")).unwrap(),
            now(),
        )
        .unwrap();
    assert_eq!(store.list_articles(None, 10, 0, false).unwrap().len(), 2);
    assert!(store.list_articles(Some("FAKE_A"), 10, 0, false).unwrap()[0].has_body);
}

#[test]
fn deleted_accounts_and_outside_window_are_not_recreated() {
    let (store, id, candidate) = setup(Config::default());
    let mut meta = metadata(&link("FAKE_A"), &html("FAKE_A")).unwrap();
    store.push_claim(id, candidate).unwrap();
    meta.published_at -= 25 * 3600;
    store.push_import(id, candidate, &meta, now()).unwrap();
    assert_eq!(
        store.push_events(None, 100).unwrap().items[0].outcome,
        "ignored"
    );
    assert!(store.list_accounts().unwrap().is_empty());
    let (store, id, candidate) = setup(Config::default());
    meta.published_at = now() as i64;
    store
        .upsert_account("FAKE_A", Some("删除号"), None)
        .unwrap();
    store
        .wb_delete_source("FAKE_A", &std::env::temp_dir())
        .unwrap();
    store.push_claim(id, candidate).unwrap();
    store.push_import(id, candidate, &meta, now()).unwrap();
    assert_eq!(
        store.push_events(None, 100).unwrap().items[0].outcome,
        "ignored"
    );
    assert!(store.list_articles(None, 10, 0, false).unwrap().is_empty());
}

fn article_budget(store: &Store, budget: i64) {
    let mut policy = store.wb_policy().unwrap();
    policy.body_daily_budget = budget;
    store.wb_set_policy(policy).unwrap();
}
#[test]
fn article_daily_budget_survives_runs_and_is_shared_with_downloads() {
    let (store, id, _) = setup(Config::default());
    article_budget(&store, 1);
    let ts = now();
    assert!(store
        .wb_request_permit(CATEGORY, PURPOSE, id, "FAKE_A", ts)
        .unwrap()
        .request_id
        .is_some());
    store.push_queue_end(id, "paused", None).unwrap();
    let next = store.push_queue_start().unwrap();
    for category in [CATEGORY, "body"] {
        let purpose = if category == CATEGORY {
            PURPOSE
        } else {
            "archive"
        };
        let denied = store
            .wb_request_permit(category, purpose, next, "FAKE_A", ts + 3601.)
            .unwrap();
        assert!(denied.request_id.is_none());
        assert!(denied.next_at >= ts + 86400.);
    }
    assert!(store
        .wb_request_permit(CATEGORY, PURPOSE, next, "FAKE_A", ts + 86401.)
        .unwrap()
        .request_id
        .is_some());
}
#[test]
fn article_interval_is_shared_and_live_policy_change_releases_waiting_monitor() {
    let (store, id, _) = setup(Config::default());
    let mut policy = store.wb_policy().unwrap();
    policy.body_gap_ms = 30000;
    policy.body_workers = 1;
    policy.body_daily_budget = 2;
    store.wb_set_policy(policy).unwrap();
    let ts = now();
    let first = store
        .wb_request_permit("body", "archive", 1, "FAKE_A", ts)
        .unwrap()
        .request_id
        .unwrap();
    store
        .wb_request_finish(&first, "ok", json!({}), ts + 2.)
        .unwrap();
    let denied = store
        .wb_request_permit(CATEGORY, PURPOSE, id, "FAKE_A", ts + 1.)
        .unwrap();
    assert!(denied.request_id.is_none());
    assert_eq!(denied.next_at, ts + 32.);
    assert!(store
        .wb_request_permit("list", "history", 1, "FAKE_A", ts + 1.)
        .unwrap()
        .request_id
        .is_some());
    assert!(store
        .wb_request_permit("image", "archive", 1, "FAKE_A", ts + 1.)
        .unwrap()
        .request_id
        .is_some());
    let second = store
        .wb_request_permit(CATEGORY, PURPOSE, id, "FAKE_A", ts + 32.)
        .unwrap()
        .request_id
        .unwrap();
    store
        .wb_request_finish(&second, "ok", json!({}), ts + 33.)
        .unwrap();
    let denied = store
        .wb_request_permit("body", "archive", 1, "FAKE_A", ts + 60.)
        .unwrap();
    assert!(denied.request_id.is_none());
    assert!(denied.next_at >= ts + 86400.);
    store
        .push_deferred(id, denied.next_at, denied.reason.as_deref())
        .unwrap();
    let mut policy = store.wb_policy().unwrap();
    policy.body_gap_ms = 5000;
    policy.body_daily_budget = 3;
    store.wb_set_policy(policy).unwrap();
    assert_eq!(store.push_run().unwrap().queue.next_fetch_at, 0.);
    assert!(store
        .wb_request_permit(CATEGORY, PURPOSE, id, "FAKE_A", ts + 38.)
        .unwrap()
        .request_id
        .is_some());
    assert_eq!(store.wb_article_window(ts - 1.).unwrap().0, 3);
    assert_eq!(store.wb_snapshot().unwrap().article_calls_24h, 3);
}

async fn fixture(code: u16, body: String) -> (String, tokio::task::JoinHandle<String>) {
    fixture_with_location(code, body, None).await
}
async fn fixture_with_location(
    code: u16,
    body: String,
    location: Option<String>,
) -> (String, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![0u8; 8192];
        let n = socket.read(&mut request).await.unwrap();
        let location = location
            .map(|value| format!("Location: {value}\r\n"))
            .unwrap_or_default();
        let head=format!("HTTP/1.1 {code} Test\r\nContent-Length: {}\r\nContent-Type: text/html; charset=utf-8\r\n{location}Connection: close\r\n\r\n",body.len());
        socket.write_all(head.as_bytes()).await.unwrap();
        socket.write_all(body.as_bytes()).await.unwrap();
        String::from_utf8_lossy(&request[..n]).into_owned()
    });
    (format!("http://{address}/s"), task)
}

#[tokio::test]
async fn article_redirect_is_durable_prioritized_and_each_hop_is_counted() {
    let (store, id, candidate_id) = setup(Config::default());
    let (endpoint, server) = fixture_with_location(
        302,
        String::new(),
        Some(format!("{}&key=discard-me", link("FAKE_A").url)),
    )
    .await;
    process_at(
        &store,
        &client().unwrap(),
        id,
        store.push_next(id, now()).unwrap().unwrap(),
        &endpoint,
    )
    .await
    .unwrap();
    server.await.unwrap();
    assert_eq!(store.push_status().unwrap().requests_24h, 1);
    store
        .push_scanned(
            id,
            Scan {
                links: vec![link("FAKE_B")],
                ..Default::default()
            },
            now() + 1.,
        )
        .unwrap();
    let next = store.push_next(id, now() + 2.).unwrap().unwrap();
    assert_eq!(next.id, candidate_id);
    assert!(!next.url.contains("key="));
    tokio::time::sleep(Duration::from_millis(1050)).await;
    let (endpoint, server) = fixture(200, html("FAKE_A")).await;
    process_at(&store, &client().unwrap(), id, next, &endpoint)
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(store.push_status().unwrap().requests_24h, 2);
    assert_eq!(store.push_status().unwrap().imported, 1);
    assert!(store
        .wb_snapshot()
        .unwrap()
        .requests
        .iter()
        .any(|r| r.outcome == "redirect"));
}

#[tokio::test]
async fn redirect_waits_for_daily_article_budget_after_reopen_without_hidden_http() {
    let (store, id, _) = setup(Config::default());
    article_budget(&store, 1);
    let (endpoint, server) =
        fixture_with_location(302, String::new(), Some(link("FAKE_A").url)).await;
    process_at(
        &store,
        &client().unwrap(),
        id,
        store.push_next(id, now()).unwrap().unwrap(),
        &endpoint,
    )
    .await
    .unwrap();
    server.await.unwrap();
    store.push_restart().unwrap();
    let next_run = store.push_queue_start().unwrap();
    let candidate = store.push_next(next_run, now()).unwrap().unwrap();
    process_at(
        &store,
        &client().unwrap(),
        next_run,
        candidate,
        "http://127.0.0.1:1/not-sent",
    )
    .await
    .unwrap();
    assert_eq!(store.push_status().unwrap().requests_24h, 1);
    assert_eq!(store.push_status().unwrap().pending, 1);
    assert!(store.push_run().unwrap().queue.next_fetch_at > now() + 86300.);
}

#[tokio::test]
async fn redirect_to_other_domain_or_other_known_article_never_gets_followed() {
    for target in [
        "https://example.invalid/s/1234567890123456789012".into(),
        link("FAKE_B").url,
    ] {
        let (store, id, _) = setup(Config::default());
        let (endpoint, server) = fixture_with_location(302, String::new(), Some(target)).await;
        process_at(
            &store,
            &client().unwrap(),
            id,
            store.push_next(id, now()).unwrap().unwrap(),
            &endpoint,
        )
        .await
        .unwrap();
        server.await.unwrap();
        assert_eq!(store.push_status().unwrap().requests_24h, 1);
        assert!(store.push_next(id, now()).unwrap().is_none());
        assert_eq!(
            store.push_events(None, 100).unwrap().items[0].outcome,
            "redirect_rejected"
        );
    }
}

#[tokio::test]
async fn redirect_loop_stops_after_three_counted_requests() {
    let (store, id, _) = setup(Config::default());
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(1050)).await;
        }
        let (endpoint, server) =
            fixture_with_location(302, String::new(), Some(link("FAKE_A").url)).await;
        process_at(
            &store,
            &client().unwrap(),
            id,
            store.push_next(id, now()).unwrap().unwrap(),
            &endpoint,
        )
        .await
        .unwrap();
        server.await.unwrap();
    }
    assert_eq!(store.push_status().unwrap().requests_24h, 3);
    assert_eq!(
        store.push_events(None, 100).unwrap().items[0].outcome,
        "redirect_limit"
    );
    assert!(store.push_next(id, now()).unwrap().is_none());
}

#[tokio::test]
async fn short_redirect_cannot_replace_original_full_article_identity() {
    let (store, id, _) = setup(Config::default());
    let (endpoint, server) = fixture_with_location(
        302,
        String::new(),
        Some("https://mp.weixin.qq.com/s/1234567890123456789012".into()),
    )
    .await;
    process_at(
        &store,
        &client().unwrap(),
        id,
        store.push_next(id, now()).unwrap().unwrap(),
        &endpoint,
    )
    .await
    .unwrap();
    server.await.unwrap();
    tokio::time::sleep(Duration::from_millis(1050)).await;
    let (endpoint, server) = fixture(200, html("FAKE_B")).await;
    process_at(
        &store,
        &client().unwrap(),
        id,
        store.push_next(id, now()).unwrap().unwrap(),
        &endpoint,
    )
    .await
    .unwrap();
    server.await.unwrap();
    assert!(store.list_accounts().unwrap().is_empty());
    assert_eq!(
        store.push_events(None, 100).unwrap().items[0]
            .reason
            .as_deref(),
        Some("invalid_metadata")
    );
}

#[tokio::test]
async fn verification_redirect_is_not_followed_and_retries_after_one_second() {
    let (store, id, _) = setup(Config::default());
    let (endpoint, server) = fixture_with_location(
        302,
        String::new(),
        Some("https://mp.weixin.qq.com/mp/verify?token=synthetic".into()),
    )
    .await;
    process_at(
        &store,
        &client().unwrap(),
        id,
        store.push_next(id, now()).unwrap().unwrap(),
        &endpoint,
    )
    .await
    .unwrap();
    server.await.unwrap();
    assert_eq!(store.push_status().unwrap().requests_24h, 1);
    assert_eq!(
        store.push_events(None, 100).unwrap().items[0]
            .reason
            .as_deref(),
        Some("verification_required")
    );
    assert_eq!(store.wb_policy().unwrap().cooldown_until, 0.);
    assert!(store.push_next(id, now() + 1.1).unwrap().is_some());
    assert!(store.push_next(id, now()).unwrap().is_none());
}

#[tokio::test]
async fn real_http_failures_retry_soon_without_global_cooldown() {
    for (code, body, expected) in [
        (200, html("FAKE_A"), "imported"),
        (200, "<html>环境异常 完成验证后</html>".into(), "pending"),
        (429, String::new(), "pending"),
        (503, String::new(), "pending"),
    ] {
        let (store, id, _) = setup(Config::default());
        let candidate = store.push_next(id, now()).unwrap().unwrap();
        let (endpoint, server) = fixture(code, body).await;
        process_at(&store, &client().unwrap(), id, candidate, &endpoint)
            .await
            .unwrap();
        let request = server.await.unwrap();
        assert!(request.starts_with("GET /s HTTP/1.1"));
        let status = store.push_status().unwrap();
        assert_eq!(
            store.push_events(None, 100).unwrap().items[0].outcome,
            expected
        );
        assert_eq!(status.requests_24h, 1);
        if code == 429
            || store.push_events(None, 100).unwrap().items[0]
                .reason
                .as_deref()
                == Some("verification_required")
        {
            assert_eq!(store.wb_policy().unwrap().cooldown_until, 0.);
            assert!(store.push_next(id, now() + 1.1).unwrap().is_some());
        }
        assert_eq!(status.imported, i64::from(expected == "imported"));
    }
}

#[tokio::test]
async fn stop_during_http_does_not_commit_late_reply() {
    let (store, id, _) = setup(Config::default());
    let store = Arc::new(store);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/s", listener.local_addr().unwrap());
    let candidate = store.push_next(id, now()).unwrap().unwrap();
    let clone = store.clone();
    let task = tokio::spawn(async move {
        process_at(&clone, &client().unwrap(), id, candidate, &endpoint).await
    });
    let (socket, _) = listener.accept().await.unwrap();
    store.push_queue_end(id, "paused", None).unwrap();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(socket);
    assert!(store.list_accounts().unwrap().is_empty());
    assert_eq!(store.push_status().unwrap().pending, 1);
    let requests = store.wb_snapshot().unwrap().requests;
    assert_eq!(requests[0].outcome, "cancelled");
    assert_eq!(requests[0].owner_kind, "push_monitor");
    assert_eq!(
        json!(store.push_status().unwrap())["run"]["queue"]["active"],
        false
    );
}

#[test]
fn persisted_queue_and_request_budget_survive_database_reopen() {
    let path = std::env::temp_dir().join(format!(
        "mpider-push-reopen-{}-{}.db",
        std::process::id(),
        (now() * 1000000.) as u64
    ));
    {
        let store = Store::open(&path).unwrap();
        article_budget(&store, 1);
        let id = start_both(&store, Config::default());
        store
            .push_scanned(
                id,
                Scan {
                    links: vec![link("FAKE_A")],
                    ..Default::default()
                },
                now(),
            )
            .unwrap();
        let candidate = store.push_next(id, now()).unwrap().unwrap();
        store
            .wb_request_permit(CATEGORY, PURPOSE, id, "FAKE_A", now())
            .unwrap();
        store.push_claim(id, candidate.id).unwrap();
    }
    {
        let store = Store::open(&path).unwrap();
        store.push_restart().unwrap();
        assert!(!store.push_status().unwrap().run.active);
        assert!(!store.push_status().unwrap().run.queue.active);
        let id = store.push_queue_start().unwrap();
        assert_eq!(store.push_next(id, now()).unwrap().unwrap().attempts, 1);
        assert!(store
            .wb_request_permit(CATEGORY, PURPOSE, id, "FAKE_A", now())
            .unwrap()
            .request_id
            .is_none());
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn fresh_candidates_take_priority_over_backlog_and_retrying_links() {
    let (store, id, old) = setup(Config::default());
    store.push_claim(id, old).unwrap();
    store
        .push_result(id, old, "network_error", true, 0.)
        .unwrap();
    store
        .push_scanned(
            id,
            Scan {
                links: vec![link("FAKE_B")],
                ..Default::default()
            },
            now() + 1.,
        )
        .unwrap();
    assert_ne!(store.push_next(id, now() + 2.).unwrap().unwrap().id, old);
}

#[tokio::test]
async fn scanner_stop_is_cooperative_and_partial_results_are_discarded() {
    struct WaitingScanner(Arc<AtomicBool>);
    impl Scanner for WaitingScanner {
        fn scan(&mut self, cancel: &AtomicBool) -> Result<Scan> {
            self.0.store(true, Ordering::Relaxed);
            while !cancel.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(Scan {
                links: vec![link("FAKE_A")],
                ..Default::default()
            })
        }
    }
    let store = Arc::new(Store::open_in_memory().unwrap());
    let id = store.push_start(Config::default()).unwrap();
    let entered = Arc::new(AtomicBool::new(false));
    let task = tokio::spawn(run_with_scanner(
        store.clone(),
        id,
        WaitingScanner(entered.clone()),
    ));
    tokio::time::timeout(Duration::from_secs(3), async {
        while !entered.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    store.push_end(id, "stopped", None).unwrap();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(store.push_events(None, 100).unwrap().items.is_empty());
}

#[test]
fn recent_discoveries_are_hour_bounded_and_keyset_pages_do_not_skip_or_repeat() {
    let store = Store::open_in_memory().unwrap();
    let id = start_both(&store, Config::default());
    let ts = now();
    store
        .push_scanned(
            id,
            Scan {
                links: vec![link("OLD")],
                ..Default::default()
            },
            ts - 7200.,
        )
        .unwrap();
    let old = store.push_next(id, ts).unwrap().unwrap().id;
    store.push_claim(id, old).unwrap();
    // 即使旧记录刚刚处理完成，也不能重新进入最近一小时的发现列表。
    store
        .push_result(id, old, "unavailable", false, 0.)
        .unwrap();
    store
        .push_scanned(
            id,
            Scan {
                links: (0..110).map(|i| link(&format!("FAKE_{i}"))).collect(),
                ..Default::default()
            },
            ts - 20.,
        )
        .unwrap();
    let first = store.push_events(None, 20).unwrap();
    assert_eq!(first.items.len(), 20);
    assert!(first.items.iter().all(|e| e.id != old));
    let mut ids: Vec<i64> = first.items.iter().map(|e| e.id).collect();
    let mut cursor = first.next_cursor;
    // 翻页期间出现新记录，后续页仍沿第一次返回的游标继续。
    store
        .push_scanned(
            id,
            Scan {
                links: vec![link("NEW")],
                ..Default::default()
            },
            now(),
        )
        .unwrap();
    while let Some(before) = cursor {
        let page = store.push_events(Some(before), 20).unwrap();
        ids.extend(page.items.iter().map(|e| e.id));
        cursor = page.next_cursor;
    }
    assert_eq!(ids.len(), 110);
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        110
    );
    assert_eq!(store.push_events(None, 20).unwrap().items[0].biz, "NEW");
    assert!(store.push_events(None, 0).is_err());
    assert!(store
        .push_events(
            Some(EventCursor {
                first_seen_at: f64::NAN,
                id: 1
            }),
            20
        )
        .is_err());
}

#[test]
fn configured_article_retry_count_applies_to_push_candidates() {
    let (store, id, candidate) = setup(Config::default());
    let mut policy = store.wb_policy().unwrap();
    policy.retries = 0;
    store.wb_set_policy(policy).unwrap();
    store.push_claim(id, candidate).unwrap();
    store
        .push_result(id, candidate, "network_error", true, now() + 1.)
        .unwrap();
    assert_eq!(
        store.push_events(None, 100).unwrap().items[0].outcome,
        "network_error"
    );
    assert_eq!(store.push_status().unwrap().pending, 0);
}

#[test]
fn concurrent_article_permits_cannot_overspend_shared_daily_budget() {
    let (store, id, _) = setup(Config::default());
    article_budget(&store, 1);
    let mut policy = store.wb_policy().unwrap();
    policy.body_gap_ms = 0;
    store.wb_set_policy(policy).unwrap();
    let store = Arc::new(store);
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let store = store.clone();
            std::thread::spawn(move || {
                let (category, purpose) = if i % 2 == 0 {
                    (CATEGORY, PURPOSE)
                } else {
                    ("body", "archive")
                };
                store
                    .wb_request_permit(category, purpose, id, "FAKE_A", now())
                    .unwrap()
                    .request_id
                    .is_some()
            })
        })
        .collect();
    assert_eq!(
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|allowed| *allowed)
            .count(),
        1
    );
    assert_eq!(store.wb_article_window(now() - 86400.).unwrap().0, 1);
}

#[test]
fn reducing_retry_setting_prevents_next_unsent_attempt() {
    let (store, id, candidate) = setup(Config::default());
    store.push_claim(id, candidate).unwrap();
    store
        .push_result(id, candidate, "network_error", true, 0.)
        .unwrap();
    let mut policy = store.wb_policy().unwrap();
    policy.retries = 0;
    store.wb_set_policy(policy).unwrap();
    assert!(!store.push_claim(id, candidate).unwrap());
    assert_eq!(
        store.push_events(None, 100).unwrap().items[0].outcome,
        "retry_limit"
    );
}

#[test]
fn saving_policy_preserves_stopped_monitor_failure_reason() {
    let (store, id, _) = setup(Config::default());
    store
        .push_end(id, "failed", Some("扫描器读取失败"))
        .unwrap();
    store.wb_set_policy(store.wb_policy().unwrap()).unwrap();
    assert_eq!(
        store.push_run().unwrap().reason.as_deref(),
        Some("扫描器读取失败")
    );
}

#[test]
fn scanning_and_processing_have_independent_generations_and_controls() {
    let store = Store::open_in_memory().unwrap();
    let scan = store.push_start(Config::default()).unwrap();
    store
        .push_scanned(
            scan,
            Scan {
                links: vec![link("FAKE_A")],
                ..Default::default()
            },
            now(),
        )
        .unwrap();
    assert!(!store.push_run().unwrap().queue.active);
    assert!(store.push_next(scan, now()).unwrap().is_none());
    assert!(store
        .wb_request_permit(CATEGORY, PURPOSE, scan, "FAKE_A", now())
        .is_err());
    store.push_end(scan, "stopped", None).unwrap();
    let scan = store.push_start(Config::default()).unwrap();
    let queue = store.push_queue_start().unwrap();
    assert_ne!(scan, queue);
    assert!(store.push_queue_start().is_err());
    let candidate = store.push_next(queue, now()).unwrap().unwrap();
    assert!(store.push_claim(queue, candidate.id).unwrap());
    store.push_queue_end(queue, "paused", None).unwrap();
    assert!(store.push_is_active(scan).unwrap());
    store
        .push_scanned(
            scan,
            Scan {
                links: vec![link("FAKE_B")],
                ..Default::default()
            },
            now(),
        )
        .unwrap();
    assert_eq!(store.push_status().unwrap().pending, 2);
    let next = store.push_queue_start().unwrap();
    store
        .push_import(
            queue,
            candidate.id,
            &metadata(&link("FAKE_A"), &html("FAKE_A")).unwrap(),
            now(),
        )
        .unwrap();
    store
        .push_queue_end(queue, "failed", Some("旧处理器晚到错误"))
        .unwrap();
    assert!(store.push_queue_is_active(next).unwrap());
    assert_eq!(store.push_status().unwrap().imported, 0);
    store.push_end(scan, "stopped", None).unwrap();
    assert!(store.push_queue_is_active(next).unwrap());
    assert!(store.push_claim(next, candidate.id).unwrap());
    store
        .push_import(
            next,
            candidate.id,
            &metadata(&link("FAKE_A"), &html("FAKE_A")).unwrap(),
            now(),
        )
        .unwrap();
    assert_eq!(store.push_status().unwrap().imported, 1);
}

#[tokio::test]
async fn queue_worker_processes_saved_links_with_scanning_stopped() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let scan = store.push_start(Config::default()).unwrap();
    store
        .push_scanned(
            scan,
            Scan {
                links: vec![link("FAKE_A")],
                ..Default::default()
            },
            now(),
        )
        .unwrap();
    store.push_end(scan, "stopped", None).unwrap();
    let queue = store.push_queue_start().unwrap();
    let (endpoint, server) = fixture(200, html("FAKE_A")).await;
    let clone = store.clone();
    let worker = tokio::spawn(async move { run_queue_at(clone, queue, Some(&endpoint)).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        server.await.unwrap();
        while store.push_status().unwrap().imported != 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let status = store.push_status().unwrap();
    assert!(!status.run.active);
    assert!(status.run.queue.active);
    assert_eq!(status.run.rounds, 1);
    assert_eq!(status.pending, 0);
    assert_eq!(status.requests_24h, 1);
    store.push_queue_end(queue, "paused", None).unwrap();
    tokio::time::timeout(Duration::from_secs(3), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn stopping_scan_keeps_inflight_queue_request_and_its_result() {
    let (store, id, _) = setup(Config::default());
    let store = Arc::new(store);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/s", listener.local_addr().unwrap());
    let candidate = store.push_next(id, now()).unwrap().unwrap();
    let clone = store.clone();
    let worker = tokio::spawn(async move {
        process_at(&clone, &client().unwrap(), id, candidate, &endpoint).await
    });
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut request = [0u8; 4096];
    let received = socket.read(&mut request).await.unwrap();
    assert!(
        received > 0,
        "queue request must arrive before stopping scan"
    );
    store.push_end(id, "stopped", None).unwrap();
    let body = html("FAKE_A");
    socket
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(store.push_status().unwrap().imported, 1);
    assert!(store.push_run().unwrap().queue.active);
    assert!(!store.push_run().unwrap().active);
}

#[test]
fn changing_scan_scope_does_not_reclassify_saved_links_or_clear_queue_wait() {
    let config = Config {
        bizzes: vec!["FAKE_A".into()],
        recent_hours: 168,
        ..Default::default()
    };
    let (store, queue, candidate) = setup(config);
    store
        .push_deferred(queue, now() + 3600., Some("测试额度等待"))
        .unwrap();
    store
        .push_end(store.push_run().unwrap().id, "stopped", None)
        .unwrap();
    store
        .push_start(Config {
            bizzes: vec!["FAKE_B".into()],
            recent_hours: 1,
            ..Default::default()
        })
        .unwrap();
    let run = store.push_run().unwrap();
    assert!(run.queue.active);
    assert!(run.queue.next_fetch_at > now());
    assert_eq!(run.queue.reason.as_deref(), Some("测试额度等待"));
    assert!(store.push_claim(queue, candidate).unwrap());
    let mut meta = metadata(&link("FAKE_A"), &html("FAKE_A")).unwrap();
    meta.published_at -= 2 * 86400;
    store.push_import(queue, candidate, &meta, now()).unwrap();
    assert_eq!(store.push_status().unwrap().imported, 1);
}

#[test]
fn legacy_queue_migration_preserves_candidates_attempts_and_discovery_scope() {
    let path = std::env::temp_dir().join(format!(
        "mpider-push-migrate-{}-{}.db",
        std::process::id(),
        (now() * 1000000.) as u64
    ));
    {
        let store = Store::open(&path).unwrap();
        let id = store
            .push_start(Config {
                bizzes: vec!["FAKE_A".into()],
                ..Default::default()
            })
            .unwrap();
        store
            .push_scanned(
                id,
                Scan {
                    links: vec![link("FAKE_A")],
                    ..Default::default()
                },
                now(),
            )
            .unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("ALTER TABLE push_candidates DROP COLUMN config; UPDATE push_candidates SET state='fetching',attempts=1; UPDATE push_monitor SET document=json_remove(document,'$.queue');").unwrap();
    }
    {
        let store = Store::open(&path).unwrap();
        store.push_restart().unwrap();
        assert!(!store.push_run().unwrap().active);
        assert!(!store.push_run().unwrap().queue.active);
        assert_eq!(
            store.push_events(None, 10).unwrap().items[0].outcome,
            "pending"
        );
        store
            .push_start(Config {
                bizzes: vec!["FAKE_B".into()],
                ..Default::default()
            })
            .unwrap();
        let queue = store.push_queue_start().unwrap();
        let candidate = store.push_next(queue, now()).unwrap().unwrap();
        assert_eq!(candidate.attempts, 1);
        store.push_claim(queue, candidate.id).unwrap();
        store
            .push_import(
                queue,
                candidate.id,
                &metadata(&link("FAKE_A"), &html("FAKE_A")).unwrap(),
                now(),
            )
            .unwrap();
        assert_eq!(store.push_status().unwrap().imported, 1);
    }
    let _ = std::fs::remove_file(path);
}
