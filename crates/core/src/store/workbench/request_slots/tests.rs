use super::*;

fn setup(workers: usize, gap: u64) -> Store {
    let s = Store::open_in_memory().unwrap();
    s.wb_set_policy(Policy {
        body_workers: workers,
        body_gap_ms: gap,
        ..Default::default()
    })
    .unwrap();
    s
}
fn permit(s: &Store, ts: f64) -> Permit {
    s.wb_request_permit("body", "archive", 1, "FAKE", ts)
        .unwrap()
}

#[test]
fn first_wave_is_immediate_and_each_slot_waits_after_its_response() {
    let s = setup(2, 10000);
    let first = permit(&s, 1000.).request_id.unwrap();
    let second = permit(&s, 1000.).request_id.unwrap();
    // 即使请求耗时远超间隔，响应之前也不提前放行。
    assert!(permit(&s, 1100.).request_id.is_none());
    s.wb_request_finish(&first, "ok", json!({}), 1101.).unwrap();
    assert!(permit(&s, 1101.).request_id.is_none());
    assert!(permit(&s, 1110.99).request_id.is_none());
    assert!(permit(&s, 1111.).request_id.is_some());
    // 另一工作位仍在进行，不能被第一个工作位的完成或间隔唤醒。
    assert!(permit(&s, 1200.).request_id.is_none());
    s.wb_request_finish(&second, "network_error", json!({}), 1201.)
        .unwrap();
    assert!(permit(&s, 1210.99).request_id.is_none());
    assert!(permit(&s, 1211.).request_id.is_some());
}

#[test]
fn concurrent_initial_requests_are_bounded_and_zero_gap_still_limits_active_requests() {
    let s = std::sync::Arc::new(setup(3, 0));
    let threads: Vec<_> = (0..12)
        .map(|_| {
            let s = s.clone();
            std::thread::spawn(move || permit(&s, 1000.).request_id)
        })
        .collect();
    let granted: Vec<_> = threads
        .into_iter()
        .filter_map(|t| t.join().unwrap())
        .collect();
    assert_eq!(granted.len(), 3);
    s.wb_request_finish(&granted[0], "ok", json!({}), 1001.)
        .unwrap();
    assert!(permit(&s, 1001.).request_id.is_some());
    assert!(permit(&s, 1001.).request_id.is_none());
}

#[test]
fn lowering_concurrency_and_changing_interval_do_not_release_active_requests() {
    let s = setup(2, 30000);
    let first = permit(&s, 1000.).request_id.unwrap();
    let second = permit(&s, 1000.).request_id.unwrap();
    let mut p = s.wb_policy().unwrap();
    p.body_workers = 1;
    p.body_gap_ms = 5000;
    s.wb_set_policy(p).unwrap();
    s.wb_request_finish(&first, "ok", json!({}), 1001.).unwrap();
    assert!(permit(&s, 1010.).request_id.is_none());
    s.wb_request_finish(&second, "ok", json!({}), 1011.)
        .unwrap();
    assert!(permit(&s, 1011.).request_id.is_some());
}

#[test]
fn finish_is_idempotent_and_restart_preserves_spacing_and_budget() {
    let s = setup(1, 10000);
    let first = permit(&s, 1000.).request_id.unwrap();
    s.wb_request_finish(&first, "ok", json!({}), 1002.).unwrap();
    s.wb_request_finish(&first, "ok", json!({}), 1100.).unwrap();
    let denied = permit(&s, 1003.);
    assert_eq!(denied.next_at, 1012.);
    let second = permit(&s, 1012.).request_id.unwrap();
    s.wb_restart(1020.).unwrap();
    assert_eq!(permit(&s, 1021.).next_at, 1030.);
    assert_eq!(
        s.wb_requests(0.)
            .unwrap()
            .iter()
            .find(|r| r.id == second)
            .unwrap()
            .outcome,
        "interrupted"
    );
    assert_eq!(s.wb_article_window(0.).unwrap().0, 2);
    assert!(permit(&s, 1030.).request_id.is_some());
}

#[test]
fn completed_slot_spacing_survives_reopening_database() {
    let path = std::env::temp_dir().join(format!(
        "mpider-request-slots-{}-{}.db",
        std::process::id(),
        now()
    ));
    {
        let s = Store::open(&path).unwrap();
        s.wb_set_policy(Policy {
            body_workers: 1,
            body_gap_ms: 10000,
            ..Default::default()
        })
        .unwrap();
        let first = permit(&s, 1000.).request_id.unwrap();
        s.wb_request_finish(&first, "ok", json!({}), 1002.).unwrap();
    }
    {
        let s = Store::open(&path).unwrap();
        assert_eq!(permit(&s, 1003.).next_at, 1012.);
        assert!(permit(&s, 1012.).request_id.is_some());
    }
    let _ = std::fs::remove_file(path);
}
