//! Persistent interaction/comment jobs. Pausing invalidates the in-flight generation.
use super::{
    authenticated_comments::{self, Collection},
    comments, Task,
};
use crate::{model::now, Store};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Progress {
    pub epoch: u64,
    pub article_id: Option<i64>,
    pub title: String,
    pub pages: u32,
    pub reply_pages: u32,
    pub added: u32,
    pub updated: u32,
    pub unchanged: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Control {
    pub task_id: i64,
    pub epoch: u64,
    pub article_id: i64,
}
tokio::task_local! { static CURRENT: Control; }
pub(crate) fn current() -> Option<Control> {
    CURRENT.try_with(|c| *c).ok()
}
pub fn is_collection(kind: &str) -> bool {
    matches!(kind, "interactions" | "comments")
}

pub(super) fn page(
    store: &Store,
    replies: bool,
    added: u32,
    updated: u32,
    unchanged: u32,
) -> Result<()> {
    if let Some(control) = current() {
        store.wb_collection_page(control, replies, added, updated, unchanged)?;
    }
    Ok(())
}

async fn cancelled(store: &Store, control: Control) {
    loop {
        if !store.wb_collection_active(control).unwrap_or(false) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn execute(store: &Store, task: Task) -> Result<()> {
    let progress = task
        .progress
        .collection
        .as_ref()
        .expect("claimed collection");
    let control = Control {
        task_id: task.id,
        epoch: progress.epoch,
        article_id: progress.article_id.expect("claimed article"),
    };
    let collection = if task.kind == "interactions" {
        Collection::Interactions
    } else {
        Collection::Comments
    };
    CURRENT.scope(control, async {
        let result = tokio::select! {
            biased;
            _ = cancelled(store, control) => return Ok(()),
            result = authenticated_comments::collect(store, control.article_id, collection) => result,
        };
        store.wb_collection_finish(control, result.err().map(|error| format!("{error:#}")))
    }).await
}

pub async fn run(store: Arc<Store>) {
    loop {
        // One worker shares the article request interval across both task types.
        if store.wb_policy().is_ok_and(|p| p.cooldown_until <= now()) {
            match store.wb_collection_claim() {
                Ok(Some(task)) => {
                    let _ = execute(&store, task).await;
                }
                Ok(None) => {}
                Err(_) => crate::applog::warn(
                    crate::applog::Stage::List,
                    "互动和评论任务无法读取，请检查存储",
                ),
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// Cached replies are merged before requesting missing replies; wire snapshots remain comparable.
pub(super) fn merge_cached(
    row: &mut comments::ArticleComment,
    cached: Option<&comments::ArticleComment>,
) {
    if let Some(cached) = cached {
        let fresh = std::mem::take(&mut row.replies);
        row.replies = cached.replies.clone();
        comments::merge_comments(&mut row.replies, fresh);
    }
}

#[cfg(test)]
mod tests;
