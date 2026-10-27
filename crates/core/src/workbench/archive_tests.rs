//! 通过真实本地 HTTP 服务验证正文下载、图片失败补齐与请求账本。
use super::*;
use crate::workbench::Policy;
use crate::ArticleFields;
use std::sync::{atomic::AtomicBool, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn server() -> (
    String,
    Arc<AtomicBool>,
    Arc<Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let recover = Arc::new(AtomicBool::new(false));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let switch = recover.clone();
    let records = observed.clone();
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let recover = switch.clone();
            let observed = records.clone();
            tokio::spawn(async move {
                let mut input = vec![0; 8192];
                let n = stream.read(&mut input).await.unwrap();
                let request = String::from_utf8_lossy(&input[..n]);
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                observed.lock().unwrap().push(path.clone());
                let png=base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jr1sAAAAASUVORK5CYII=").unwrap();
                let (status,data)=match path.split('?').next().unwrap_or_default(){
            "/body"=>(200,format!(r#"<html><head><meta property="og:title" content="虚构离线文章"></head><body><div id="js_content" class="rich_media_content"><p>真实 HTTP 测试正文</p><img data-src="http://{addr}/image.png" onerror="alert(1)"><img data-src="http://{addr}/missing.png"><script>alert(1)</script></div></body></html>"#).into_bytes()),
            "/missing.png" if !recover.load(Ordering::SeqCst)=>(503,b"synthetic unavailable".to_vec()),
            "/limited"=>(429,b"synthetic rate signal".to_vec()),
            "/verify"=>(200,br#"<html><script src="/mp/secitptpage/template/verify.js"></script></html>"#.to_vec()),
            "/image.png"|"/missing.png"=>(200,png),_ =>(404,b"not found".to_vec())};
                let header=format!("HTTP/1.1 {status} Synthetic\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",data.len());
                let _ = stream.write_all(header.as_bytes()).await;
                let _ = stream.write_all(&data).await;
            });
        }
    });
    (format!("http://{addr}"), recover, observed, handle)
}
fn setup(url: &str) -> (Arc<Store>, i64, i64, PathBuf) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    store
        .wb_set_policy(Policy {
            list_min_ms: 0,
            list_max_ms: 0,
            body_gap_ms: 0,
            image_gap_ms: 0,
            retries: 1,
            ..Default::default()
        })
        .unwrap();
    let id = store
        .upsert_article(
            "SYNTHETIC",
            "1",
            1,
            &ArticleFields {
                title: Some("虚构".into()),
                content_url: Some(url.into()),
                ..Default::default()
            },
        )
        .unwrap();
    let task = store
        .wb_download_enqueue(vec![id])
        .unwrap()
        .task_id
        .unwrap();
    let dir = std::env::temp_dir().join(format!(
        "mpider-offline-test-{}-{task}-{}",
        std::process::id(),
        (now() * 1000000.) as u64
    ));
    (store, id, task, dir)
}
#[tokio::test]
async fn partial_images_refill_without_refetching_body_and_offline_reader_is_local() {
    let (host, recover, observed, server) = server().await;
    let (store, id, task, root) = setup(&format!("{host}/body"));
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let first = collect_article(store.clone(), root.clone(), task, id, client.clone())
        .await
        .unwrap();
    assert_eq!(first.state, "partial");
    assert_eq!(first.missing_images.len(), 1);
    let path = Path::new(&first.directory);
    let html = std::fs::read_to_string(path.join("index.html")).unwrap();
    assert!(!html.contains("<script"));
    assert!(!html.contains("onerror"));
    assert!(!html.contains("src=\"http"));
    assert!(html.contains("图片待补齐"));
    recover.store(true, Ordering::SeqCst);
    let next = task;
    let second = collect_article(store.clone(), root.clone(), next, id, client)
        .await
        .unwrap();
    assert_eq!(second.state, "complete");
    let calls = observed.lock().unwrap().clone();
    assert_eq!(calls.iter().filter(|p| *p == "/body").count(), 1);
    assert_eq!(calls.iter().filter(|p| *p == "/image.png").count(), 1);
    assert_eq!(calls.iter().filter(|p| *p == "/missing.png").count(), 3);
    let reader = reader_html(&store, &root, id).unwrap();
    assert!(reader.contains("data:image/png;base64,"));
    assert!(!reader.contains("src=\"images/"));
    let events = store.wb_requests(0.).unwrap();
    assert_eq!(events.len(), calls.len());
    assert_eq!(
        events.iter().filter(|e| e.outcome == "http_error").count(),
        2
    );
    server.abort();
    let _ = std::fs::remove_dir_all(root);
}
#[tokio::test]
async fn http_429_records_status_and_cools_all_categories_without_retry() {
    let (host, _, observed, server) = server().await;
    let (store, _, task, root) = setup(&format!("{host}/limited"));
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    assert!(http_get(
        &store,
        &client,
        &format!("{host}/limited"),
        "body",
        task,
        "SYNTHETIC",
        1,
    )
    .await
    .is_err());
    assert_eq!(observed.lock().unwrap().len(), 1);
    assert!(store.wb_policy().unwrap().cooldown_until > now());
    let events = store.wb_requests(0.).unwrap();
    assert_eq!(events[0].outcome, "rate_limited");
    assert_eq!(events[0].detail["http_status"], 429);
    assert!(events[0].detail.get("min_ms").is_some());
    assert!(store
        .wb_request_permit("list", "history", 1, "SYNTHETIC", now())
        .unwrap()
        .request_id
        .is_none());
    server.abort();
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn http_200_verification_without_control_pauses_without_claiming_rate_limit() {
    let (host, _, observed, server) = server().await;
    let (store, id, task, root) = setup(&format!("{host}/verify"));
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    assert!(
        collect_article(store.clone(), root.clone(), task, id, client)
            .await
            .is_err()
    );
    assert_eq!(observed.lock().unwrap().len(), 1);
    assert_eq!(store.wb_task(task).unwrap().state, "paused");
    assert!(store.wb_policy().unwrap().cooldown_until <= now());
    let events = store.wb_requests(0.).unwrap();
    assert_eq!(events[0].outcome, "verification_required");
    assert_eq!(events[0].detail["http_status"], 200);
    assert_eq!(events[0].detail["content_outcome"], "verification_required");
    server.abort();
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn downloaded_during_queue_wait_skips_before_http_permit_and_ledger() {
    let (host, _, observed, server) = server().await;
    let (store, id, task, root) = setup(&format!("{host}/body"));
    store
        .upsert_article(
            "SYNTHETIC",
            "1",
            1,
            &ArticleFields {
                content_md: Some("等待期间已由其他入口下载".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    assert!(
        download_article(store.clone(), root.clone(), task, id, client)
            .await
            .unwrap()
            .is_none()
    );
    assert!(observed.lock().unwrap().is_empty());
    assert!(store.wb_requests(0.).unwrap().is_empty());
    assert!(!root.exists());
    server.abort();
}

#[tokio::test]
async fn download_tasks_run_fifo_repeated_clicks_never_refetch_or_create_auto_zip() {
    let (host, _, observed, server) = server().await;
    let (store, first, legacy, root) = setup(&format!("{host}/body?article=first"));
    store.wb_task_state(legacy, "cancelled", None).unwrap();
    let second = store
        .upsert_article(
            "SYNTHETIC",
            "2",
            1,
            &ArticleFields {
                content_url: Some(format!("{host}/body?article=second")),
                ..Default::default()
            },
        )
        .unwrap();
    let one = store
        .wb_download_enqueue(vec![first])
        .unwrap()
        .task_id
        .unwrap();
    let two = store
        .wb_download_enqueue(vec![second])
        .unwrap()
        .task_id
        .unwrap();
    let duplicate = store.wb_download_enqueue(vec![first, second]).unwrap();
    assert_eq!(duplicate.already_queued, 2);
    assert!(duplicate.task_id.is_none());
    let engine = tokio::spawn(run(store.clone(), root.clone()));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if store.wb_task(two).unwrap().state == "done" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    for id in [one, two] {
        let task = store.wb_task(id).unwrap();
        assert_eq!(task.state, "done");
        assert_eq!(task.progress.failed, 0);
        assert_eq!(task.progress.completed, 1);
        assert!(task.result_path.is_none());
    }
    let calls = observed.lock().unwrap().clone();
    let body: Vec<_> = calls
        .iter()
        .filter(|p| p.starts_with("/body"))
        .map(String::as_str)
        .collect();
    assert_eq!(body, vec!["/body?article=first", "/body?article=second"]);
    assert_eq!(store.wb_requests(0.).unwrap().len(), calls.len());
    let repeated = store.wb_download_enqueue(vec![first, second]).unwrap();
    assert_eq!(repeated.already_downloaded, 2);
    assert_eq!(repeated.queued, 0);
    assert!(repeated.task_id.is_none());
    assert_eq!(store.wb_snapshot().unwrap().download_queue.completed, 2);
    assert_eq!(store.wb_requests(0.).unwrap().len(), calls.len());
    assert!(!root.join("exports").exists());
    engine.abort();
    let _ = engine.await;
    server.abort();
    let _ = std::fs::remove_dir_all(root);
}
