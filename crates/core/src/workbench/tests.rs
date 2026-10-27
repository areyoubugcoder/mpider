//! 工作台行为回归：覆盖用户可观察的数据边界、跨重启续采及实验有效期证据。
use super::*;
use crate::{
    model::ParsedArticle,
    native::{
        protocol::{NativePage, NativeRequest},
        NativeConfig,
    },
    ArticleFields, Store,
};
use serde_json::json;
use std::sync::Arc;

fn article(mid: &str, published: i64) -> ParsedArticle {
    ParsedArticle {
        mid: mid.into(),
        idx: 1,
        title: Some(mid.into()),
        published_at: Some(published),
        content_url: Some(format!(
            "https://mp.weixin.qq.com/s?__biz=FAKE&mid={mid}&idx=1"
        )),
        ..Default::default()
    }
}
fn page(biz: &str, cursor: &str, items: Vec<ParsedArticle>, end: bool) -> NativePage {
    NativePage {
        biz: biz.into(),
        user_name: format!("fake-{biz}"),
        nickname: Some(format!("虚构-{biz}")),
        avatar: None,
        cursor: cursor.into(),
        is_end: end,
        articles: items,
    }
}
fn register(store: &Store, biz: &str) -> Source {
    store.upsert_account(biz, Some("虚构公众号"), None).unwrap();
    let first = page(
        biz,
        "cursor-first",
        vec![article("home-1", 1000), article("home-2", 900)],
        false,
    );
    for a in &first.articles {
        store
            .upsert_article(
                biz,
                &a.mid,
                a.idx,
                &ArticleFields {
                    title: a.title.clone(),
                    content_url: a.content_url.clone(),
                    ..Default::default()
                },
            )
            .unwrap();
    }
    store
        .wb_capture(
            &first,
            &NativeConfig::default(),
            &format!("fake-{biz}.dat"),
            "fake-home.dat",
            1000.,
        )
        .unwrap();
    store.wb_source(biz).unwrap().unwrap()
}
fn history(store: &Store, biz: &str, count: u32) -> i64 {
    store
        .wb_history_create(
            vec![biz.into()],
            HistoryTarget {
                count: Some(count),
                cutoff_at: None,
                pages: None,
                cursor_id: None,
            },
        )
        .unwrap()[0]
}
fn zero_policy(store: &Store) {
    store
        .wb_set_policy(Policy {
            list_min_ms: 0,
            list_max_ms: 0,
            body_gap_ms: 0,
            image_gap_ms: 0,
            ..Default::default()
        })
        .unwrap();
}

