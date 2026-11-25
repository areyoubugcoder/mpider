use super::*;
use crate::workbench::interactions::ArticleInteractions;

fn seed(store: &Store, mid: &str) -> i64 {
    store
        .upsert_article("B", mid, 1, &Default::default())
        .unwrap()
}
#[test]
fn batches_skip_complete_data_deduplicate_and_allow_explicit_refresh() {
    let store = Store::open_in_memory().unwrap();
    let a = seed(&store, "1");
    let b = seed(&store, "2");
    store
        .save_article_interactions(
            a,
            &ArticleInteractions {
                read_count: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
    let receipt = store
        .wb_collection_enqueue("interactions", Some("B".into()), vec![], false)
        .unwrap();
    assert_eq!(
        (receipt.requested, receipt.queued, receipt.skipped),
        (2, 1, 1)
    );
    assert_eq!(
        store.wb_task(receipt.task_id.unwrap()).unwrap().target["article_ids"],
        serde_json::json!([b])
    );
    let receipt = store
        .wb_collection_enqueue("interactions", None, vec![a, b], true)
        .unwrap();
    assert_eq!((receipt.queued, receipt.already_queued), (1, 1));
    store
        .save_article_comment_batch(a, &Default::default(), &[], Some((0, -1)))
        .unwrap();
    let receipt = store
        .wb_collection_enqueue("comments", Some("B".into()), vec![], false)
        .unwrap();
    assert_eq!((receipt.queued, receipt.skipped), (1, 1));
    assert_eq!(store.wb_task_page("comments", 10, 0).unwrap().total, 1);
}

#[tokio::test]
async fn pause_cancel_and_fast_resume_reject_late_writes_and_new_requests() {
    let store = Store::open_in_memory().unwrap();
    let id = seed(&store, "1");
    let task_id = store
        .wb_collection_enqueue("comments", None, vec![id], true)
        .unwrap()
        .task_id
        .unwrap();
    let task = store.wb_collection_claim().unwrap().unwrap();
    let control = Control {
        task_id,
        article_id: id,
        epoch: task.progress.collection.unwrap().epoch,
    };
    CURRENT
        .scope(control, async {
            page(&store, false, 1, 0, 0).unwrap();
            page(&store, false, 0, 1, 2).unwrap();
            store.wb_task_state(task_id, "paused", None).unwrap();
            assert!(store
                .save_article_interactions(
                    id,
                    &ArticleInteractions {
                        read_count: Some(9),
                        ..Default::default()
                    }
                )
                .is_err());
            assert!(store
                .wb_request_permit("interaction", "article_comments", id, "B", now())
                .is_err());
            store.wb_task_state(task_id, "queued", None).unwrap();
            assert!(!store.wb_collection_active(control).unwrap());
            store.wb_collection_finish(control, None).unwrap();
            assert_eq!(store.wb_task(task_id).unwrap().progress.completed, 0);
            store.wb_task_state(task_id, "cancelled", None).unwrap();
            assert!(store.wb_collection_claim().unwrap().is_none());
            assert_eq!(
                store
                    .wb_task(task_id)
                    .unwrap()
                    .progress
                    .collection
                    .unwrap()
                    .pages,
                2
            );
        })
        .await;
}

#[tokio::test]
async fn cancellation_drops_in_flight_work_without_waiting_for_response() {
    let store = Store::open_in_memory().unwrap();
    let id = seed(&store, "1");
    let task_id = store
        .wb_collection_enqueue("comments", None, vec![id], true)
        .unwrap()
        .task_id
        .unwrap();
    let task = store.wb_collection_claim().unwrap().unwrap();
    let control = Control {
        task_id,
        article_id: id,
        epoch: task.progress.collection.unwrap().epoch,
    };
    store.wb_task_state(task_id, "cancelled", None).unwrap();
    tokio::time::timeout(Duration::from_millis(250), cancelled(&store, control))
        .await
        .unwrap();
}

#[test]
fn merging_cached_replies_preserves_history_and_updates_changed_reply_content() {
    let parse = |v| comments::parse_comment(&v, 0).unwrap();
    let old = parse(
        serde_json::json!({"content_id":"a","content":"root","reply_new":{"reply_total_cnt":2,"max_reply_id":"2","reply_list":[{"reply_id":"1","content":"old"},{"reply_id":"2","content":"kept"}]}}),
    );
    let mut fresh = parse(
        serde_json::json!({"content_id":"a","content":"root","reply_new":{"reply_total_cnt":3,"max_reply_id":"3","reply_list":[{"reply_id":"1","content":"edited"},{"reply_id":"3","content":"new"}]}}),
    );
    merge_cached(&mut fresh, Some(&old));
    assert_eq!(fresh.replies.len(), 3);
    assert_eq!(fresh.replies[0].content, "edited");
    assert_eq!(fresh.replies[1].content, "kept");
    assert_eq!(fresh.reply_total, Some(3));
}

#[test]
fn failed_article_can_resume_without_repeating_finished_articles() {
    let store = Store::open_in_memory().unwrap();
    let a = seed(&store, "1");
    let b = seed(&store, "2");
    let id = store
        .wb_collection_enqueue("interactions", None, vec![a, b], true)
        .unwrap()
        .task_id
        .unwrap();
    let claim = || {
        let task = store.wb_collection_claim().unwrap().unwrap();
        let progress = task.progress.collection.unwrap();
        Control {
            task_id: task.id,
            article_id: progress.article_id.unwrap(),
            epoch: progress.epoch,
        }
    };
    let first = claim();
    assert_eq!(first.article_id, a);
    store.wb_collection_finish(first, None).unwrap();
    let second = claim();
    assert_eq!(second.article_id, b);
    store
        .wb_collection_finish(second, Some("请打开公众号文章后重试".into()))
        .unwrap();
    assert_eq!(store.wb_task(id).unwrap().state, "needs_action");
    assert!(store.wb_collection_claim().unwrap().is_none());
    store.wb_task_state(id, "queued", None).unwrap();
    let resumed = claim();
    assert_eq!(resumed.article_id, b);
    store.wb_collection_finish(resumed, None).unwrap();
    let task = store.wb_task(id).unwrap();
    assert_eq!(task.state, "done");
    assert_eq!(task.progress.finished_ids, vec![a, b]);
    assert_eq!(
        store
            .wb_article_collection_task(b, "interactions")
            .unwrap()
            .unwrap()
            .id,
        id
    );
}

#[test]
fn article_dialog_uses_latest_refresh_after_older_batch_finished_that_article() {
    let store = Store::open_in_memory().unwrap();
    let a = seed(&store, "1");
    let b = seed(&store, "2");
    let first = store
        .wb_collection_enqueue("interactions", None, vec![a, b], true)
        .unwrap()
        .task_id
        .unwrap();
    let task = store.wb_collection_claim().unwrap().unwrap();
    store
        .wb_collection_finish(
            Control {
                task_id: first,
                article_id: a,
                epoch: task.progress.collection.unwrap().epoch,
            },
            None,
        )
        .unwrap();
    store.wb_task_state(first, "paused", None).unwrap();
    let latest = store
        .wb_collection_enqueue("interactions", None, vec![a], true)
        .unwrap()
        .task_id
        .unwrap();
    let task = store.wb_collection_claim().unwrap().unwrap();
    store
        .wb_collection_finish(
            Control {
                task_id: latest,
                article_id: a,
                epoch: task.progress.collection.unwrap().epoch,
            },
            None,
        )
        .unwrap();
    assert_eq!(
        store
            .wb_article_collection_task(a, "interactions")
            .unwrap()
            .unwrap()
            .id,
        latest
    );
    assert_eq!(
        store
            .wb_article_collection_task(b, "interactions")
            .unwrap()
            .unwrap()
            .id,
        first
    );
}
