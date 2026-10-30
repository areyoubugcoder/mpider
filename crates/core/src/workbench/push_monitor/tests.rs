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
fn no_gap(store: &Store) {
    let mut policy = store.wb_policy().unwrap();
    policy.body_gap_ms = 0;
    store.wb_set_policy(policy).unwrap();
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
fn hourly_budget_survives_runs_and_body_gap_is_shared() {
    let (store, id, _) = setup(Config {
        hourly_budget: 1,
        ..Default::default()
    });
    no_gap(&store);
    let ts = now();
    let permit = store
        .wb_request_permit("body", PURPOSE, id, "FAKE_A", ts)
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
        .wb_request_permit("body", PURPOSE, second, "FAKE_A", ts + 5.)
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
        .wb_request_permit("body", PURPOSE, id, "FAKE_A", ts + 1.)
        .unwrap()
        .request_id
        .is_none());
}

async fn fixture(code: u16, body: String) -> (String, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![0u8; 8192];
        let n = socket.read(&mut request).await.unwrap();
        let head=format!("HTTP/1.1 {code} Test\r\nContent-Length: {}\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n",body.len());
        socket.write_all(head.as_bytes()).await.unwrap();
        socket.write_all(body.as_bytes()).await.unwrap();
        String::from_utf8_lossy(&request[..n]).into_owned()
    });
    (format!("http://{address}/s"), task)
}

#[tokio::test]
async fn real_http_success_commits_and_verification_or_429_cools_down_with_pending_candidate() {
    for (code, body, expected) in [
        (200, html("FAKE_A"), "imported"),
        (200, "<html>环境异常 完成验证后</html>".into(), "pending"),
        (429, String::new(), "pending"),
        (503, String::new(), "pending"),
    ] {
        let (store, id, _) = setup(Config::default());
        no_gap(&store);
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
            assert!(store.wb_policy().unwrap().cooldown_until > now() + 7100.);
        }
        assert_eq!(status.imported, i64::from(expected == "imported"));
    }
}

#[tokio::test]
async fn stop_during_http_does_not_commit_late_reply() {
    let (store, id, _) = setup(Config::default());
    let store = Arc::new(store);
    no_gap(&store);
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