#[test]
fn buffer_survives_tasks_and_existing_rows_count_toward_coverage() {
    let s = Store::open_in_memory().unwrap();
    let source = register(&s, "FAKE");
    let id = history(&s, "FAKE", 1);
    let next = page(
        "FAKE",
        "cursor-second",
        vec![
            article("old-1", 800),
            article("old-2", 700),
            article("old-3", 600),
        ],
        false,
    );
    s.wb_history_commit(id, &next, &source.session_id, "fake-second.dat", 1100.)
        .unwrap();
    let done = s.wb_consume(id).unwrap();
    assert_eq!(done.state, "done");
    assert_eq!(done.progress.completed, 1);
    let checkpoint = s.wb_source("FAKE").unwrap().unwrap();
    assert_eq!(checkpoint.pending_count, 2);
    let next_id = history(&s, "FAKE", 2);
    let done = s.wb_consume(next_id).unwrap();
    assert_eq!(done.progress.completed, 2);
    assert_eq!(done.progress.pages, 0);
    assert_eq!(done.progress.new_articles, 0);
    assert_eq!(
        s.wb_source("FAKE").unwrap().unwrap().history_cursor_id,
        checkpoint.history_cursor_id
    );
}
#[test]
fn refreshing_home_and_session_never_replaces_history_or_pending() {
    let s = Store::open_in_memory().unwrap();
    let source = register(&s, "FAKE");
    let id = history(&s, "FAKE", 1);
    s.wb_history_commit(
        id,
        &page(
            "FAKE",
            "older-cursor",
            vec![article("old1", 800), article("old2", 700)],
            false,
        ),
        &source.session_id,
        "older.dat",
        1100.,
    )
    .unwrap();
    s.wb_consume(id).unwrap();
    let before = s.wb_source("FAKE").unwrap().unwrap();
    s.wb_capture(
        &page("FAKE", "latest-cursor", vec![article("new", 2000)], false),
        &NativeConfig::default(),
        "new-session.dat",
        "new-home.dat",
        2000.,
    )
    .unwrap();
    let after = s.wb_source("FAKE").unwrap().unwrap();
    assert_eq!(after.history_cursor_id, before.history_cursor_id);
    assert_eq!(after.pending_count, 1);
    assert_ne!(after.home_cursor_id, before.home_cursor_id);
    assert_ne!(after.session_id, before.session_id);
}
#[test]
fn date_boundary_ignores_old_pinned_item_and_keeps_older_buffer() {
    let s = Store::open_in_memory().unwrap();
    let source = register(&s, "FAKE");
    let id = s
        .wb_history_create(
            vec!["FAKE".into()],
            HistoryTarget {
                count: None,
                cutoff_at: Some(500),
                pages: None,
                cursor_id: None,
            },
        )
        .unwrap()[0];
    s.wb_history_commit(
        id,
        &page(
            "FAKE",
            "c2",
            vec![article("pinned", 10), article("recent", 800)],
            false,
        ),
        &source.session_id,
        "p2.dat",
        1100.,
    )
    .unwrap();
    assert_eq!(s.wb_consume(id).unwrap().state, "running");
    s.wb_history_commit(
        id,
        &page("FAKE", "c3", vec![article("old1", 400)], false),
        &source.session_id,
        "p3.dat",
        1200.,
    )
    .unwrap();
    assert_eq!(s.wb_consume(id).unwrap().state, "running");
    s.wb_history_commit(
        id,
        &page("FAKE", "c4", vec![article("old2", 300)], false),
        &source.session_id,
        "p4.dat",
        1300.,
    )
    .unwrap();
    let done = s.wb_consume(id).unwrap();
    assert_eq!(done.state, "done");
    assert_eq!(done.progress.completed, 1);
    assert_eq!(s.wb_source("FAKE").unwrap().unwrap().pending_count, 3);
    assert_eq!(
        s.wb_consume(history(&s, "FAKE", 3))
            .unwrap()
            .progress
            .completed,
        3
    );
}
#[test]
fn confirmed_rebuild_uses_stable_boundary_not_old_page_number() {
    let s = Store::open_in_memory().unwrap();
    let source = register(&s, "FAKE");
    let id = history(&s, "FAKE", 1);
    s.wb_begin_rebuild(id).unwrap();
    s.wb_history_commit(
        id,
        &page(
            "FAKE",
            "rebuilt",
            vec![
                article("new", 2000),
                article("home-1", 1000),
                article("home-2", 900),
                article("older", 800),
            ],
            false,
        ),
        &source.session_id,
        "rebuilt.dat",
        2000.,
    )
    .unwrap();
    let done = s.wb_consume(id).unwrap();
    assert_eq!(done.progress.completed, 1);
    assert_eq!(done.progress.rebuilds, 1);
    assert!(!done.progress.rebuilding);
    assert_eq!(s.wb_source("FAKE").unwrap().unwrap().pending_count, 0);
    assert!(s.wb_begin_rebuild(id).is_err());
}
#[test]
fn rebuild_missing_boundary_stops_without_replacing_saved_checkpoint() {
    let s = Store::open_in_memory().unwrap();
    let source = register(&s, "FAKE");
    let id = history(&s, "FAKE", 10);
    s.wb_begin_rebuild(id).unwrap();
    s.wb_history_commit(
        id,
        &page("FAKE", "", vec![article("unrelated", 20)], true),
        &source.session_id,
        "end.dat",
        2000.,
    )
    .unwrap();
    assert_eq!(s.wb_task(id).unwrap().state, "needs_action");
    assert_eq!(
        s.wb_source("FAKE").unwrap().unwrap().history_cursor_id,
        source.history_cursor_id
    );
}
#[test]
fn wrong_source_and_cancelled_inflight_response_do_not_advance() {
    let s = Store::open_in_memory().unwrap();
    let source = register(&s, "FAKE");
    let id = history(&s, "FAKE", 10);
    assert!(s
        .wb_history_commit(
            id,
            &page("OTHER", "x", vec![article("foreign", 1)], false),
            &source.session_id,
            "bad.dat",
            2000.
        )
        .is_err());
    s.wb_task_state(id, "cancelled", None).unwrap();
    s.wb_history_commit(
        id,
        &page("FAKE", "next", vec![article("ignored", 1)], false),
        &source.session_id,
        "next.dat",
        2000.,
    )
    .unwrap();
    assert_eq!(
        s.wb_source("FAKE").unwrap().unwrap().history_cursor_id,
        source.history_cursor_id
    );
    assert_eq!(s.count_articles(None).unwrap(), 2);
}
#[test]
fn paused_source_does_not_block_another_source_or_independent_archive() {
    let s = Store::open_in_memory().unwrap();
    register(&s, "A");
    register(&s, "B");
    let first = history(&s, "A", 10);
    s.wb_task_state(first, "needs_action", None).unwrap();
    let second = history(&s, "B", 10);
    assert_eq!(s.wb_consume(second).unwrap().state, "running");
    let aid = s.list_articles(None, 10, 0, false).unwrap()[0].id;
    let archive = s.wb_download_enqueue(vec![aid]).unwrap().task_id.unwrap();
    s.wb_task_state(archive, "cancelled", None).unwrap();
    assert_eq!(s.wb_task(second).unwrap().state, "running");
    assert_eq!(s.wb_task(first).unwrap().state, "needs_action");
}
#[test]
fn download_target_is_a_fixed_snapshot_and_deduplicated() {
    let s = Store::open_in_memory().unwrap();
    register(&s, "A");
    let aid = s.list_articles(None, 10, 0, false).unwrap()[0].id;
    let id = s
        .wb_download_enqueue(vec![aid, aid])
        .unwrap()
        .task_id
        .unwrap();
    s.upsert_article("A", "later", 1, &ArticleFields::default())
        .unwrap();
    assert_eq!(s.wb_task(id).unwrap().target["article_ids"], json!([aid]));
}
#[test]
fn shared_budget_counts_native_once_and_includes_experiment_controls() {
    let s = Store::open_in_memory().unwrap();
    zero_policy(&s);
    let mut p = s.wb_policy().unwrap();
    p.list_daily_budget = 2;
    s.wb_set_policy(p).unwrap();
    let cfg = NativeConfig {
        transport: "session".into(),
        min_gap_seconds: 0,
        max_gap_seconds: 0,
        ..Default::default()
    };
    let native = s.native_create(&cfg).unwrap();
    s.native_record_request(
        &native,
        &NativeRequest {
            request_id: "observed-native".into(),
            ts: 1000.,
            cgi: 5814,
            action: Some(0),
            offset: None,
            user_name: Some("fake".into()),
            transport: "native".into(),
            capture_only: false,
        },
        "fake",
    )
    .unwrap();
    assert!(s
        .wb_request_permit("list", "cursor_control_home", 1, "FAKE", 1001.)
        .unwrap()
        .request_id
        .is_some());
    assert!(s
        .wb_request_permit("list", "history", 2, "FAKE", 1002.)
        .unwrap()
        .request_id
        .is_none());
    assert_eq!(s.wb_requests(0.).unwrap().len(), 2);
    assert_eq!(s.wb_list_window(0.).unwrap().0, 2);
}
#[test]
fn live_policy_change_applies_to_next_unsent_request_below_sixty_seconds() {
    let s = Store::open_in_memory().unwrap();
    let mut p = Policy {
        list_min_ms: 60000,
        list_max_ms: 60000,
        ..Default::default()
    };
    s.wb_set_policy(p.clone()).unwrap();
    assert!(s
        .wb_request_permit("list", "history", 1, "A", 1000.)
        .unwrap()
        .request_id
        .is_some());
    assert!(s
        .wb_request_permit("list", "history", 1, "A", 1001.)
        .unwrap()
        .request_id
        .is_none());
    p.list_min_ms = 500;
    p.list_max_ms = 500;
    let version = s.wb_set_policy(p).unwrap().version;
    assert!(s
        .wb_request_permit("list", "history", 1, "A", 1001.)
        .unwrap()
        .request_id
        .is_some());
    assert_eq!(s.wb_requests(0.).unwrap()[0].policy_version, version);
}
#[test]
fn concurrent_workers_cannot_multiply_category_rate() {
    let s = Arc::new(Store::open_in_memory().unwrap());
    zero_policy(&s);
    let mut p = s.wb_policy().unwrap();
    p.image_gap_ms = 1000;
    s.wb_set_policy(p).unwrap();
    let threads: Vec<_> = (0..12)
        .map(|i| {
            let s = s.clone();
            std::thread::spawn(move || {
                s.wb_request_permit("image", "archive", i, "A", 1000.)
                    .unwrap()
                    .request_id
                    .is_some()
            })
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .map(|t| t.join().unwrap())
            .filter(|v| *v)
            .count(),
        1
    );
}
#[test]
fn experiments_reject_invalid_schedules_and_never_expose_cursor_raw_value() {
    let s = Store::open_in_memory().unwrap();
    let source = register(&s, "A");
    let cid = source.history_cursor_id.unwrap();
    for hours in [vec![25.], vec![-1.], vec![f64::NAN]] {
        assert!(s.wb_experiment_create(cid, hours, 1000.).is_err());
    }
    let id = s
        .wb_experiment_create(cid, vec![2., 1., 1., 24.], 1000.)
        .unwrap();
    let e = s.wb_experiment(id).unwrap();
    assert_eq!(e.schedule_hours, vec![0., 1., 2., 24.]);
    let serialized = serde_json::to_string(&s.wb_snapshot().unwrap()).unwrap();
    assert!(!serialized.contains("cursor-first"));
    assert!(!serialized.contains("fake-home.dat"));
}
#[test]
fn experiment_missing_points_are_recorded_without_catchup_requests() {
    let s = Store::open_in_memory().unwrap();
    let source = register(&s, "A");
    let cid = source.history_cursor_id.unwrap();
    let c = s.wb_cursor(cid).unwrap();
    let id = s
        .wb_experiment_create(cid, vec![1., 2., 24.], 1000.)
        .unwrap();
    let mut e = s.wb_experiment(id).unwrap();
    e.next_index = 1;
    s.wb_save_experiment(&e).unwrap();
    engine::mark_missed(&s, &mut e, &c, 1000. + 7200. + 30.).unwrap();
    assert_eq!(e.next_index, 2);
    assert_eq!(s.wb_probes(id).unwrap().len(), 1);
    engine::mark_missed(&s, &mut e, &c, 1000. + 86400. + 2.).unwrap();
    assert_eq!(e.next_index, 4);
    assert!(s.wb_requests(0.).unwrap().is_empty());
    assert_eq!(
        s.wb_source("A").unwrap().unwrap().history_cursor_id,
        Some(cid)
    );
}
#[test]
fn restart_keeps_checkpoints_sessions_experiments_and_request_cost() {
    let path = std::env::temp_dir().join(format!("mpider-workbench-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let (cid, eid, tid) = {
        let s = Store::open(&path).unwrap();
        let source = register(&s, "A");
        let cid = source.history_cursor_id.unwrap();
        let tid = history(&s, "A", 100);
        s.wb_consume(tid).unwrap();
        let eid = s.wb_experiment_create(cid, vec![1., 24.], 1000.).unwrap();
        zero_policy(&s);
        s.wb_request_permit("list", "history", tid, "A", 1000.)
            .unwrap();
        (cid, eid, tid)
    };
    let s = Store::open(&path).unwrap();
    s.wb_restart(2000.).unwrap();
    assert_eq!(s.wb_task(tid).unwrap().state, "paused");
    assert_eq!(s.wb_experiment(eid).unwrap().state, "running");
    assert_eq!(
        s.wb_source("A").unwrap().unwrap().history_cursor_id,
        Some(cid)
    );
    assert_eq!(s.wb_requests(0.).unwrap()[0].outcome, "interrupted");
    drop(s);
    let _ = std::fs::remove_file(path);
}

#[test]
fn known_legacy_cursor_is_preserved_without_inventing_page_evidence() {
    let s = Store::open_in_memory().unwrap();
    let cfg = NativeConfig {
        transport: "session".into(),
        ..Default::default()
    };
    let old = s.native_create(&cfg).unwrap();
    let request = NativeRequest {
        request_id: "legacy-first".into(),
        ts: 1000.,
        cgi: 5814,
        action: Some(0),
        offset: None,
        user_name: Some("fake-A".into()),
        transport: "native".into(),
        capture_only: false,
    };
    s.native_record_request(&old, &request, "fake").unwrap();
    s.native_commit_page(
        old.id,
        &request,
        &page(
            "A",
            "legacy-offset",
            vec![article("legacy-known", 500)],
            false,
        ),
        1001.,
        &crate::native::protocol::ResponseCodes {
            outer: Some(0),
            jsapi: Some(0),
            inner: Some(0),
            rate_signal: false,
            session_timeout: false,
        },
    )
    .unwrap();
    s.native_set_state(old.id, "done", None).unwrap();
    let current = s.native_create(&cfg).unwrap();
    let source = register(&s, "A");
    let (old_id, offset, observed) = s
        .wb_legacy_checkpoint("A", "fake-A", current.id)
        .unwrap()
        .unwrap();
    assert_eq!(old_id, old.id);
    assert_eq!(offset, "legacy-offset");
    assert_eq!(observed, 1001.);
    s.wb_import_legacy_checkpoint("A", &offset, "protected-legacy.dat", observed)
        .unwrap();
    let migrated = s.wb_source("A").unwrap().unwrap();
    assert_ne!(migrated.history_cursor_id, source.home_cursor_id);
    assert!(migrated.boundary_keys.is_empty());
    let cursor = s.wb_cursor(migrated.history_cursor_id.unwrap()).unwrap();
    assert_eq!(cursor.state, "legacy_unverified");
    let id = history(&s, "A", 1);
    s.wb_history_commit(
        id,
        &page(
            "A",
            "deeper",
            vec![article("legacy-known", 500), article("older", 400)],
            false,
        ),
        &source.session_id,
        "deeper.dat",
        2000.,
    )
    .unwrap();
    let done = s.wb_consume(id).unwrap();
    assert_eq!(done.progress.completed, 1);
    assert_eq!(s.wb_source("A").unwrap().unwrap().pending_count, 0);
}

#[test]
fn probe_and_schedule_index_commit_atomically_and_duplicates_do_not_repeat() {
    let s = Store::open_in_memory().unwrap();
    let source = register(&s, "A");
    let cid = source.history_cursor_id.unwrap();
    let id = s.wb_experiment_create(cid, vec![1., 24.], 1000.).unwrap();
    let probe = Probe {
        id: 0,
        experiment_id: id,
        request_id: None,
        role: "scheduled".into(),
        scheduled_at: 1000.,
        actual_at: 1001.,
        cursor_age_seconds: 1.,
        outcome: "valid".into(),
        evidence_path: None,
        detail: json!({}),
    };
    let first = s.wb_probe_add(probe.clone()).unwrap();
    assert_eq!(s.wb_experiment(id).unwrap().next_index, 1);
    assert_eq!(s.wb_probe_add(probe).unwrap(), first);
    assert_eq!(s.wb_probes(id).unwrap().len(), 1);
}

#[test]
fn monitoring_registers_all_sources_without_importing_articles_or_creating_tasks() {
    let s = Store::open_in_memory().unwrap();
    let run = s.wb_discovery_start(None).unwrap();
    s.wb_discovery_state(run, "running", None).unwrap();
    assert!(s.wb_discovery_start(None).is_err());
    let home = page("NEW", "fake-new", vec![article("home", 1000)], false);
    s.wb_observe_home(&home, 1000.).unwrap();
    s.wb_discovery_admit(run, "NEW", false, 1000.).unwrap();
    assert_eq!(s.wb_source("NEW").unwrap().unwrap().article_count, 0);
    assert!(s.wb_source("NEW").unwrap().unwrap().session_id.is_empty());
    assert!(s.wb_tasks().unwrap().is_empty());
    // 另一公众号独立准备好，不需要等待前一号的材料。
    register(&s, "OTHER");
    s.wb_discovery_admit(run, "OTHER", true, 1001.).unwrap();
    s.wb_discovery_capture(
        run,
        1002.,
        &home,
        &NativeConfig::default(),
        "fake-new-session.dat",
        "fake-home.dat",
        1002.,
    )
    .unwrap();
    let source = s.wb_source("NEW").unwrap().unwrap();
    assert!(source.history_cursor_id.is_some());
    s.wb_discovery_admit(run, "NEW", true, 1002.).unwrap();
    s.wb_discovery_admit(run, "NEW", true, 1003.).unwrap();
    assert!(s.wb_tasks().unwrap().is_empty());
    assert_eq!(s.wb_source("NEW").unwrap().unwrap().article_count, 0);
    let run = s.wb_discovery().unwrap().unwrap();
    assert_eq!(run.accounts.len(), 2);
    assert!(run
        .accounts
        .iter()
        .all(|a| a.session_ready && a.task_id.is_none()));
}

#[test]
fn monitoring_preserves_paused_tasks_and_stopped_runs_reject_late_admission() {
    let s = Store::open_in_memory().unwrap();
    register(&s, "FAKE");
    let old = history(&s, "FAKE", 10);
    s.wb_task_state(old, "paused", Some("用户暂停")).unwrap();
    let run = s.wb_discovery_start(None).unwrap();
    s.wb_discovery_admit(run, "FAKE", true, 1200.).unwrap();
    assert_eq!(s.wb_tasks().unwrap().len(), 1);
    assert_eq!(s.wb_task(old).unwrap().target["count"], 10);
    assert_eq!(s.wb_task(old).unwrap().state, "paused");
    assert_eq!(s.wb_discovery().unwrap().unwrap().accounts[0].task_id, None);
    s.wb_discovery_state(run, "stopping", None).unwrap();
    assert!(s.wb_discovery_admit(run, "FAKE", true, 1300.).is_err());
    s.wb_restart(1400.).unwrap();
    assert_eq!(s.wb_discovery().unwrap().unwrap().state, "stopped");
}

#[test]
fn passive_home_refresh_preserves_history_and_request_budget_is_idempotent() {
    let s = Store::open_in_memory().unwrap();
    let original = register(&s, "FAKE");
    let task = history(&s, "FAKE", 1);
    s.wb_history_commit(
        task,
        &page(
            "FAKE",
            "fake-page2",
            vec![article("old", 800), article("older", 700)],
            false,
        ),
        &original.session_id,
        "fake-page2.dat",
        1100.,
    )
    .unwrap();
    s.wb_consume(task).unwrap();
    let before = s.wb_source("FAKE").unwrap().unwrap();
    let home = page("FAKE", "fake-refreshed", vec![article("new", 2000)], false);
    s.wb_observe_home(&home, 1200.).unwrap();
    s.wb_capture(
        &home,
        &NativeConfig::default(),
        "fake-refreshed-session.dat",
        "fake-new-home.dat",
        1200.,
    )
    .unwrap();
    let after = s.wb_source("FAKE").unwrap().unwrap();
    assert_eq!(after.history_cursor_id, before.history_cursor_id);
    assert_eq!(after.pending_count, before.pending_count);
    let run = s.wb_discovery_start(None).unwrap();
    let ts = crate::model::now();
    s.wb_discovery_request(run, "fake-observed", ts).unwrap();
    s.wb_discovery_request(run, "fake-observed", ts).unwrap();
    assert_eq!(s.wb_snapshot().unwrap().list_calls_24h, 1);
    assert_eq!(s.wb_discovery().unwrap().unwrap().observed_requests, 1);
}

#[test]
fn next_page_keeps_pending_buffer_and_only_new_cursor_starts_new_age() {
    let s = Store::open_in_memory().unwrap();
    let original = register(&s, "FAKE");
    let id = history(&s, "FAKE", 1);
    s.wb_history_commit(
        id,
        &page(
            "FAKE",
            "fake-page2",
            vec![article("a", 800), article("b", 700)],
            false,
        ),
        &original.session_id,
        "fake-page2.dat",
        1100.,
    )
    .unwrap();
    s.wb_consume(id).unwrap();
    let before = s.wb_source("FAKE").unwrap().unwrap();
    let cursor = s.wb_cursor(before.history_cursor_id.unwrap()).unwrap();
    assert_eq!(cursor.source_page, Some(2));
    let id = s
        .wb_history_create(
            vec!["FAKE".into()],
            HistoryTarget {
                pages: Some(1),
                ..Default::default()
            },
        )
        .unwrap()[0];
    assert_eq!(s.wb_consume(id).unwrap().state, "running");
    assert_eq!(
        s.wb_source("FAKE").unwrap().unwrap().pending_count,
        before.pending_count
    );
    s.wb_cursor_result(cursor.id, "authentication_failed", 1199.)
        .unwrap();
    assert_eq!(s.wb_cursor(cursor.id).unwrap().observed_at, 1100.);
    s.wb_history_commit(
        id,
        &page("FAKE", "fake-page3", vec![article("c", 600)], false),
        &original.session_id,
        "fake-page3.dat",
        1200.,
    )
    .unwrap();
    let done = s.wb_consume(id).unwrap();
    assert_eq!(done.state, "done");
    assert_eq!(done.progress.pages, 1);
    assert_eq!(done.progress.received_articles, Some(1));
    let after = s.wb_source("FAKE").unwrap().unwrap();
    assert_eq!(after.pending_count, before.pending_count + 1);
    let latest = s.wb_cursor(after.history_cursor_id.unwrap()).unwrap();
    assert_eq!(latest.source_page, Some(3));
    assert_eq!(latest.observed_at, 1200.);
    assert_eq!(s.wb_cursor(cursor.id).unwrap().observed_at, 1100.);
}

#[test]
fn new_experiment_uses_latest_session_while_old_experiment_stays_fixed() {
    let s = Store::open_in_memory().unwrap();
    let original = register(&s, "FAKE");
    let cursor = original.history_cursor_id.unwrap();
    let old = s.wb_experiment_create(cursor, vec![1.], 1100.).unwrap();
    s.wb_capture(
        &page("FAKE", "new-home", vec![article("new", 2000)], false),
        &NativeConfig::default(),
        "fake-new-session.dat",
        "fake-home.dat",
        1200.,
    )
    .unwrap();
    let latest = s.wb_source("FAKE").unwrap().unwrap();
    let new = s.wb_experiment_create(cursor, vec![1.], 1200.).unwrap();
    assert_eq!(
        s.wb_experiment(old).unwrap().session_id,
        original.session_id
    );
    assert_eq!(s.wb_experiment(new).unwrap().session_id, latest.session_id);
    let control = s
        .wb_experiment_create_with_session(cursor, vec![1.], Some(&original.session_id), 1200.)
        .unwrap();
    assert_eq!(
        s.wb_experiment(control).unwrap().session_id,
        original.session_id
    );
    let other = register(&s, "OTHER");
    assert!(s
        .wb_experiment_create_with_session(cursor, vec![1.], Some(&other.session_id), 1200.)
        .is_err());
}

#[test]
fn continuous_and_one_shot_observers_cannot_attach_concurrently() {
    let s = Store::open_in_memory().unwrap();
    let mut config = NativeConfig {
        enabled: true,
        transport: "session".into(),
        window_title: "虚构公众号".into(),
        ..Default::default()
    };
    let run = s.wb_discovery_start(None).unwrap();
    assert!(s.native_create(&config).is_err());
    s.wb_discovery_state(run, "stopped", None).unwrap();
    config.max_pages = 1;
    s.native_create(&config).unwrap();
    assert!(s.wb_discovery_start(None).is_err());
}

#[test]
fn late_session_delivery_does_not_roll_back_latest_session_or_collection_time() {
    let s = Store::open_in_memory().unwrap();
    register(&s, "FAKE");
    let newer = page("FAKE", "newer", vec![article("newer", 2000)], false);
    s.wb_capture(
        &newer,
        &NativeConfig::default(),
        "fake-newer-session.dat",
        "fake-newer-home.dat",
        2000.,
    )
    .unwrap();
    let expected = s.wb_source("FAKE").unwrap().unwrap();
    let late = page("FAKE", "late", vec![article("late", 1500)], false);
    s.wb_observe_home(&late, 1500.).unwrap();
    s.wb_capture(
        &late,
        &NativeConfig::default(),
        "fake-late-session.dat",
        "fake-late-home.dat",
        1500.,
    )
    .unwrap();
    let actual = s.wb_source("FAKE").unwrap().unwrap();
    assert_eq!(actual.session_id, expected.session_id);
    assert_eq!(actual.home_cursor_id, expected.home_cursor_id);
    assert_eq!(actual.history_cursor_id, expected.history_cursor_id);
    assert_eq!(actual.updated_at, 2000.);
}

#[test]
fn refreshed_home_becomes_global_current_without_resetting_history_or_old_experiments() {
    let s = Store::open_in_memory().unwrap();
    let source = register(&s, "FAKE");
    let task = history(&s, "FAKE", 1);
    s.wb_history_commit(
        task,
        &page(
            "FAKE",
            "older-page",
            vec![article("a", 800), article("b", 700)],
            false,
        ),
        &source.session_id,
        "fake-older.dat",
        1100.,
    )
    .unwrap();
    s.wb_consume(task).unwrap();
    let before = s.wb_source("FAKE").unwrap().unwrap();
    assert_eq!(before.current_cursor_id, before.history_cursor_id);
    let experiment = s
        .wb_experiment_create(before.current_cursor_id.unwrap(), vec![1.], 1110.)
        .unwrap();
    s.wb_capture(
        &page("FAKE", "fresh-home", vec![article("new", 2000)], false),
        &NativeConfig::default(),
        "fresh-session.dat",
        "fresh-home.dat",
        1200.,
    )
    .unwrap();
    let after = s.wb_snapshot().unwrap().sources.remove(0);
    assert_eq!(after.current_cursor_id, after.home_cursor_id);
    assert_eq!(after.current_cursor_at, 1200.);
    assert_eq!(
        s.wb_cursor(after.current_cursor_id.unwrap())
            .unwrap()
            .observed_at,
        1200.
    );
    assert_eq!(after.history_cursor_id, before.history_cursor_id);
    assert_eq!(after.pending_count, before.pending_count);
    assert_eq!(
        s.wb_experiment(experiment).unwrap().cursor_id,
        before.current_cursor_id.unwrap()
    );
    s.wb_cursor_result(
        after.current_cursor_id.unwrap(),
        "authentication_failed",
        1300.,
    )
    .unwrap();
    assert_eq!(
        s.wb_source("FAKE").unwrap().unwrap().current_cursor_at,
        1200.
    );
    assert_eq!(
        s.wb_cursor(after.current_cursor_id.unwrap())
            .unwrap()
            .observed_at,
        1200.
    );
}

#[test]
fn manual_next_page_uses_global_current_and_preserves_diverged_history_buffer() {
    let s = Store::open_in_memory().unwrap();
    let source = register(&s, "FAKE");
    let task = history(&s, "FAKE", 1);
    s.wb_history_commit(
        task,
        &page(
            "FAKE",
            "old-history",
            vec![article("a", 800), article("b", 700)],
            false,
        ),
        &source.session_id,
        "fake-old-history.dat",
        1100.,
    )
    .unwrap();
    s.wb_consume(task).unwrap();
    let historical = s.wb_source("FAKE").unwrap().unwrap();
    s.wb_capture(
        &page("FAKE", "new-home", vec![article("new", 2000)], false),
        &NativeConfig::default(),
        "new-session.dat",
        "new-home.dat",
        1200.,
    )
    .unwrap();
    let current = s.wb_source("FAKE").unwrap().unwrap();
    let task = s
        .wb_history_create(
            vec!["FAKE".into()],
            HistoryTarget {
                pages: Some(1),
                ..Default::default()
            },
        )
        .unwrap()[0];
    assert_eq!(
        s.wb_task(task).unwrap().target["cursor_id"],
        current.current_cursor_id.unwrap()
    );
    s.wb_consume(task).unwrap();
    s.wb_history_commit(
        task,
        &page("FAKE", "new-page2", vec![article("page2", 1700)], false),
        &current.session_id,
        "new-page2.dat",
        1300.,
    )
    .unwrap();
    assert_eq!(s.wb_consume(task).unwrap().state, "done");
    let after = s.wb_source("FAKE").unwrap().unwrap();
    let cursor = s.wb_cursor(after.current_cursor_id.unwrap()).unwrap();
    assert_eq!(cursor.parent_id, current.current_cursor_id);
    assert_eq!(cursor.source_page, Some(2));
    assert_eq!(cursor.observed_at, 1300.);
    assert_eq!(after.history_cursor_id, historical.history_cursor_id);
    assert_eq!(after.pending_count, historical.pending_count);
    let resume = history(&s, "FAKE", 1);
    assert_eq!(s.wb_consume(resume).unwrap().progress.pages, 0);
    assert_eq!(s.wb_task(resume).unwrap().state, "done");
}

#[test]
fn global_current_end_and_late_responses_do_not_resurrect_old_cursor() {
    let s = Store::open_in_memory().unwrap();
    register(&s, "FAKE");
    s.wb_capture(
        &page("FAKE", "new-home", vec![article("new", 2000)], false),
        &NativeConfig::default(),
        "new-session.dat",
        "new-home.dat",
        1200.,
    )
    .unwrap();
    let current = s.wb_source("FAKE").unwrap().unwrap();
    let task = s
        .wb_history_create(
            vec!["FAKE".into()],
            HistoryTarget {
                pages: Some(1),
                ..Default::default()
            },
        )
        .unwrap()[0];
    s.wb_consume(task).unwrap();
    s.wb_history_commit(
        task,
        &page("FAKE", "", vec![article("end", 1700)], true),
        &current.session_id,
        "new-end.dat",
        1300.,
    )
    .unwrap();
    s.wb_consume(task).unwrap();
    let ended = s.wb_source("FAKE").unwrap().unwrap();
    assert!(ended.current_cursor_id.is_none());
    assert_eq!(ended.current_cursor_at, 1300.);
    assert!(s
        .wb_history_create(
            vec!["FAKE".into()],
            HistoryTarget {
                pages: Some(1),
                ..Default::default()
            }
        )
        .is_err());
    s.wb_capture(
        &page("FAKE", "late-home", vec![article("late", 1900)], false),
        &NativeConfig::default(),
        "late-session.dat",
        "late-home.dat",
        1250.,
    )
    .unwrap();
    assert!(s
        .wb_source("FAKE")
        .unwrap()
        .unwrap()
        .current_cursor_id
        .is_none());
    s.wb_capture(
        &page("FAKE", "fresh-home", vec![article("fresh", 2100)], false),
        &NativeConfig::default(),
        "fresh-session.dat",
        "fresh-home.dat",
        1400.,
    )
    .unwrap();
    let fresh = s.wb_source("FAKE").unwrap().unwrap();
    assert_eq!(fresh.current_cursor_id, fresh.home_cursor_id);
    assert_eq!(fresh.current_cursor_at, 1400.);
}

#[test]
fn current_home_can_fetch_next_page_after_historical_end() {
    let s = Store::open_in_memory().unwrap();
    let source = register(&s, "FAKE");
    let task = history(&s, "FAKE", 10);
    s.wb_history_commit(
        task,
        &page("FAKE", "", vec![article("end", 800)], true),
        &source.session_id,
        "fake-end.dat",
        1100.,
    )
    .unwrap();
    s.wb_consume(task).unwrap();
    assert!(s.wb_source("FAKE").unwrap().unwrap().ended);
    s.wb_capture(
        &page("FAKE", "fresh-home", vec![article("new", 2000)], false),
        &NativeConfig::default(),
        "fresh-session.dat",
        "fresh-home.dat",
        1200.,
    )
    .unwrap();
    let task = s
        .wb_history_create(
            vec!["FAKE".into()],
            HistoryTarget {
                pages: Some(1),
                ..Default::default()
            },
        )
        .unwrap()[0];
    assert_eq!(s.wb_consume(task).unwrap().state, "running");
    assert!(s.wb_task(task).unwrap().target["cursor_id"]
        .as_i64()
        .is_some());
}

#[test]
fn source_download_snapshots_all_articles_and_appends_only_new_items() {
    let store = Store::open_in_memory().unwrap();
    register(&store, "BODY-SOURCE");
    register(&store, "OTHER-SOURCE");
    for n in 0..75 {
        store
            .upsert_article(
                "BODY-SOURCE",
                &format!("extra-{n}"),
                1,
                &ArticleFields::default(),
            )
            .unwrap();
    }
    assert_eq!(store.wb_source_counts().unwrap()["BODY-SOURCE"], 77);
    let first = store.wb_download_source("BODY-SOURCE").unwrap();
    assert_eq!(first.queued, 77);
    let task = store.wb_task(first.task_id.unwrap()).unwrap();
    let ids: Vec<i64> = serde_json::from_value(task.target["article_ids"].clone()).unwrap();
    assert_eq!(ids.len(), 77);
    assert_eq!(task.biz.as_deref(), Some("BODY-SOURCE"));
    assert!(ids
        .iter()
        .all(|id| store.get_article_md(*id).unwrap().unwrap().0.biz == "BODY-SOURCE"));
    let repeated = store.wb_download_source("BODY-SOURCE").unwrap();
    assert_eq!(repeated.queued, 0);
    assert_eq!(repeated.already_queued, 77);
    assert!(repeated.task_id.is_none());
    store
        .upsert_article("BODY-SOURCE", "later", 1, &ArticleFields::default())
        .unwrap();
    let later = store.wb_download_source("BODY-SOURCE").unwrap();
    assert_eq!(later.queued, 1);
    assert_eq!(later.already_queued, 77);
    assert_ne!(first.task_id, later.task_id);
    assert_eq!(
        store.wb_task(first.task_id.unwrap()).unwrap().target["article_ids"]
            .as_array()
            .unwrap()
            .len(),
        77
    );
    assert!(store.wb_download_source("MISSING").is_err());
}

#[test]
fn download_queue_deduplicates_completed_and_concurrently_queued_articles() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    register(&store, "DOWNLOAD");
    let ready = store
        .upsert_article(
            "DOWNLOAD",
            "ready",
            1,
            &ArticleFields {
                content_md: Some("已有正文".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let ids: Vec<_> = store
        .list_articles(Some("DOWNLOAD"), -1, 0, false)
        .unwrap()
        .iter()
        .map(|a| a.id)
        .collect();
    let receipt = store.wb_download_enqueue(ids.clone()).unwrap();
    assert_eq!(receipt.requested, 3);
    assert_eq!(receipt.queued, 2);
    assert_eq!(receipt.already_downloaded, 1);
    assert_eq!(store.wb_download_enqueue(ids).unwrap().already_queued, 2);
    assert_eq!(
        store
            .wb_download_enqueue(vec![ready, ready])
            .unwrap()
            .requested,
        1
    );
    assert_eq!(
        store.wb_download_enqueue(vec![ready]).unwrap().task_id,
        None
    );
    let later = store
        .upsert_article(
            "DOWNLOAD",
            "concurrent",
            1,
            &ArticleFields {
                content_md: Some(" \n\t\r".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.wb_download_enqueue(vec![later]).unwrap()
            })
        })
        .collect();
    let receipts: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(receipts.iter().map(|r| r.queued).sum::<usize>(), 1);
    assert_eq!(receipts.iter().map(|r| r.already_queued).sum::<usize>(), 7);
    let before = store.wb_tasks().unwrap().len();
    assert!(store.wb_download_enqueue(vec![later, 999999]).is_err());
    assert_eq!(store.wb_tasks().unwrap().len(), before);
    let queue = store.wb_snapshot().unwrap().download_queue;
    assert_eq!(queue.task_count, 2);
    assert_eq!(queue.total, 3);
    assert_eq!(queue.completed, 0);
    assert!(store.wb_requests(0.).unwrap().is_empty());
}

#[test]
fn deleting_selected_articles_is_atomic_and_does_not_race_unfinished_downloads() {
    let store = Store::open_in_memory().unwrap();
    register(&store, "DELETE");
    let ids: Vec<_> = store
        .list_articles(Some("DELETE"), -1, 0, false)
        .unwrap()
        .iter()
        .map(|a| a.id)
        .collect();
    let task = store
        .wb_download_enqueue(vec![ids[0]])
        .unwrap()
        .task_id
        .unwrap();
    assert!(store.wb_delete_articles(ids.clone()).is_err());
    assert_eq!(store.wb_source_counts().unwrap()["DELETE"], 2);
    store.wb_task_state(task, "cancelled", None).unwrap();
    assert_eq!(store.wb_delete_articles(vec![ids[0], ids[0]]).unwrap(), 1);
    assert_eq!(store.wb_source_counts().unwrap()["DELETE"], 1);
    assert!(!store.wb_body_available(ids[0]).unwrap());
    assert!(store.wb_download_enqueue(vec![ids[0]]).is_err());
    assert_eq!(store.wb_download_source("DELETE").unwrap().queued, 1);
    let queue = store.wb_snapshot().unwrap().download_queue;
    assert_eq!(queue.total, 2);
    assert_eq!(queue.cancelled, 1);
    assert_eq!(queue.pending, 1);
}

#[test]
fn next_page_with_existing_articles_records_response_count_and_distinct_cursor_age() {
    let s = Store::open_in_memory().unwrap();
    let before = register(&s, "FAKE");
    let task = s
        .wb_history_create(
            vec!["FAKE".into()],
            HistoryTarget {
                pages: Some(1),
                ..Default::default()
            },
        )
        .unwrap()[0];
    s.wb_history_commit(
        task,
        &page(
            "FAKE",
            "distinct-next",
            vec![article("home-1", 1000), article("home-2", 900)],
            false,
        ),
        &before.session_id,
        "fake-next.dat",
        1200.,
    )
    .unwrap();
    let done = s.wb_consume(task).unwrap();
    assert_eq!(done.state, "done");
    assert_eq!(done.progress.received_articles, Some(2));
    assert_eq!(done.progress.new_articles, 0);
    assert_eq!(
        s.wb_cursor(before.current_cursor_id.unwrap())
            .unwrap()
            .observed_at,
        1000.
    );
    let after = s.wb_source("FAKE").unwrap().unwrap();
    assert_ne!(after.current_cursor_id, before.current_cursor_id);
    assert_eq!(after.current_cursor_at, 1200.);
    let logs = s
        .system_logs(
            &crate::store::system_logs::LogFilter {
                task_id: Some(task),
                ..Default::default()
            },
            None,
            100,
        )
        .unwrap();
    assert!(logs
        .rows
        .iter()
        .any(|r| r.context["outcome"] == "page_saved"
            && r.context["received_articles"] == 2
            && r.context["new_articles"] == 0));
    assert_eq!(
        serde_json::from_value::<TaskProgress>(json!({"completed":0,"pages":1}))
            .unwrap()
            .received_articles,
        None
    );
}

#[test]
fn session_timeout_after_success_keeps_at_least_valid_verdict() {
    let s = Store::open_in_memory().unwrap();
    let source = register(&s, "A");
    let cid = source.current_cursor_id.unwrap();
    let id = s.wb_experiment_create(cid, vec![1.], 1000.).unwrap();
    for (outcome, age, due) in [("valid", 60., 1000.), ("session_timeout", 3660., 4600.)] {
        s.wb_probe_add(Probe {
            id: 0,
            experiment_id: id,
            request_id: Some("fake".into()),
            role: "scheduled".into(),
            scheduled_at: due,
            actual_at: due,
            cursor_age_seconds: age,
            outcome: outcome.into(),
            evidence_path: None,
            detail: json!({}),
        })
        .unwrap();
    }
    let mut e = s.wb_experiment(id).unwrap();
    e.state = "done".into();
    e.next_index = 2;
    s.wb_save_experiment(&e).unwrap();
    let root = std::env::temp_dir().join(format!(
        "mpider-lab-report-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    engine::write_report(&s, &root, &mut e).unwrap();
    assert_eq!(e.verdict.as_deref(), Some("at_least_valid"));
    let conclusion = e.conclusion.as_deref().unwrap();
    assert!(conclusion.contains("至少可用"));
    assert!(conclusion.contains("停止"));
    let _ = std::fs::remove_dir_all(&root);
}
#[test]
fn immediate_experiment_and_live_phases_survive_stale_updates_and_cancel() {
    let s = Store::open_in_memory().unwrap();
    let source = register(&s, "A");
    let id = s
        .wb_experiment_create(source.current_cursor_id.unwrap(), vec![], 1000.)
        .unwrap();
    let mut stale = s.wb_experiment(id).unwrap();
    assert_eq!(stale.schedule_hours, vec![0.]);
    s.wb_experiment_phase(id, "control_home", Some(1100.))
        .unwrap();
    s.wb_save_experiment(&stale).unwrap();
    let active = s.wb_experiment(id).unwrap();
    assert_eq!(active.phase, "control_home");
    assert_eq!(active.next_action_at, Some(1100.));
    stale.state = "cancelled".into();
    s.wb_save_experiment(&stale).unwrap();
    s.wb_experiment_phase(id, "requesting", None).unwrap();
    stale.state = "running".into();
    s.wb_save_experiment(&stale).unwrap();
    let closed = s.wb_experiment(id).unwrap();
    assert_eq!(closed.state, "cancelled");
    assert_eq!(closed.phase, "cancelled");
    assert_eq!(closed.next_action_at, None);
}

#[test]
fn expired_session_requires_actual_recapture_before_resuming_original_cursor() {
    let s = Store::open_in_memory().unwrap();
    let original = register(&s, "A");
    let id = s
        .wb_history_create(
            vec!["A".into()],
            HistoryTarget {
                pages: Some(1),
                ..Default::default()
            },
        )
        .unwrap()[0];
    s.wb_task_block_session(id, &original.session_id, "session_timeout")
        .unwrap();
    let blocked = s.wb_task(id).unwrap();
    assert_eq!(blocked.state, "needs_action");
    assert_eq!(
        blocked.progress.blocked_session_id.as_deref(),
        Some(original.session_id.as_str())
    );
    assert!(blocked
        .reason
        .as_deref()
        .unwrap()
        .contains("更新会话并继续"));
    assert!(s
        .wb_task_state(id, "queued", None)
        .unwrap_err()
        .to_string()
        .contains("仍是已失效"));
    // 只观察到首页还不够，必须等到完整会话材料交付。
    let fresh_home = page("A", "new-home", vec![article("new-1", 1200)], false);
    s.wb_observe_home(&fresh_home, 2000.).unwrap();
    assert!(s.wb_task_state(id, "queued", None).is_err());
    s.wb_capture(
        &fresh_home,
        &NativeConfig::default(),
        "fresh-session.dat",
        "fresh-home.dat",
        2000.,
    )
    .unwrap();
    s.wb_task_state(id, "queued", None).unwrap();
    let resumed = s.wb_task(id).unwrap();
    assert_eq!(
        resumed.target["cursor_id"],
        original.current_cursor_id.unwrap()
    );
    assert_eq!(resumed.progress.pages, 0);
    let updated = s.wb_source("A").unwrap().unwrap();
    assert_eq!(updated.history_cursor_id, original.history_cursor_id);
    assert_ne!(updated.current_cursor_id, original.current_cursor_id);
    assert!(s.wb_requests(0.).unwrap().is_empty());
}

#[test]
fn late_session_failure_cannot_revive_cancelled_or_paused_task() {
    for state in ["paused", "cancelled"] {
        let s = Store::open_in_memory().unwrap();
        let original = register(&s, "A");
        let id = history(&s, "A", 20);
        s.wb_task_state(id, state, Some("用户操作")).unwrap();
        s.wb_task_block_session(id, &original.session_id, "session_timeout")
            .unwrap();
        assert_eq!(s.wb_task(id).unwrap().state, state);
    }
}

fn recovering(s: &Store) -> (i64, i64, Source) {
    let original = register(s, "RECOVERY_A");
    let task = s
        .wb_history_create(
            vec![original.biz.clone()],
            HistoryTarget {
                pages: Some(1),
                ..Default::default()
            },
        )
        .unwrap()[0];
    s.wb_task_block_session(task, &original.session_id, "session_timeout")
        .unwrap();
    let (run, created) = s.wb_recover_session(task, 1500.).unwrap();
    assert!(created);
    let run = run.unwrap();
    s.wb_discovery_state(run, "running", None).unwrap();
    (task, run, original)
}
#[test]
fn monitoring_is_idempotent_and_rejects_global_collection_scopes() {
    let s = Store::open_in_memory().unwrap();
    let (run, new) = s.wb_home_sync_start(None, false).unwrap();
    assert!(new);
    assert_eq!(s.wb_home_sync_start(None, false).unwrap(), (run, false));
    s.wb_discovery_state(run, "running", None).unwrap();
    assert!(s
        .wb_home_sync_start(
            Some(HistoryTarget {
                count: Some(5),
                ..Default::default()
            }),
            true
        )
        .is_err());
    assert!(s
        .wb_discovery_start(Some(HistoryTarget {
            count: Some(5),
            ..Default::default()
        }))
        .is_err());
    assert_eq!(s.wb_home_sync_start(None, false).unwrap(), (run, false));
    assert!(s.wb_discovery().unwrap().unwrap().target.is_none());
    s.wb_discovery_state(run, "stopping", None).unwrap();
    assert!(s.wb_home_sync_start(None, false).is_err());
}
#[test]
fn recovery_waits_for_correct_run_request_and_complete_session_then_preserves_fixed_cursor() {
    let s = Store::open_in_memory().unwrap();
    let (task, run, original) = recovering(&s);
    let fixed = s.wb_task(task).unwrap().target.clone();
    let before = s
        .wb_task(task)
        .unwrap()
        .progress
        .session_recovery
        .unwrap()
        .requested_at;
    assert_eq!(
        s.wb_recover_session(task, 1700.).unwrap(),
        (Some(run), false)
    );
    assert_eq!(
        s.wb_task(task)
            .unwrap()
            .progress
            .session_recovery
            .unwrap()
            .requested_at,
        before
    );
    let fresh = page(&original.biz, "fresh", vec![article("fresh", 2000)], false);
    s.wb_observe_home(&fresh, 2000.).unwrap();
    s.wb_recovery_home(run, &original.biz, 1600.).unwrap();
    assert_eq!(s.wb_task(task).unwrap().state, "needs_action");
    assert_eq!(
        s.wb_task(task)
            .unwrap()
            .progress
            .session_recovery
            .unwrap()
            .phase,
        "preparing_session"
    );
    // 迟到的旧请求即便响应时间较新，也不能恢复。
    s.wb_discovery_capture(
        run,
        1400.,
        &fresh,
        &NativeConfig::default(),
        "late-session.dat",
        "fake-home.dat",
        2000.,
    )
    .unwrap();
    assert_eq!(s.wb_task(task).unwrap().state, "needs_action");
    // 别的公众号的完整会话不会恢复本任务。
    s.wb_discovery_capture(
        run,
        1600.,
        &page("OTHER", "other", vec![article("other", 2000)], false),
        &NativeConfig::default(),
        "other.dat",
        "other-home.dat",
        2100.,
    )
    .unwrap();
    assert_eq!(s.wb_task(task).unwrap().state, "needs_action");
    s.wb_discovery_capture(
        run,
        1600.,
        &fresh,
        &NativeConfig::default(),
        "new-session.dat",
        "fake-home.dat",
        2200.,
    )
    .unwrap();
    let resumed = s.wb_task(task).unwrap();
    assert_eq!(resumed.state, "queued");
    assert_eq!(resumed.target, fixed);
    assert_eq!(
        s.wb_source(&original.biz)
            .unwrap()
            .unwrap()
            .history_cursor_id,
        original.history_cursor_id
    );
    assert_eq!(resumed.progress.pages, 0);
    assert_eq!(resumed.progress.session_recovery.unwrap().phase, "resumed");
}
#[test]
fn recovery_cancel_stop_restart_and_stale_capture_never_revive_tasks() {
    for action in ["pause", "cancel", "stop", "restart"] {
        let s = Store::open_in_memory().unwrap();
        let (task, run, source) = recovering(&s);
        match action {
            "pause" => s.wb_task_state(task, "paused", None).unwrap(),
            "cancel" => s.wb_task_state(task, "cancelled", None).unwrap(),
            "stop" => s.wb_discovery_state(run, "stopping", None).unwrap(),
            _ => s.wb_restart(1800.).unwrap(),
        }
        let before = s.wb_task(task).unwrap().state;
        let _ = s.wb_discovery_capture(
            run,
            1900.,
            &page(&source.biz, "fresh", vec![article("fresh", 2000)], false),
            &NativeConfig::default(),
            "new.dat",
            "fake-home.dat",
            2000.,
        );
        assert_eq!(s.wb_task(task).unwrap().state, before);
        assert_ne!(before, "queued");
    }
}
#[test]
fn recovery_uses_already_saved_new_session_without_starting_or_replacing_it() {
    let s = Store::open_in_memory().unwrap();
    let (task, run, source) = recovering(&s);
    s.wb_discovery_state(run, "stopped", None).unwrap();
    s.wb_capture(
        &page(&source.biz, "new", vec![article("new", 2000)], false),
        &NativeConfig::default(),
        "new.dat",
        "fake-home.dat",
        2000.,
    )
    .unwrap();
    assert_eq!(s.wb_recover_session(task, 2100.).unwrap(), (None, false));
    assert_eq!(s.wb_task(task).unwrap().state, "queued");
    assert_eq!(s.wb_discovery().unwrap().unwrap().state, "stopped");
}
#[test]
fn starting_recovery_sends_no_request_and_passive_home_is_counted_once() {
    let s = Store::open_in_memory().unwrap();
    let (_, run, source) = recovering(&s);
    assert!(s.wb_requests(0.).unwrap().is_empty());
    for _ in 0..2 {
        s.wb_discovery_request(run, "fake-user-home", crate::model::now())
            .unwrap();
    }
    s.wb_discovery_response("fake-user-home", &source.biz, "ok", crate::model::now())
        .unwrap();
    assert_eq!(s.wb_requests(0.).unwrap().len(), 1);
    assert_eq!(s.wb_snapshot().unwrap().list_calls_24h, 1);
    assert_eq!(s.wb_discovery().unwrap().unwrap().observed_requests, 1);
}

#[test]
fn late_ready_and_home_updates_cannot_undo_user_stop() {
    let s = Store::open_in_memory().unwrap();
    let (task, run, _) = recovering(&s);
    s.wb_discovery_state(run, "stopping", None).unwrap();
    s.wb_discovery_state(run, "running", None).unwrap();
    assert_eq!(s.wb_discovery().unwrap().unwrap().state, "stopping");
    assert!(s.wb_recovery_home(run, "RECOVERY_A", 9999.).is_err());
    assert_eq!(
        s.wb_task(task)
            .unwrap()
            .progress
            .session_recovery
            .unwrap()
            .phase,
        "interrupted"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn expired_list_tasks_wait_for_user_home_then_resume_through_real_worker_pipes() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;
    for kind in ["history", "home"] {
        let s = Arc::new(Store::open_in_memory().unwrap());
        zero_policy(&s);
        let original = register(&s, "FAKE_FLOW");
        let root = std::env::temp_dir().join(format!(
            "mpider-auto-recovery-{}",
            (crate::model::now() * 1000000.) as u64
        ));
        std::fs::create_dir_all(&root).unwrap();
        let fake = root.join("fake-python");
        let script = r##"#!/usr/bin/env python3
import sys,json,time,hashlib
from pathlib import Path

def body(cursor):
 return {'BaseResponse':{'Ret':0},'AccountInfo':{'UserName':'fake-FAKE_FLOW','NickName':'虚构公众号'},'MsgList':{'PagingInfo':{'Offset':cursor,'IsEnd':0},'Msg':[{'AppMsg':{'BaseInfo':{'CreateTime':1000},'DetailInfo':[{'Title':'虚构文章','ContentUrl':'https://mp.weixin.qq.com/s?__biz=FAKE_FLOW&mid=fake-flow&idx=1'}]}}]}}
def emit(event,**value):print(json.dumps(dict(event=event,**value)),flush=True)
if sys.argv[1].endswith('session_probe.py'):
 c=json.loads(sys.stdin.readline());Path(c['evidence_file']).write_text('fake-protected-evidence')
 result=body('page-after-resume') if 'old-session.dat' not in c['session_file'] else {'BaseResponse':{'Ret':-262,'ErrMsg':'H5 Session Timeout'}}
 print(json.dumps({'outcome':'response','request_cursor_hash':hashlib.sha256(b'current-before-expiry').hexdigest() if c.get('cursor_file') else None,'payload':{'transport':'mmtls','authenticated_outer_records':3,'inner_authenticated':True,'body':result}}))
else:
 import threading
 directory=Path(sys.argv[sys.argv.index('--directory')+1]);emit('ready');stop=threading.Event()
 def control():
  for line in sys.stdin:
   with (directory/'received-commands.jsonl').open('a') as f:f.write(line)
   if json.loads(line)['action']=='stop':break
  stop.set()
 threading.Thread(target=control,daemon=True).start()
 while not stop.is_set() and not (directory/'user-open-home').exists():time.sleep(.02)
 if not stop.is_set():
  request='watch-fake-home';ts=time.time();emit('observed_request',request_id=request,ts=ts)
  p={'base_resp':{'ret':0},'err_msg':'H5ExtTransfer:ok','jsapi_resp':{'ret':0,'resp_json':body('fresh-home')}}
  emit('home',request_id=request,user_name='fake-FAKE_FLOW',payload=p,ts=time.time(),request_ts=ts)
  sessions=directory/'sessions';sessions.mkdir(exist_ok=True)
  session=sessions/'new-session.dat';evidence=sessions/'fresh-home.dat';session.write_text('fake-session');evidence.write_text('fake-evidence')
  emit('session_ready',request_id=request,user_name='fake-FAKE_FLOW',payload=p,ts=time.time(),request_ts=ts,session_path=str(session),evidence_path=str(evidence))
 stop.wait()

"##;
        std::fs::write(&fake, script).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = NativeConfig {
            python_path: fake.display().to_string(),
            ..Default::default()
        };
        s.wb_capture(
            &page(
                &original.biz,
                "current-before-expiry",
                vec![article("fake-new", 2000)],
                false,
            ),
            &config,
            "old-session.dat",
            "fake-home.dat",
            1200.,
        )
        .unwrap();
        let fixed = s
            .wb_source(&original.biz)
            .unwrap()
            .unwrap()
            .current_cursor_id
            .unwrap();
        let task = if kind == "home" {
            s.wb_home_create(&original.biz).unwrap()
        } else {
            s.wb_history_create(
                vec![original.biz.clone()],
                HistoryTarget {
                    pages: Some(1),
                    ..Default::default()
                },
            )
            .unwrap()[0]
        };
        let runner = tokio::spawn(engine::run(s.clone(), root.clone()));
        let run = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(run) = s.wb_discovery().unwrap().filter(|r| r.state == "running") {
                    break run;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
        // 监测就绪后也不会主动导航或继续；只等待用户新打开的主页。
        tokio::time::sleep(Duration::from_millis(350)).await;
        let waiting = s.wb_task(task).unwrap();
        assert_eq!(waiting.state, "needs_action");
        assert_eq!(
            waiting.progress.session_recovery.unwrap().phase,
            "waiting_home"
        );
        assert_eq!(s.wb_requests(0.).unwrap().len(), 1);
        let directory = root.join("discovery").join(run.id.to_string());
        assert!(!directory.join("received-commands.jsonl").exists());
        // 仅测试替身用文件模拟用户打开主页；生产观察器没有该入口。
        std::fs::write(directory.join("user-open-home"), "").unwrap();
        let completed = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let t = s.wb_task(task).unwrap();
                if t.state == "done" {
                    break t;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await;
        let discovery = s.wb_discovery().unwrap();
        if let Some(run) = &discovery {
            s.wb_discovery_state(run.id, "stopping", None).unwrap();
        }
        runner.abort();
        if discovery.is_some() {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !s
                    .wb_discovery()
                    .unwrap()
                    .unwrap()
                    .phases
                    .iter()
                    .any(|p| p.phase == "worker_reaped")
                {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            })
            .await
            .unwrap();
        }
        let completed = completed.expect("自动恢复未完成");
        if kind == "history" {
            assert_eq!(completed.target["cursor_id"], fixed);
        } else {
            assert_eq!(completed.kind, "home");
        }
        assert_eq!(completed.progress.pages, 1);
        assert_eq!(
            s.wb_source(&original.biz)
                .unwrap()
                .unwrap()
                .history_cursor_id,
            original.history_cursor_id
        );
        let requests = s.wb_requests(0.).unwrap();
        if kind == "home" {
            assert!(requests
                .iter()
                .filter(|r| r.owner_kind == "task")
                .all(|r| r.purpose == "home_sync" && r.detail["cursor_id"].is_null()));
        }
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().any(|r| r.outcome == "session_timeout"));
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.purpose == "observed_home" && r.outcome == "ok")
                .count(),
            1
        );
        let commands = std::fs::read_to_string(directory.join("received-commands.jsonl")).unwrap();
        let actions: Vec<String> = commands
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["action"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(actions, vec!["stop"]);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn explicit_home_sync_preserves_history_buffer_other_sources_and_paused_tasks() {
    let s = Store::open_in_memory().unwrap();
    let original = register(&s, "SOURCE_A");
    register(&s, "SOURCE_B");
    let id = history(&s, "SOURCE_A", 1);
    s.wb_history_commit(
        id,
        &page(
            "SOURCE_A",
            "older-page",
            vec![article("old-a", 800), article("old-b", 700)],
            false,
        ),
        &original.session_id,
        "fake-history.dat",
        1100.,
    )
    .unwrap();
    s.wb_consume(id).unwrap();
    let before = s.wb_source("SOURCE_A").unwrap().unwrap();
    let other = serde_json::to_value(s.wb_source("SOURCE_B").unwrap()).unwrap();
    let home = s.wb_home_create("SOURCE_A").unwrap();
    assert_eq!(s.wb_home_create("SOURCE_A").unwrap(), home);
    let latest = page(
        "SOURCE_A",
        "latest-home",
        vec![article("latest-a", 3000)],
        false,
    );
    s.wb_home_commit(
        home,
        &latest,
        &original.session_id,
        "fake-latest.dat",
        2000.,
    )
    .unwrap();
    let after = s.wb_source("SOURCE_A").unwrap().unwrap();
    assert_eq!(after.history_cursor_id, before.history_cursor_id);
    assert_eq!(after.pending_count, 1);
    assert_eq!(after.boundary_keys, before.boundary_keys);
    assert_ne!(after.current_cursor_id, before.current_cursor_id);
    assert_eq!(
        serde_json::to_value(s.wb_source("SOURCE_B").unwrap()).unwrap(),
        other
    );
    assert_eq!(s.wb_task(home).unwrap().state, "done");
    assert_eq!(s.wb_task(home).unwrap().progress.new_articles, 1);
    let next = history(&s, "SOURCE_A", 1);
    assert_eq!(s.wb_consume(next).unwrap().progress.completed, 1);
    assert_eq!(s.wb_consume(next).unwrap().progress.pages, 0);
    for state in ["paused", "cancelled"] {
        let home = s.wb_home_create("SOURCE_A").unwrap();
        s.wb_task_state(home, state, None).unwrap();
        s.wb_home_commit(
            home,
            &page(
                "SOURCE_A",
                "late-home",
                vec![article("late-only", 4000)],
                false,
            ),
            &original.session_id,
            "fake-late.dat",
            3000.,
        )
        .unwrap();
        assert_eq!(s.wb_task(home).unwrap().state, state);
        assert_eq!(
            s.wb_source("SOURCE_A").unwrap().unwrap().article_count,
            after.article_count
        );
        if state == "paused" {
            s.wb_task_state(home, "cancelled", None).unwrap();
        }
    }
}

#[test]
fn home_sync_rejects_wrong_identity_and_legacy_sources_can_request_monitoring() {
    let s = Store::open_in_memory().unwrap();
    let source = register(&s, "SYNC_A");
    let other = register(&s, "SYNC_B");
    let id = s.wb_home_create(&source.biz).unwrap();
    assert!(s
        .wb_home_commit(
            id,
            &page(&other.biz, "wrong", vec![article("wrong", 3000)], false),
            &source.session_id,
            "fake.dat",
            2000.
        )
        .is_err());
    assert!(s
        .wb_home_commit(
            id,
            &page(&source.biz, "wrong", vec![article("wrong", 3000)], false),
            &other.session_id,
            "fake.dat",
            2000.
        )
        .is_err());
    assert_eq!(s.wb_task(id).unwrap().state, "queued");
    assert_eq!(
        s.wb_source(&source.biz).unwrap().unwrap().article_count,
        source.article_count
    );
    assert!(s.wb_home_create("MISSING").is_err());
    s.upsert_account("LEGACY_ROW", Some("旧公众号"), None)
        .unwrap();
    let legacy = s.wb_home_create("LEGACY_ROW").unwrap();
    assert!(s
        .wb_source("LEGACY_ROW")
        .unwrap()
        .unwrap()
        .session_id
        .is_empty());
    s.wb_task_state(legacy, "needs_action", Some("等待会话"))
        .unwrap();
    let (run, new) = s.wb_recover_session(legacy, 4000.).unwrap();
    assert!(run.is_some() && new);
    assert_eq!(
        s.wb_task(legacy)
            .unwrap()
            .progress
            .session_recovery
            .unwrap()
            .phase,
        "starting"
    );
    let history = history(&s, "LEGACY_ROW", 10);
    assert_eq!(
        s.wb_task(history).unwrap().biz.as_deref(),
        Some("LEGACY_ROW")
    );
}

#[test]
fn late_home_sync_from_old_session_imports_articles_without_replacing_new_monitor_cursor() {
    let s = Store::open_in_memory().unwrap();
    let old = register(&s, "LATE_HOME");
    let id = s.wb_home_create(&old.biz).unwrap();
    let (run, _) = s.wb_home_sync_start(None, false).unwrap();
    s.wb_discovery_state(run, "running", None).unwrap();
    s.wb_discovery_capture(
        run,
        2000.,
        &page(
            &old.biz,
            "new-monitored-home",
            vec![article("monitored-only", 3000)],
            false,
        ),
        &NativeConfig::default(),
        "new-session.dat",
        "fake-new-home.dat",
        2100.,
    )
    .unwrap();
    let fresh = s.wb_source(&old.biz).unwrap().unwrap();
    assert_eq!(fresh.article_count, old.article_count);
    s.wb_home_commit(
        id,
        &page(
            &old.biz,
            "old-late-page",
            vec![article("explicit-home", 2500)],
            false,
        ),
        &old.session_id,
        "fake-explicit-home.dat",
        2200.,
    )
    .unwrap();
    let after = s.wb_source(&old.biz).unwrap().unwrap();
    assert_eq!(after.current_cursor_id, fresh.current_cursor_id);
    assert_eq!(after.home_cursor_id, fresh.home_cursor_id);
    assert_eq!(after.session_id, fresh.session_id);
    assert_eq!(after.article_count, old.article_count + 1);
}
