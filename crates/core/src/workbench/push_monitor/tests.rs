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
fn setup(config: Config) -> (Store, i64, i64) {
    let store = Store::open_in_memory().unwrap();
    let id = store.push_start(config).unwrap();
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
    let id = store.push_start(Config::default()).unwrap();
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
        store.push_status().unwrap().events[0].reason.as_deref(),
        Some("link_updated")
    );
    assert_eq!(store.push_status().unwrap().requests_hour, 0);
}

#[test]
fn old_config_gets_one_second_interval_and_new_default_budget_is_3600() {
    let config: Config = serde_json::from_str(r#"{"hourly_budget":18}"#).unwrap();
    assert_eq!(config.fetch_seconds, 1);
    assert_eq!(config.hourly_budget, 18);
    assert_eq!(Config::default().hourly_budget, 3600);
    assert!(Config {
        fetch_seconds: 0,
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
    let id = store.push_start(Config::default()).unwrap();
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
    let clone = store.clone();
    let scanner = WaitingScanner(entered.clone());
    let task =
        tokio::spawn(async move { run_with_scanner_at(clone, id, scanner, Some(&endpoint)).await });
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
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(store.push_status().unwrap().requests_hour, 1);
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
    let next = store.push_start(Config::default()).unwrap();
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
    let id = store.push_start(Config::default()).unwrap();
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
    assert_eq!(store.push_status().unwrap().events[0].outcome, "fetching");
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
        store.push_status().unwrap().events[0].article_id,
        Some(article)
    );
    assert_eq!(store.push_status().unwrap().events[0].outcome, "existing");
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
    assert_eq!(store.push_status().unwrap().events[0].outcome, "ignored");
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
    assert_eq!(store.push_status().unwrap().events[0].outcome, "ignored");
    assert!(store.list_articles(None, 10, 0, false).unwrap().is_empty());
}

#[test]
fn hourly_budget_survives_runs_and_webpage_rate_is_independent() {
    let (store, id, _) = setup(Config {
        hourly_budget: 1,
        ..Default::default()
    });
    let ts = now();
    let permit = store
        .wb_request_permit(CATEGORY, PURPOSE, id, "FAKE_A", ts)
        .unwrap();
    assert!(permit.request_id.is_some());
    store.push_end(id, "stopped", None).unwrap();
    let second = store
        .push_start(Config {
            hourly_budget: 1,
            ..Default::default()
        })
        .unwrap();
    let denied = store
        .wb_request_permit(CATEGORY, PURPOSE, second, "FAKE_A", ts + 5.)
        .unwrap();
    assert!(denied.request_id.is_none());
    assert!(denied.next_at >= ts + 3600.);
    let (store, id, _) = setup(Config::default());
    let mut p = store.wb_policy().unwrap();
    p.body_gap_ms = 30_000;
    store.wb_set_policy(p).unwrap();
    store
        .wb_request_permit("body", "archive", 777, "FAKE_A", ts)
        .unwrap();
    assert!(store
        .wb_request_permit(CATEGORY, PURPOSE, id, "FAKE_A", ts + 0.1)
        .unwrap()
        .request_id
        .is_some());
    let early = store
        .wb_request_permit(CATEGORY, PURPOSE, id, "FAKE_A", ts + 0.9)
        .unwrap();
    assert!(early.request_id.is_none());
    assert!((early.next_at - (ts + 1.1)).abs() < 0.001);
    let body = store
        .wb_request_permit("body", "archive", 777, "FAKE_A", ts + 1.)
        .unwrap();
    assert!((body.next_at - (ts + 30.)).abs() < 0.001);
    store.wb_cooldown(ts + 7200.).unwrap();
    assert!(store
        .wb_request_permit(CATEGORY, PURPOSE, id, "FAKE_A", ts + 1.1)
        .unwrap()
        .request_id
        .is_some());
    assert!(store
        .wb_request_permit("body", "archive", 777, "FAKE_A", ts + 31.)
        .unwrap()
        .request_id
        .is_none());
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
    assert_eq!(store.push_status().unwrap().requests_hour, 1);
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
    assert_eq!(store.push_status().unwrap().requests_hour, 2);
    assert_eq!(store.push_status().unwrap().imported, 1);
    assert!(store
        .wb_snapshot()
        .unwrap()
        .requests
        .iter()
        .any(|r| r.outcome == "redirect"));
}

#[tokio::test]
async fn redirect_waits_for_hourly_budget_after_reopen_without_hidden_http() {
    let (store, id, _) = setup(Config {
        hourly_budget: 1,
        ..Default::default()
    });
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
    let next_run = store
        .push_start(Config {
            hourly_budget: 1,
            ..Default::default()
        })
        .unwrap();
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
    assert_eq!(store.push_status().unwrap().requests_hour, 1);
    assert_eq!(store.push_status().unwrap().pending, 1);
    assert!(store.push_run().unwrap().next_fetch_at > now() + 3500.);
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
        assert_eq!(store.push_status().unwrap().requests_hour, 1);
        assert!(store.push_next(id, now()).unwrap().is_none());
        assert_eq!(
            store.push_status().unwrap().events[0].outcome,
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
    assert_eq!(store.push_status().unwrap().requests_hour, 3);
    assert_eq!(
        store.push_status().unwrap().events[0].outcome,
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
        store.push_status().unwrap().events[0].reason.as_deref(),
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
    assert_eq!(store.push_status().unwrap().requests_hour, 1);
    assert_eq!(
        store.push_status().unwrap().events[0].reason.as_deref(),
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
        assert_eq!(status.events[0].outcome, expected);
        assert_eq!(status.requests_hour, 1);
        if code == 429 || status.events[0].reason.as_deref() == Some("verification_required") {
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
    store.push_end(id, "stopped", None).unwrap();
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
    assert_eq!(json!(store.push_status().unwrap())["run"]["active"], false);
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
        let id = store
            .push_start(Config {
                hourly_budget: 1,
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
        let id = store
            .push_start(Config {
                hourly_budget: 1,
                ..Default::default()
            })
            .unwrap();
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
    assert!(store.push_status().unwrap().events.is_empty());
}
