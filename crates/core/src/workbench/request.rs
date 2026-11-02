//! HTTP future 被取消或提前返回时也结束账本，避免永久占用文章请求工作位。
use crate::{model::now, Store};
use anyhow::Result;
use serde_json::{json, Value};

pub(super) struct Completion<'a> {
    store: &'a Store,
    id: String,
    finished: bool,
}

impl<'a> Completion<'a> {
    pub fn new(store: &'a Store, id: &str) -> Self {
        Self {
            store,
            id: id.into(),
            finished: false,
        }
    }
    pub fn finish(&mut self, outcome: &str, detail: Value, ts: f64) -> Result<()> {
        self.store
            .wb_request_finish(&self.id, outcome, detail, ts)?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for Completion<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.store.wb_request_finish(
                &self.id,
                "interrupted",
                json!({"reason":"HTTP执行已中断"}),
                now(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_http_execution_releases_slot_but_preserves_post_response_spacing() {
        let store = Store::open_in_memory().unwrap();
        store
            .wb_set_policy(crate::workbench::Policy {
                body_workers: 1,
                body_gap_ms: 10000,
                ..Default::default()
            })
            .unwrap();
        let id = store
            .wb_request_permit("body", "archive", 1, "FAKE", now())
            .unwrap()
            .request_id
            .unwrap();
        {
            let _completion = Completion::new(&store, &id);
        }
        let event = store.wb_requests(0.).unwrap().pop().unwrap();
        assert_eq!(event.outcome, "interrupted");
        let end = event.finished_at.unwrap();
        assert!(store
            .wb_request_permit("body", "archive", 1, "FAKE", end + 9.)
            .unwrap()
            .request_id
            .is_none());
        assert!(store
            .wb_request_permit("body", "archive", 1, "FAKE", end + 10.)
            .unwrap()
            .request_id
            .is_some());
    }
}
